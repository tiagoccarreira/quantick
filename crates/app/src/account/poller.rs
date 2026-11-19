//! MCP poller thread: positions, account info, and trade execution.
//!
//! Spawned from [`super::spawn`]. Network stays here; the UI only posts
//! [`super::TradeCommand`] values and drains [`super::TradeNotice`] toasts.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use quantick_engine::Side;
use rust_decimal::Decimal;
use rust_decimal::prelude::FromPrimitive;
use serde_json::{Map, Value};
use tokio::sync::watch;

use super::venue;
use super::{
    AccountInfo, AccountSnapshot, HTTP_TIMEOUT, MCP_PROTOCOL, OpenTicket, POLL_MS, PendingKind,
    PendingOrder, PositionSnapshot, TradeCommand, TradeNotice, parse_account_info, price_lots,
    volume_lots,
};

pub(super) async fn run_poller(
    url: String,
    token: String,
    mut symbol_rx: watch::Receiver<String>,
    snap_tx: watch::Sender<Option<AccountSnapshot>>,
    cmd_rx: std::sync::mpsc::Receiver<TradeCommand>,
    notice_tx: std::sync::mpsc::Sender<TradeNotice>,
    stop: Arc<AtomicBool>,
) {
    let client = match reqwest::Client::builder().timeout(HTTP_TIMEOUT).build() {
        Ok(client) => client,
        Err(error) => {
            tracing::warn!(
                target: "quantick::app",
                schema_version = 1_u8,
                event_code = "MT5_MCP_CLIENT",
                error = %error,
                "account poller could not build an HTTP client"
            );
            return;
        }
    };
    let mut session = String::new();
    let mut rpc_id: u64 = 1;
    while !stop.load(Ordering::Relaxed) {
        if session.is_empty() {
            match initialize(&client, &url, &token).await {
                Ok(id) => {
                    tracing::info!(
                        target: "quantick::app",
                        schema_version = 1_u8,
                        event_code = "MT5_MCP_SESSION",
                        action = "initialized",
                        "account MCP session started"
                    );
                    session = id;
                }
                Err(error) => {
                    tracing::warn!(
                        target: "quantick::app",
                        schema_version = 1_u8,
                        event_code = "MT5_MCP_INIT_FAILED",
                        error = %error,
                        "account MCP initialize failed"
                    );
                    let _ = snap_tx.send(None);
                }
            }
        }
        if !session.is_empty() {
            let symbol = symbol_rx.borrow().clone();
            match poll_account(&client, &url, &token, &session, &mut rpc_id, &symbol).await {
                Ok(snapshot) => {
                    let _ = snap_tx.send(Some(snapshot));
                }
                Err(error) => {
                    tracing::warn!(
                        target: "quantick::app",
                        schema_version = 1_u8,
                        event_code = "MT5_MCP_POLL_FAILED",
                        error = %error,
                        "account MCP poll failed"
                    );
                    let _ = snap_tx.send(None);
                    session.clear();
                }
            }
            while !session.is_empty()
                && let Ok(command) = cmd_rx.try_recv()
            {
                if let Some(notice) =
                    execute_trade(&client, &url, &token, &session, &mut rpc_id, command).await
                {
                    let _ = notice_tx.send(notice);
                }
            }
        }
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(POLL_MS)) => {}
            changed = symbol_rx.changed() => {
                if changed.is_err() {
                    break;
                }
            }
        }
    }
}

async fn initialize(client: &reqwest::Client, url: &str, token: &str) -> Result<String, String> {
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": MCP_PROTOCOL,
            "capabilities": {},
            "clientInfo": { "name": "quantick", "version": env!("CARGO_PKG_VERSION") }
        }
    });
    let response = post(client, url, token, None, &body).await?;
    let session = response
        .headers()
        .get("mcp-session-id")
        .or_else(|| response.headers().get("Mcp-Session-Id"))
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_owned();
    let _ = read_rpc_body(response).await?;
    if session.is_empty() {
        return Err("MCP initialize returned no Mcp-Session-Id".into());
    }
    let notify = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized",
        "params": {}
    });
    let _ = post(client, url, token, Some(&session), &notify).await?;
    Ok(session)
}

async fn poll_positions(
    client: &reqwest::Client,
    url: &str,
    token: &str,
    session: &str,
    rpc_id: &mut u64,
    symbol: &str,
) -> Result<PositionSnapshot, String> {
    *rpc_id = rpc_id.saturating_add(1);
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": *rpc_id,
        "method": "tools/call",
        "params": {
            "name": "get_trading_open_positions",
            "arguments": { "symbol": symbol, "include_orders": true }
        }
    });
    let response = post(client, url, token, Some(session), &body).await?;
    let value = read_rpc_body(response).await?;
    if value
        .get("isError")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Err("get_trading_open_positions returned isError".into());
    }
    Ok(parse_positions(&value, symbol))
}

async fn poll_account(
    client: &reqwest::Client,
    url: &str,
    token: &str,
    session: &str,
    rpc_id: &mut u64,
    symbol: &str,
) -> Result<AccountSnapshot, String> {
    let positions = poll_positions(client, url, token, session, rpc_id, symbol).await?;
    let info = poll_info(client, url, token, session, rpc_id)
        .await
        .unwrap_or(None);
    Ok(AccountSnapshot { positions, info })
}

async fn poll_info(
    client: &reqwest::Client,
    url: &str,
    token: &str,
    session: &str,
    rpc_id: &mut u64,
) -> Result<Option<AccountInfo>, String> {
    let value = tools_call(
        client,
        url,
        token,
        session,
        rpc_id,
        "get_trading_account_info",
        serde_json::json!({}),
    )
    .await?;
    Ok(parse_account_info(&value))
}

#[allow(clippy::too_many_arguments)]
async fn execute_trade(
    client: &reqwest::Client,
    url: &str,
    token: &str,
    session: &str,
    rpc_id: &mut u64,
    command: TradeCommand,
) -> Option<TradeNotice> {
    match command {
        TradeCommand::PlaceMarket {
            side,
            volume,
            symbol,
            stop_loss,
            take_profit,
        } => Some(
            place_market(
                client,
                url,
                token,
                session,
                rpc_id,
                side,
                volume,
                symbol,
                stop_loss,
                take_profit,
            )
            .await,
        ),
        TradeCommand::PlacePending {
            side,
            kind,
            volume,
            price,
            symbol,
            stop_loss,
            take_profit,
        } => Some(
            place_pending(
                client,
                url,
                token,
                session,
                rpc_id,
                side,
                kind,
                volume,
                price,
                symbol,
                stop_loss,
                take_profit,
            )
            .await,
        ),
        TradeCommand::ModifySlTp {
            position_ticket,
            symbol,
            stop_loss,
            take_profit,
        } => Some(
            modify_sl_tp(
                client,
                url,
                token,
                session,
                rpc_id,
                position_ticket,
                symbol,
                stop_loss,
                take_profit,
            )
            .await,
        ),
        TradeCommand::DeleteOrder {
            order_ticket,
            symbol,
        } => Some(delete_order(client, url, token, session, rpc_id, order_ticket, symbol).await),
        TradeCommand::Close { tickets, symbol } => {
            if tickets.is_empty() {
                return None;
            }
            Some(close_tickets(client, url, token, session, rpc_id, tickets, symbol).await)
        }
        TradeCommand::Reverse {
            tickets,
            symbol,
            side,
            volume,
            stop_loss,
            take_profit,
        } => {
            if tickets.is_empty() {
                return None;
            }
            Some(
                reverse_position(
                    client,
                    url,
                    token,
                    session,
                    rpc_id,
                    tickets,
                    symbol,
                    side,
                    volume,
                    stop_loss,
                    take_profit,
                )
                .await,
            )
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn place_market(
    client: &reqwest::Client,
    url: &str,
    token: &str,
    session: &str,
    rpc_id: &mut u64,
    side: Side,
    volume: Decimal,
    symbol: String,
    stop_loss: Option<Decimal>,
    take_profit: Option<Decimal>,
) -> TradeNotice {
    let Some(lots) = volume_lots(volume) else {
        return TradeNotice::Refused("volume inválido".into());
    };
    let side_word = match side {
        Side::Buy => "buy",
        Side::Sell => "sell",
    };
    let mut arguments = serde_json::json!({
        "symbol": symbol,
        "type": side_word,
        "volume": lots,
    });
    with_protection(&mut arguments, stop_loss, take_profit);
    tracing::info!(
        target: "quantick::app",
        schema_version = 1_u8,
        event_code = "MT5_TRADE_CHECK",
        symbol = %symbol,
        side = side_word,
        volume = lots,
        "checking a market order"
    );
    match tools_call(
        client,
        url,
        token,
        session,
        rpc_id,
        "trade_check_market_order",
        arguments.clone(),
    )
    .await
    {
        Ok(value) => {
            if let Some(reason) = venue::tool_refusal(&value) {
                tracing::warn!(
                    target: "quantick::app",
                    schema_version = 1_u8,
                    event_code = "MT5_TRADE_REFUSED",
                    reason = %reason,
                    "market check refused"
                );
                return TradeNotice::Refused(reason);
            }
        }
        Err(reason) => {
            tracing::warn!(
                target: "quantick::app",
                schema_version = 1_u8,
                event_code = "MT5_TRADE_REFUSED",
                reason = %reason,
                "market check failed"
            );
            return TradeNotice::Refused(reason);
        }
    }
    tracing::info!(
        target: "quantick::app",
        schema_version = 1_u8,
        event_code = "MT5_TRADE_SEND",
        symbol = %symbol,
        side = side_word,
        volume = lots,
        "sending a market order"
    );
    match tools_call(
        client,
        url,
        token,
        session,
        rpc_id,
        "trade_send_market_order",
        arguments,
    )
    .await
    {
        Ok(value) => {
            if let Some(reason) = venue::tool_refusal(&value) {
                tracing::warn!(
                    target: "quantick::app",
                    schema_version = 1_u8,
                    event_code = "MT5_TRADE_REFUSED",
                    reason = %reason,
                    "market send refused"
                );
                TradeNotice::Refused(reason)
            } else {
                TradeNotice::Placed { side, volume }
            }
        }
        Err(reason) => {
            tracing::warn!(
                target: "quantick::app",
                schema_version = 1_u8,
                event_code = "MT5_TRADE_REFUSED",
                reason = %reason,
                "market send failed"
            );
            TradeNotice::Refused(reason)
        }
    }
}

fn with_protection(
    arguments: &mut Value,
    stop_loss: Option<Decimal>,
    take_profit: Option<Decimal>,
) {
    let Some(map) = arguments.as_object_mut() else {
        return;
    };
    if let Some(sl) = stop_loss.and_then(price_lots) {
        map.insert("sl".into(), serde_json::json!(sl));
    }
    if let Some(tp) = take_profit.and_then(price_lots) {
        map.insert("tp".into(), serde_json::json!(tp));
    }
}

#[allow(clippy::too_many_arguments)]
async fn place_pending(
    client: &reqwest::Client,
    url: &str,
    token: &str,
    session: &str,
    rpc_id: &mut u64,
    side: Side,
    kind: PendingKind,
    volume: Decimal,
    price: Decimal,
    symbol: String,
    stop_loss: Option<Decimal>,
    take_profit: Option<Decimal>,
) -> TradeNotice {
    let Some(lots) = volume_lots(volume) else {
        return TradeNotice::Refused("volume inválido".into());
    };
    let Some(price_f) = price_lots(price) else {
        return TradeNotice::Refused("price inválido".into());
    };
    let order_type = kind.mcp_type(side);
    // B3 (WIN/WDO) refuses GTC/unspecified pending as retcode 10022 Invalid
    // expiration — same default the local bridge sends (`ORDER_TIME_DAY`).
    let mut arguments = serde_json::json!({
        "symbol": symbol,
        "type": order_type,
        "volume": lots,
        "price": price_f,
        "type_time": "ORDER_TIME_DAY",
        "expiration": 0,
    });
    with_protection(&mut arguments, stop_loss, take_profit);
    tracing::info!(
        target: "quantick::app",
        schema_version = 1_u8,
        event_code = "MT5_TRADE_CHECK",
        symbol = %symbol,
        order_type,
        volume = lots,
        price = price_f,
        "checking a pending order"
    );
    match tools_call(
        client,
        url,
        token,
        session,
        rpc_id,
        "trade_check_pending_order",
        arguments.clone(),
    )
    .await
    {
        Ok(value) => {
            if let Some(reason) = venue::tool_refusal(&value) {
                return TradeNotice::Refused(reason);
            }
        }
        Err(reason) => return TradeNotice::Refused(reason),
    }
    tracing::info!(
        target: "quantick::app",
        schema_version = 1_u8,
        event_code = "MT5_TRADE_SEND",
        symbol = %symbol,
        order_type,
        volume = lots,
        price = price_f,
        "sending a pending order"
    );
    match tools_call(
        client,
        url,
        token,
        session,
        rpc_id,
        "trade_send_pending_order",
        arguments,
    )
    .await
    {
        Ok(value) => {
            if let Some(reason) = venue::tool_refusal(&value) {
                TradeNotice::Refused(reason)
            } else {
                TradeNotice::PendingPlaced { side, kind, price }
            }
        }
        Err(reason) => TradeNotice::Refused(reason),
    }
}

#[allow(clippy::too_many_arguments)]
async fn modify_sl_tp(
    client: &reqwest::Client,
    url: &str,
    token: &str,
    session: &str,
    rpc_id: &mut u64,
    position_ticket: u64,
    symbol: String,
    stop_loss: Option<Decimal>,
    take_profit: Option<Decimal>,
) -> TradeNotice {
    let mut arguments = serde_json::json!({
        "symbol": symbol,
        "position_ticket": position_ticket.to_string(),
        "sl": stop_loss.and_then(price_lots).unwrap_or(0.0),
        "tp": take_profit.and_then(price_lots).unwrap_or(0.0),
    });
    // Always send both so empty clears; MCP uses 0 to remove.
    let _ = &mut arguments;
    tracing::info!(
        target: "quantick::app",
        schema_version = 1_u8,
        event_code = "MT5_TRADE_MODIFY",
        symbol = %symbol,
        ticket = position_ticket,
        "modifying SL/TP"
    );
    match tools_call(
        client,
        url,
        token,
        session,
        rpc_id,
        "trade_modify_sl_tp",
        arguments,
    )
    .await
    {
        Ok(value) => {
            if let Some(reason) = venue::tool_refusal(&value) {
                TradeNotice::Refused(reason)
            } else {
                TradeNotice::BracketSet
            }
        }
        Err(reason) => TradeNotice::Refused(reason),
    }
}

async fn delete_order(
    client: &reqwest::Client,
    url: &str,
    token: &str,
    session: &str,
    rpc_id: &mut u64,
    order_ticket: u64,
    symbol: String,
) -> TradeNotice {
    let arguments = serde_json::json!({
        "symbol": symbol,
        "order_ticket": order_ticket.to_string(),
    });
    tracing::info!(
        target: "quantick::app",
        schema_version = 1_u8,
        event_code = "MT5_TRADE_DELETE",
        symbol = %symbol,
        ticket = order_ticket,
        "deleting a pending order"
    );
    match tools_call(
        client,
        url,
        token,
        session,
        rpc_id,
        "trade_delete_order",
        arguments,
    )
    .await
    {
        Ok(value) => {
            if let Some(reason) = venue::tool_refusal(&value) {
                TradeNotice::Refused(reason)
            } else {
                TradeNotice::OrderDeleted
            }
        }
        Err(reason) => TradeNotice::Refused(reason),
    }
}

#[allow(clippy::too_many_arguments)]
async fn close_tickets(
    client: &reqwest::Client,
    url: &str,
    token: &str,
    session: &str,
    rpc_id: &mut u64,
    tickets: Vec<u64>,
    symbol: String,
) -> TradeNotice {
    if tickets.is_empty() {
        return TradeNotice::Refused("sem posição".into());
    }
    let mut closed = 0_usize;
    let mut last_error = None;
    for ticket in tickets {
        tracing::info!(
            target: "quantick::app",
            schema_version = 1_u8,
            event_code = "MT5_TRADE_CLOSE",
            symbol = %symbol,
            ticket,
            "closing a position"
        );
        let arguments = serde_json::json!({
            "symbol": symbol,
            "position_ticket": ticket.to_string(),
        });
        match tools_call(
            client,
            url,
            token,
            session,
            rpc_id,
            "trade_close_single_position",
            arguments,
        )
        .await
        {
            Ok(value) => {
                if let Some(reason) = venue::tool_refusal(&value) {
                    last_error = Some(reason);
                } else {
                    closed += 1;
                }
            }
            Err(reason) => last_error = Some(reason),
        }
    }
    if closed == 0 {
        TradeNotice::Refused(last_error.unwrap_or_else(|| "did not close".into()))
    } else {
        TradeNotice::Closed { count: closed }
    }
}

#[allow(clippy::too_many_arguments)]
async fn reverse_position(
    client: &reqwest::Client,
    url: &str,
    token: &str,
    session: &str,
    rpc_id: &mut u64,
    tickets: Vec<u64>,
    symbol: String,
    side: Side,
    volume: Decimal,
    stop_loss: Option<Decimal>,
    take_profit: Option<Decimal>,
) -> TradeNotice {
    match close_tickets(client, url, token, session, rpc_id, tickets, symbol.clone()).await {
        TradeNotice::Closed { .. } => {}
        refused @ TradeNotice::Refused(_) => return refused,
        other => {
            return TradeNotice::Refused(format!("reverse abortou no close ({})", other.toast()));
        }
    }
    match place_market(
        client,
        url,
        token,
        session,
        rpc_id,
        side,
        volume,
        symbol,
        stop_loss,
        take_profit,
    )
    .await
    {
        TradeNotice::Placed { side, volume } => TradeNotice::Reversed { side, volume },
        refused @ TradeNotice::Refused(_) => refused,
        other => TradeNotice::Refused(format!("close ok, market falhou ({})", other.toast())),
    }
}

async fn tools_call(
    client: &reqwest::Client,
    url: &str,
    token: &str,
    session: &str,
    rpc_id: &mut u64,
    name: &str,
    arguments: Value,
) -> Result<Value, String> {
    *rpc_id = rpc_id.saturating_add(1);
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": *rpc_id,
        "method": "tools/call",
        "params": { "name": name, "arguments": arguments }
    });
    let response = post(client, url, token, Some(session), &body).await?;
    read_rpc_body(response).await
}

async fn post(
    client: &reqwest::Client,
    url: &str,
    token: &str,
    session: Option<&str>,
    body: &Value,
) -> Result<reqwest::Response, String> {
    let mut request = client
        .post(url)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .header(
            reqwest::header::ACCEPT,
            "application/json, text/event-stream",
        )
        .header("MCP-Protocol-Version", MCP_PROTOCOL)
        .header(reqwest::header::AUTHORIZATION, format!("Bearer {token}"))
        .json(body);
    if let Some(session) = session {
        request = request.header("Mcp-Session-Id", session);
    }
    request.send().await.map_err(|error| error.to_string())
}

async fn read_rpc_body(response: reqwest::Response) -> Result<Value, String> {
    if !response.status().is_success() {
        return Err(format!("MCP HTTP {}", response.status()));
    }
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_owned();
    let bytes = response.bytes().await.map_err(|error| error.to_string())?;
    let text = String::from_utf8_lossy(&bytes);
    let payload = if content_type.contains("event-stream") || looks_like_sse(&text) {
        sse_data(&text)
    } else {
        text.into_owned()
    };
    let value: Value = serde_json::from_str(&payload).map_err(|error| error.to_string())?;
    if let Some(error) = value.get("error") {
        return Err(error.to_string());
    }
    Ok(value.get("result").cloned().unwrap_or(value))
}

fn looks_like_sse(text: &str) -> bool {
    let trimmed = text.trim_start();
    trimmed.starts_with("event:") || trimmed.starts_with("data:") || trimmed.starts_with("id:")
}

fn sse_data(text: &str) -> String {
    let mut data = String::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("data:") {
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(rest.trim_start());
        }
    }
    if data.is_empty() {
        text.to_owned()
    } else {
        data
    }
}

/// Pull tickets and pending orders for `symbol` out of an MCP tools/call result.
#[must_use]
pub fn parse_positions(value: &Value, symbol: &str) -> PositionSnapshot {
    let mut tickets = Vec::new();
    let mut orders = Vec::new();
    collect_book(value, &mut tickets, &mut orders);
    tickets.retain(|ticket| ticket.symbol.eq_ignore_ascii_case(symbol));
    orders.retain(|order| order.symbol.eq_ignore_ascii_case(symbol));
    tickets.sort_by_key(|ticket| ticket.ticket);
    orders.sort_by_key(|order| order.ticket);
    PositionSnapshot {
        tickets: tickets
            .into_iter()
            .map(|parsed| OpenTicket {
                ticket: parsed.ticket,
                side: parsed.side,
                volume: parsed.volume,
                price_open: parsed.price_open,
                profit: parsed.profit,
                currency: parsed.currency,
                stop_loss: parsed.stop_loss,
                take_profit: parsed.take_profit,
            })
            .collect(),
        orders: orders
            .into_iter()
            .map(|parsed| PendingOrder {
                ticket: parsed.ticket,
                side: parsed.side,
                kind: parsed.kind,
                volume: parsed.volume,
                price: parsed.price,
                stop_loss: parsed.stop_loss,
                take_profit: parsed.take_profit,
            })
            .collect(),
    }
}

struct ParsedTicket {
    ticket: u64,
    symbol: String,
    side: Side,
    volume: Decimal,
    price_open: Decimal,
    profit: Decimal,
    currency: Option<String>,
    stop_loss: Option<Decimal>,
    take_profit: Option<Decimal>,
}

struct ParsedOrder {
    ticket: u64,
    symbol: String,
    side: Side,
    kind: PendingKind,
    volume: Decimal,
    price: Decimal,
    stop_loss: Option<Decimal>,
    take_profit: Option<Decimal>,
}

fn collect_book(value: &Value, tickets: &mut Vec<ParsedTicket>, orders: &mut Vec<ParsedOrder>) {
    match value {
        Value::Array(items) => {
            for item in items {
                collect_book(item, tickets, orders);
            }
        }
        Value::Object(map) => {
            if let Some(order) = order_from_object(map) {
                orders.push(order);
            } else if let Some(ticket) = ticket_from_object(map) {
                tickets.push(ticket);
            }
            if let Some(Value::String(text)) = map.get("text")
                && let Ok(nested) = serde_json::from_str::<Value>(text)
            {
                collect_book(&nested, tickets, orders);
            }
            for (key, nested) in map {
                if key == "text" {
                    continue;
                }
                collect_book(nested, tickets, orders);
            }
        }
        Value::String(text) => {
            if let Ok(nested) = serde_json::from_str::<Value>(text) {
                collect_book(&nested, tickets, orders);
            }
        }
        _ => {}
    }
}

fn protective_levels(map: &Map<String, Value>) -> (Option<Decimal>, Option<Decimal>) {
    let stop_loss = decimal_field(map, &["sl", "stop_loss", "stopLoss", "price_sl"])
        .filter(|value| *value > Decimal::ZERO);
    let take_profit = decimal_field(map, &["tp", "take_profit", "takeProfit", "price_tp"])
        .filter(|value| *value > Decimal::ZERO);
    (stop_loss, take_profit)
}

fn ticket_from_object(map: &Map<String, Value>) -> Option<ParsedTicket> {
    let price_open = decimal_field(map, &["price_open", "open_price", "priceOpen"])?;
    let volume =
        decimal_field(map, &["volume", "volume_current", "lots", "qty"]).unwrap_or(Decimal::ONE);
    if volume <= Decimal::ZERO {
        return None;
    }
    let symbol = string_field(map, &["symbol", "Symbol"]).filter(|s| !s.is_empty())?;
    let ticket = int_field(map, &["ticket", "position_id", "ticket_id", "id"]).unwrap_or(0);
    let profit = decimal_field(map, &["profit", "profit_money", "unrealized_profit", "pnl"])
        .unwrap_or(Decimal::ZERO);
    let side = side_field(map)?;
    let currency = string_field(map, &["currency", "profit_currency", "account_currency"]);
    let (stop_loss, take_profit) = protective_levels(map);
    Some(ParsedTicket {
        ticket,
        symbol,
        side,
        volume,
        price_open,
        profit,
        currency,
        stop_loss,
        take_profit,
    })
}

fn order_from_object(map: &Map<String, Value>) -> Option<ParsedOrder> {
    let raw = map
        .get("type")
        .or_else(|| map.get("order_type"))
        .or_else(|| map.get("orderType"))
        .and_then(Value::as_str)?;
    let (side, kind) = pending_kind(raw)?;
    let price = decimal_field(map, &["price", "price_open", "price_order", "priceOpen"])?;
    let volume =
        decimal_field(map, &["volume", "volume_current", "lots", "qty"]).unwrap_or(Decimal::ONE);
    if volume <= Decimal::ZERO {
        return None;
    }
    let symbol = string_field(map, &["symbol", "Symbol"]).filter(|s| !s.is_empty())?;
    let ticket = int_field(map, &["ticket", "order", "order_ticket", "ticket_id", "id"])?;
    let (stop_loss, take_profit) = protective_levels(map);
    Some(ParsedOrder {
        ticket,
        symbol,
        side,
        kind,
        volume,
        price,
        stop_loss,
        take_profit,
    })
}

fn pending_kind(raw: &str) -> Option<(Side, PendingKind)> {
    let upper = raw.to_ascii_uppercase().replace('-', "_");
    let side = if upper.contains("SELL") {
        Side::Sell
    } else if upper.contains("BUY") {
        Side::Buy
    } else {
        return None;
    };
    let kind = if upper.contains("STOP") && !upper.contains("STOP_LIMIT") {
        PendingKind::Stop
    } else if upper.contains("LIMIT") {
        PendingKind::Limit
    } else {
        return None;
    };
    Some((side, kind))
}

fn side_field(map: &Map<String, Value>) -> Option<Side> {
    let raw = map
        .get("type")
        .or_else(|| map.get("position_type"))
        .or_else(|| map.get("side"))
        .or_else(|| map.get("action"))?;
    match raw {
        Value::Number(n) if n.as_u64() == Some(0) => Some(Side::Buy),
        Value::Number(n) if n.as_u64() == Some(1) => Some(Side::Sell),
        Value::String(text) => {
            let upper = text.to_ascii_uppercase();
            if upper.contains("SELL") || upper == "V" || upper == "SHORT" {
                Some(Side::Sell)
            } else if upper.contains("BUY") || upper == "C" || upper == "LONG" {
                Some(Side::Buy)
            } else {
                None
            }
        }
        _ => None,
    }
}

fn string_field(map: &Map<String, Value>, keys: &[&str]) -> Option<String> {
    for key in keys {
        match map.get(*key) {
            Some(Value::String(text)) if !text.is_empty() => return Some(text.clone()),
            Some(Value::Number(n)) => return Some(n.to_string()),
            _ => {}
        }
    }
    None
}

fn int_field(map: &Map<String, Value>, keys: &[&str]) -> Option<u64> {
    for key in keys {
        match map.get(*key) {
            Some(Value::Number(n)) => return n.as_u64(),
            Some(Value::String(text)) => return text.parse().ok(),
            _ => {}
        }
    }
    None
}

fn decimal_field(map: &Map<String, Value>, keys: &[&str]) -> Option<Decimal> {
    for key in keys {
        if let Some(value) = map.get(*key)
            && let Some(decimal) = decimal_value(value)
        {
            return Some(decimal);
        }
    }
    None
}

fn decimal_value(value: &Value) -> Option<Decimal> {
    match value {
        Value::Number(n) => n
            .as_f64()
            .and_then(Decimal::from_f64)
            .or_else(|| n.as_i64().map(Decimal::from)),
        Value::String(text) => text.parse().ok(),
        _ => None,
    }
}
