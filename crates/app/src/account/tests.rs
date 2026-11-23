use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::venue::{AccountInfo, tool_refusal};
use super::{
    AccountMode, ExecutionTarget, OpenTicket, PendingAction, PositionSnapshot, TradeBlock,
    VenueTicket, allowed_command, badge_lines, close_command, parse_account_info, parse_positions,
    resolve_token, trade_block,
};
use quantick_engine::Side;
use rust_decimal::Decimal;
use serde_json::json;

fn ticket(side: Side, volume: i64, price: i64, profit: i64) -> OpenTicket {
    OpenTicket {
        ticket: price.unsigned_abs(),
        side,
        volume: Decimal::from(volume),
        price_open: Decimal::from(price),
        profit: Decimal::from(profit),
        currency: Some("BRL".into()),
        stop_loss: None,
        take_profit: None,
    }
}

#[test]
fn the_badge_vanishes_without_a_snapshot() {
    assert!(badge_lines(&PositionSnapshot::default()).is_empty());
}

#[test]
fn two_tickets_on_the_same_paper_sum_pnl_and_average_the_entry() {
    let snapshot = PositionSnapshot {
        tickets: vec![
            ticket(Side::Sell, 1, 172_460, -100),
            ticket(Side::Sell, 1, 172_480, -80),
        ],
        orders: vec![],
    };
    assert_eq!(snapshot.total_profit(), Decimal::from(-180));
    assert_eq!(snapshot.entry_vwap(), Some(Decimal::from(172_470)));
    assert_eq!(snapshot.net_side(), Some(Side::Sell));
    assert_eq!(
        badge_lines(&snapshot),
        ["V 1 @ 172460", "V 1 @ 172480", "aberto \u{2212}180 R$"]
    );
}

#[test]
fn a_hedge_keeps_one_badge_line_per_ticket() {
    let snapshot = PositionSnapshot {
        tickets: vec![
            ticket(Side::Buy, 2, 100, 10),
            ticket(Side::Sell, 1, 110, -4),
        ],
        orders: vec![],
    };
    assert_eq!(snapshot.net_side(), Some(Side::Buy));
    assert_eq!(
        snapshot.entry_vwap(),
        Some(Decimal::new(310, 0) / Decimal::from(3))
    );
    let lines = badge_lines(&snapshot);
    assert_eq!(lines[0], "C 2 @ 100");
    assert_eq!(lines[1], "V 1 @ 110");
    assert!(lines[2].starts_with("aberto +6"));
}

#[test]
fn parse_keeps_the_chart_symbol_and_drops_the_others() {
    let payload = json!({
        "content": [{
            "type": "text",
            "text": "[{\"ticket\":11,\"symbol\":\"WINZ26\",\"type\":\"POSITION_TYPE_SELL\",\"volume\":1,\"price_open\":172460,\"profit\":-180,\"currency\":\"BRL\"},{\"ticket\":12,\"symbol\":\"WDO$N\",\"type\":\"POSITION_TYPE_BUY\",\"volume\":1,\"price_open\":5800,\"profit\":10,\"currency\":\"BRL\"}]"
        }]
    });
    let snapshot = parse_positions(&payload, "winz26");
    assert_eq!(snapshot.tickets.len(), 1);
    assert_eq!(snapshot.tickets[0].ticket, 11);
    assert_eq!(snapshot.tickets[0].side, Side::Sell);
    assert_eq!(snapshot.total_profit(), Decimal::from(-180));
    assert_eq!(
        badge_lines(&snapshot),
        ["V 1 @ 172460", "aberto \u{2212}180 R$"]
    );
}

#[test]
fn parse_reads_a_positions_array_without_inventing_pnl() {
    let payload = json!({
        "positions": [{
            "ticket": 7,
            "symbol": "XAUUSD",
            "side": "buy",
            "volume": "0.10",
            "price_open": "2650.5",
            "profit": "12.3"
        }]
    });
    let snapshot = parse_positions(&payload, "XAUUSD");
    assert_eq!(snapshot.tickets.len(), 1);
    assert_eq!(snapshot.tickets[0].side, Side::Buy);
    assert!(snapshot.currency().is_none());
    assert_eq!(badge_lines(&snapshot).last().unwrap(), "aberto +12.3");
}

#[test]
fn an_empty_mcp_url_keeps_the_overlay_dark() {
    let settings = crate::config::MetaTraderSettings {
        mcp_token: "not-empty".into(),
        mcp_url: String::new(),
        ..crate::config::MetaTraderSettings::default()
    };
    assert!(resolve_token(&settings).is_none());
}

fn real_account() -> AccountInfo {
    AccountInfo {
        mode: AccountMode::Prod,
        login: "4526573".into(),
        server: "Genial-Real".into(),
        broker: "Genial Investimentos".into(),
        trade_allowed: true,
    }
}

#[test]
fn the_execution_target_opens_on_sim() {
    assert_eq!(ExecutionTarget::DEFAULT, ExecutionTarget::Sim);
    assert_eq!(VenueTicket::default().target, ExecutionTarget::Sim);
    crate::store_home::next_test_home();
    assert_eq!(VenueTicket::load().target, ExecutionTarget::Sim);
}

#[test]
fn one_click_persists_across_load() {
    crate::store_home::next_test_home();
    let mut ticket = VenueTicket::default();
    assert!(!ticket.one_click);
    ticket.set_one_click(true);
    assert!(VenueTicket::load().one_click);
    ticket.set_one_click(false);
    assert!(!VenueTicket::load().one_click);
}

#[test]
fn visible_targets_hide_demo_on_a_real_account() {
    assert_eq!(
        ExecutionTarget::visible(None),
        vec![ExecutionTarget::Prod, ExecutionTarget::Sim]
    );
    assert_eq!(
        ExecutionTarget::visible(Some(&real_account())),
        vec![ExecutionTarget::Prod, ExecutionTarget::Sim]
    );
    let demo = AccountInfo {
        mode: AccountMode::Demo,
        login: "1".into(),
        server: "Demo".into(),
        broker: "Broker".into(),
        trade_allowed: true,
    };
    assert_eq!(
        ExecutionTarget::visible(Some(&demo)),
        vec![
            ExecutionTarget::Prod,
            ExecutionTarget::Demo,
            ExecutionTarget::Sim
        ]
    );
}

#[test]
fn demo_target_falls_back_to_sim_when_the_account_is_real() {
    assert_eq!(
        ExecutionTarget::Demo.reconcile(Some(&real_account())),
        ExecutionTarget::Sim
    );
    assert_eq!(
        ExecutionTarget::Prod.reconcile(Some(&real_account())),
        ExecutionTarget::Prod
    );
}

#[test]
fn prod_target_on_a_demo_account_produces_no_command() {
    let info = AccountInfo {
        mode: AccountMode::Demo,
        login: "1".into(),
        server: "Demo".into(),
        broker: "Broker".into(),
        trade_allowed: true,
    };
    let block = trade_block(true, AccountMode::Prod, Some(&info));
    let action = PendingAction::Market {
        side: Side::Buy,
        volume: Decimal::ONE,
        symbol: "WINZ26".into(),
        stop_loss: None,
        take_profit: None,
    };
    assert!(matches!(
        block,
        Some(TradeBlock::Mismatch {
            selector: AccountMode::Prod,
            connected: AccountMode::Demo
        })
    ));
    assert!(allowed_command(block, action).is_err());
}

#[test]
fn demo_selector_on_a_real_account_produces_no_command() {
    let info = real_account();
    let block = trade_block(true, AccountMode::Demo, Some(&info));
    let action = PendingAction::Market {
        side: Side::Buy,
        volume: Decimal::ONE,
        symbol: "WINZ26".into(),
        stop_loss: None,
        take_profit: None,
    };
    assert!(matches!(
        block,
        Some(TradeBlock::Mismatch {
            selector: AccountMode::Demo,
            connected: AccountMode::Prod
        })
    ));
    assert!(allowed_command(block, action).is_err());
}

#[test]
fn prod_on_a_real_account_arms_the_command() {
    let info = real_account();
    let block = trade_block(true, AccountMode::Prod, Some(&info));
    let action = PendingAction::Market {
        side: Side::Buy,
        volume: Decimal::ONE,
        symbol: "WINZ26".into(),
        stop_loss: None,
        take_profit: None,
    };
    assert!(block.is_none());
    assert!(allowed_command(block, action).is_ok());
}

#[test]
fn absolute_bracket_places_long_protection_below_and_above() {
    let (sl, tp) = super::plate::absolute_bracket(
        Side::Buy,
        Decimal::from(100),
        Some(Decimal::from(5)),
        Some(Decimal::from(10)),
    );
    assert_eq!(sl, Some(Decimal::from(95)));
    assert_eq!(tp, Some(Decimal::from(110)));
}

#[test]
fn venue_session_snaps_pending_to_instrument_tick() {
    let mut session = super::VenueSession::default();
    assert_eq!(
        session.snap_price(Decimal::from(167_604)),
        Decimal::from(167_604),
        "no tick known: leave the click alone"
    );
    session.set_price_tick(Some(Decimal::from(5)));
    assert_eq!(
        session.snap_price(Decimal::from(167_604)),
        Decimal::from(167_605),
        "WIN tick 5: Invalid price (10015) without this"
    );
    assert_eq!(
        session.snap_price(Decimal::from(167_602)),
        Decimal::from(167_600)
    );
}

#[test]
fn parse_reads_pending_orders_and_brackets() {
    let payload = json!({
        "content": [{
            "type": "text",
            "text": "{\"positions\":[{\"ticket\":11,\"symbol\":\"WINZ26\",\"type\":\"POSITION_TYPE_BUY\",\"volume\":1,\"price_open\":100,\"profit\":0,\"sl\":95,\"tp\":110}],\"orders\":[{\"ticket\":22,\"symbol\":\"WINZ26\",\"type\":\"ORDER_TYPE_SELL_LIMIT\",\"volume\":1,\"price\":105,\"sl\":110,\"tp\":90}]}"
        }]
    });
    let snapshot = parse_positions(&payload, "WINZ26");
    assert_eq!(snapshot.tickets.len(), 1);
    assert_eq!(snapshot.tickets[0].stop_loss, Some(Decimal::from(95)));
    assert_eq!(snapshot.tickets[0].take_profit, Some(Decimal::from(110)));
    assert_eq!(snapshot.orders.len(), 1);
    assert_eq!(snapshot.orders[0].ticket, 22);
    assert_eq!(snapshot.orders[0].side, Side::Sell);
    assert_eq!(snapshot.orders[0].kind, super::PendingKind::Limit);
}

#[test]
fn close_without_tickets_is_a_noop() {
    assert!(close_command(&[], "WINZ26".into()).is_none());
}

#[test]
fn parse_reads_the_connected_account_type() {
    let payload = json!({
        "content": [{
            "type": "text",
            "text": "{\"account\":{\"login\":4526573,\"type\":\"real\",\"server\":\"Genial-Real\",\"broker\":\"Genial Investimentos\"},\"terminal\":{\"mcp_trade_allowed\":true}}"
        }]
    });
    let info = parse_account_info(&payload).expect("account");
    assert_eq!(info.mode, AccountMode::Prod);
    assert_eq!(info.login, "4526573");
    assert_eq!(info.caption(), "Genial · real · 4526573");
}

#[test]
fn a_refused_check_never_sends() {
    let (url, seen) = serve_tools();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let notice = runtime.block_on(async {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .expect("client");
        let mut rpc_id = 1;
        super::poller::place_market(
            &client,
            &url,
            "tok",
            "sess",
            &mut rpc_id,
            Side::Buy,
            Decimal::ONE,
            "WINZ26".into(),
            None,
            None,
        )
        .await
    });
    match notice {
        super::TradeNotice::Refused(reason) => {
            assert!(reason.contains("no money"), "{reason}");
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
    let names = seen.lock().expect("seen").clone();
    assert!(
        names.iter().any(|name| name == "trade_check_market_order"),
        "check must run: {names:?}"
    );
    assert!(
        !names.iter().any(|name| name == "trade_send_market_order"),
        "send must not run: {names:?}"
    );
}

#[test]
fn tool_refusal_keeps_the_mt5_comment() {
    let value = json!({
        "content": [{"type": "text", "text": "no money"}],
        "isError": true
    });
    assert_eq!(tool_refusal(&value).as_deref(), Some("no money"));
    let passed = json!({"retcode": 10009});
    assert_eq!(tool_refusal(&passed), None);
}

#[test]
fn a_done_mcp_trade_with_retcode_zero_is_not_a_refusal() {
    let value = json!({
        "ok": true,
        "request": "exchange sell 1 WINZ26 at market",
        "result": {
            "retcode": 0,
            "retcode_string": "Done",
            "balance": 248.0,
            "equity": 248.0
        }
    });
    assert_eq!(tool_refusal(&value), None);
    let nested = json!({
        "content": [{
            "type": "text",
            "text": "{\"ok\":true,\"result\":{\"retcode\":0,\"retcode_string\":\"Done\"}}"
        }]
    });
    assert_eq!(tool_refusal(&nested), None);
}

fn serve_tools() -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    listener.set_nonblocking(false).expect("blocking accept");
    let addr = listener.local_addr().expect("addr");
    let seen = Arc::new(Mutex::new(Vec::new()));
    let seen_thread = Arc::clone(&seen);
    std::thread::spawn(move || {
        while let Ok((mut stream, _)) = listener.accept() {
            let request = read_http(&mut stream);
            if let Some(name) = tool_name(&request) {
                seen_thread.lock().expect("seen").push(name);
            }
            let body = json!({
                "jsonrpc": "2.0",
                "id": 2,
                "result": {
                    "content": [{"type": "text", "text": "no money"}],
                    "isError": true
                }
            })
            .to_string();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
        }
    });
    (format!("http://{addr}/mcp"), seen)
}

fn read_http(stream: &mut std::net::TcpStream) -> String {
    stream
        .set_read_timeout(Some(Duration::from_millis(400)))
        .ok();
    let mut data = Vec::new();
    let mut buf = [0_u8; 2048];
    while let Ok(n) = stream.read(&mut buf) {
        if n == 0 {
            break;
        }
        data.extend_from_slice(&buf[..n]);
        let text = String::from_utf8_lossy(&data);
        if let Some(split) = text.find("\r\n\r\n") {
            let headers = &text[..split];
            let length = headers.lines().find_map(|line| {
                line.split_once(':').and_then(|(name, value)| {
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                })
            });
            if let Some(length) = length
                && data.len() >= split + 4 + length
            {
                break;
            }
        }
    }
    String::from_utf8_lossy(&data).into_owned()
}

fn tool_name(request: &str) -> Option<String> {
    let body = request.split("\r\n\r\n").nth(1)?;
    let value: serde_json::Value = serde_json::from_str(body.trim()).ok()?;
    value
        .get("params")
        .and_then(|params| params.get("name"))
        .and_then(serde_json::Value::as_str)
        .map(ToOwned::to_owned)
}
