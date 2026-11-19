//! Execution target, Demo/Prod gate, persisted ticket fields, and MCP parse.
//!
//! Prod/Demo do not change the MetaTrader login. A venue send is refused when
//! the target disagrees with `get_trading_account_info.type`. Sim never hits
//! the MCP.

use quantick_engine::Side;
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::PositionSnapshot;

const TICKET_FILE: &str = "venue-ticket.toml";

/// Where Buy/Sell go: the platform simulator, or the connected MT5 account.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionTarget {
    Sim,
    Prod,
    Demo,
}

impl ExecutionTarget {
    pub const DEFAULT: Self = Self::Sim;

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Sim => "Sim",
            Self::Prod => "Prod",
            Self::Demo => "Demo",
        }
    }

    /// Account-type lock for a venue send. `None` on Sim (paper only).
    #[must_use]
    pub fn account_mode(self) -> Option<AccountMode> {
        match self {
            Self::Sim => None,
            Self::Prod => Some(AccountMode::Prod),
            Self::Demo => Some(AccountMode::Demo),
        }
    }

    /// Chips the toolbar shows: Prod and Sim always; Demo only on a demo login.
    #[must_use]
    pub fn visible(info: Option<&AccountInfo>) -> Vec<Self> {
        let mut targets = vec![Self::Prod, Self::Sim];
        if Self::demo_visible(info) {
            targets.insert(1, Self::Demo);
        }
        targets
    }

    #[must_use]
    pub fn demo_visible(info: Option<&AccountInfo>) -> bool {
        info.is_some_and(|info| info.mode == AccountMode::Demo)
    }

    /// Drop Demo when the connected account is no longer demo.
    #[must_use]
    pub fn reconcile(self, info: Option<&AccountInfo>) -> Self {
        if self == Self::Demo && !info.is_some_and(|info| info.mode == AccountMode::Demo) {
            Self::Sim
        } else {
            self
        }
    }

    fn from_persist(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "prod" | "real" => Self::Prod,
            "demo" => Self::Demo,
            _ => Self::Sim,
        }
    }

    fn persist(self) -> &'static str {
        match self {
            Self::Sim => "sim",
            Self::Prod => "prod",
            Self::Demo => "demo",
        }
    }
}

/// What kind of MetaTrader account is connected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountMode {
    Demo,
    Prod,
}

impl AccountMode {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Demo => "Demo",
            Self::Prod => "Prod",
        }
    }

    #[must_use]
    pub fn connected_word(self) -> &'static str {
        match self {
            Self::Demo => "demo",
            Self::Prod => "real",
        }
    }

    #[must_use]
    pub fn from_mcp(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "demo" | "contest" => Some(Self::Demo),
            "real" | "live" | "prod" => Some(Self::Prod),
            _ => None,
        }
    }
}

/// Facts the plate names, from `get_trading_account_info`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountInfo {
    pub mode: AccountMode,
    pub login: String,
    pub server: String,
    pub broker: String,
    pub trade_allowed: bool,
}

impl AccountInfo {
    #[must_use]
    pub fn caption(&self) -> String {
        let broker = if self.broker.is_empty() {
            "MT5"
        } else {
            self.broker.split_whitespace().next().unwrap_or("MT5")
        };
        format!("{broker} · {} · {}", self.mode.connected_word(), self.login)
    }
}

/// Positions plus the connected account, latest-wins.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AccountSnapshot {
    pub positions: PositionSnapshot,
    pub info: Option<AccountInfo>,
}

/// Why C/V/Fechar stay dark.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TradeBlock {
    NoSource,
    NoAccount,
    TradeOff,
    Mismatch {
        selector: AccountMode,
        connected: AccountMode,
    },
}

impl TradeBlock {
    #[must_use]
    pub fn explanation(&self) -> String {
        match self {
            Self::NoSource => "sem conta neste source".into(),
            Self::NoAccount => "aguardando a conta do MCP".into(),
            Self::TradeOff => "MCP trade is off on this terminal".into(),
            Self::Mismatch {
                selector,
                connected,
            } => format!(
                "conta ligada é {}; seletor está em {}",
                connected.connected_word(),
                selector.label()
            ),
        }
    }
}

/// Whether a send may leave this process.
#[must_use]
pub fn trade_block(
    source_ok: bool,
    selector: AccountMode,
    info: Option<&AccountInfo>,
) -> Option<TradeBlock> {
    if !source_ok {
        return Some(TradeBlock::NoSource);
    }
    let Some(info) = info else {
        return Some(TradeBlock::NoAccount);
    };
    if !info.trade_allowed {
        return Some(TradeBlock::TradeOff);
    }
    if selector != info.mode {
        return Some(TradeBlock::Mismatch {
            selector,
            connected: info.mode,
        });
    }
    None
}

/// A send the plate may post, or the block that kept it on this side.
pub fn allowed_command(
    block: Option<TradeBlock>,
    action: PendingAction,
) -> Result<TradeCommand, TradeBlock> {
    match block {
        Some(block) => Err(block),
        None => Ok(action.into_command()),
    }
}

/// Close every listed ticket, or `None` when there is nothing to send.
#[must_use]
pub fn close_command(tickets: &[u64], symbol: String) -> Option<TradeCommand> {
    (!tickets.is_empty()).then(|| TradeCommand::Close {
        tickets: tickets.to_vec(),
        symbol,
    })
}

/// Command the UI posts onto the poller thread.
#[derive(Debug, Clone)]
pub enum TradeCommand {
    PlaceMarket {
        side: Side,
        volume: Decimal,
        symbol: String,
        stop_loss: Option<Decimal>,
        take_profit: Option<Decimal>,
    },
    PlacePending {
        side: Side,
        kind: super::PendingKind,
        volume: Decimal,
        price: Decimal,
        symbol: String,
        stop_loss: Option<Decimal>,
        take_profit: Option<Decimal>,
    },
    ModifySlTp {
        position_ticket: u64,
        symbol: String,
        stop_loss: Option<Decimal>,
        take_profit: Option<Decimal>,
    },
    DeleteOrder {
        order_ticket: u64,
        symbol: String,
    },
    Close {
        tickets: Vec<u64>,
        symbol: String,
    },
    Reverse {
        tickets: Vec<u64>,
        symbol: String,
        side: Side,
        volume: Decimal,
        stop_loss: Option<Decimal>,
        take_profit: Option<Decimal>,
    },
}

/// What the poller tells the toast lane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TradeNotice {
    Refused(String),
    Placed {
        side: Side,
        volume: Decimal,
    },
    PendingPlaced {
        side: Side,
        kind: super::PendingKind,
        price: Decimal,
    },
    BracketSet,
    OrderDeleted,
    Closed {
        count: usize,
    },
    Reversed {
        side: Side,
        volume: Decimal,
    },
}

impl TradeNotice {
    #[must_use]
    pub fn toast(&self) -> String {
        match self {
            Self::Refused(reason) => format!("MT5: {reason}"),
            Self::Placed { side, volume } => {
                let letter = match side {
                    Side::Buy => 'C',
                    Side::Sell => 'V',
                };
                format!("MT5: {letter} {volume}")
            }
            Self::PendingPlaced { side, kind, price } => {
                let letter = match side {
                    Side::Buy => 'C',
                    Side::Sell => 'V',
                };
                format!("MT5: {letter} {} @ {price}", kind.label())
            }
            Self::BracketSet => "MT5: SL/TP updated".into(),
            Self::OrderDeleted => "MT5: pending cancelled".into(),
            Self::Closed { count } => format!("MT5: fechou {count}"),
            Self::Reversed { side, volume } => {
                let letter = match side {
                    Side::Buy => 'C',
                    Side::Sell => 'V',
                };
                format!("MT5: reverse → {letter} {volume}")
            }
        }
    }
}

/// Prod confirm, or a Demo send waiting to go.
#[derive(Debug, Clone)]
pub struct PendingTrade {
    pub action: PendingAction,
}

#[derive(Debug, Clone)]
pub enum PendingAction {
    Market {
        side: Side,
        volume: Decimal,
        symbol: String,
        stop_loss: Option<Decimal>,
        take_profit: Option<Decimal>,
    },
    Pending {
        side: Side,
        kind: super::PendingKind,
        volume: Decimal,
        price: Decimal,
        symbol: String,
        stop_loss: Option<Decimal>,
        take_profit: Option<Decimal>,
    },
    ModifySlTp {
        position_ticket: u64,
        symbol: String,
        stop_loss: Option<Decimal>,
        take_profit: Option<Decimal>,
    },
    DeleteOrder {
        order_ticket: u64,
        symbol: String,
    },
    Close {
        tickets: Vec<u64>,
        symbol: String,
    },
    Reverse {
        tickets: Vec<u64>,
        symbol: String,
        side: Side,
        volume: Decimal,
        stop_loss: Option<Decimal>,
        take_profit: Option<Decimal>,
    },
}

impl PendingAction {
    #[must_use]
    pub fn summary(&self) -> String {
        match self {
            Self::Market {
                side,
                volume,
                symbol,
                stop_loss,
                take_profit,
            } => {
                let letter = match side {
                    Side::Buy => 'C',
                    Side::Sell => 'V',
                };
                let mut line = format!("{letter} {volume} {symbol} a mercado");
                if let Some(sl) = stop_loss {
                    line.push_str(&format!(" SL {sl}"));
                }
                if let Some(tp) = take_profit {
                    line.push_str(&format!(" TP {tp}"));
                }
                line
            }
            Self::Pending {
                side,
                kind,
                volume,
                price,
                symbol,
                ..
            } => {
                let letter = match side {
                    Side::Buy => 'C',
                    Side::Sell => 'V',
                };
                format!("{letter} {} {volume} {symbol} @ {price}", kind.label())
            }
            Self::ModifySlTp {
                position_ticket,
                stop_loss,
                take_profit,
                ..
            } => format!(
                "SL/TP ticket {position_ticket} → SL {} TP {}",
                stop_loss
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "—".into()),
                take_profit
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "—".into()),
            ),
            Self::DeleteOrder {
                order_ticket,
                symbol,
            } => format!("cancel pending {order_ticket} {symbol}"),
            Self::Close { tickets, symbol } => {
                format!("fechar {} {} em {symbol}", tickets.len(), {
                    if tickets.len() == 1 {
                        "ticket"
                    } else {
                        "tickets"
                    }
                })
            }
            Self::Reverse {
                side,
                volume,
                symbol,
                ..
            } => {
                let letter = match side {
                    Side::Buy => 'C',
                    Side::Sell => 'V',
                };
                format!("reverse → {letter} {volume} {symbol}")
            }
        }
    }

    #[must_use]
    pub fn into_command(self) -> TradeCommand {
        match self {
            Self::Market {
                side,
                volume,
                symbol,
                stop_loss,
                take_profit,
            } => TradeCommand::PlaceMarket {
                side,
                volume,
                symbol,
                stop_loss,
                take_profit,
            },
            Self::Pending {
                side,
                kind,
                volume,
                price,
                symbol,
                stop_loss,
                take_profit,
            } => TradeCommand::PlacePending {
                side,
                kind,
                volume,
                price,
                symbol,
                stop_loss,
                take_profit,
            },
            Self::ModifySlTp {
                position_ticket,
                symbol,
                stop_loss,
                take_profit,
            } => TradeCommand::ModifySlTp {
                position_ticket,
                symbol,
                stop_loss,
                take_profit,
            },
            Self::DeleteOrder {
                order_ticket,
                symbol,
            } => TradeCommand::DeleteOrder {
                order_ticket,
                symbol,
            },
            Self::Close { tickets, symbol } => TradeCommand::Close { tickets, symbol },
            Self::Reverse {
                tickets,
                symbol,
                side,
                volume,
                stop_loss,
                take_profit,
            } => TradeCommand::Reverse {
                tickets,
                symbol,
                side,
                volume,
                stop_loss,
                take_profit,
            },
        }
    }
}

/// Execution target and the Prod confirm, persisted in the cockpit home.
#[derive(Debug, Clone)]
pub struct VenueTicket {
    pub target: ExecutionTarget,
    /// Absolute price for a Prod/Demo limit or stop (Trading tab).
    pub pending_price: String,
    /// Absolute SL typed on CONTA for the open position.
    pub sl_text: String,
    /// Absolute TP typed on CONTA for the open position.
    pub tp_text: String,
    pub pending: Option<PendingTrade>,
    /// Last line the plate should show (`MT5: …`).
    pub status: Option<String>,
    /// One-shot toast the frame drain publishes once.
    toast_once: Option<String>,
}

impl Default for VenueTicket {
    fn default() -> Self {
        Self {
            target: ExecutionTarget::DEFAULT,
            pending_price: String::new(),
            sl_text: String::new(),
            tp_text: String::new(),
            pending: None,
            status: None,
            toast_once: None,
        }
    }
}

impl VenueTicket {
    #[must_use]
    pub fn load() -> Self {
        let path = ticket_path();
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        let Ok(saved) = toml::from_str::<SavedTicket>(&text) else {
            return Self::default();
        };
        Self {
            target: ExecutionTarget::from_persist(&saved.target),
            pending_price: String::new(),
            sl_text: String::new(),
            tp_text: String::new(),
            pending: None,
            status: None,
            toast_once: None,
        }
    }

    #[must_use]
    pub fn parsed_pending_price(&self) -> Option<Decimal> {
        let value: Decimal = self.pending_price.trim().parse().ok()?;
        (value > Decimal::ZERO).then_some(value)
    }

    #[must_use]
    pub fn parsed_level(text: &str) -> Option<Decimal> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return None;
        }
        let value: Decimal = trimmed.parse().ok()?;
        (value > Decimal::ZERO).then_some(value)
    }

    /// Keep Demo only while a demo account is connected.
    pub fn reconcile(&mut self, info: Option<&AccountInfo>) {
        let next = self.target.reconcile(info);
        if next != self.target {
            self.target = next;
            self.persist();
        }
    }

    pub fn set_status(&mut self, message: impl Into<String>) {
        self.status = Some(message.into());
    }

    /// Keep the plate line and queue one toast for the frame drain.
    pub fn announce(&mut self, message: impl Into<String>) {
        let message = message.into();
        self.status = Some(message.clone());
        self.toast_once = Some(message);
    }

    pub fn take_toast(&mut self) -> Option<String> {
        self.toast_once.take()
    }

    pub fn persist(&self) {
        let saved = SavedTicket {
            target: ExecutionTarget::persist(self.target).to_owned(),
        };
        let Ok(text) = toml::to_string(&saved) else {
            return;
        };
        let path = ticket_path();
        if crate::store_home::guard_write(&path).is_err() {
            return;
        }
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&path, text);
    }
}

fn ticket_path() -> std::path::PathBuf {
    if cfg!(test) {
        crate::store_home::test_path(TICKET_FILE)
    } else {
        crate::store_home::resolve(TICKET_FILE)
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct SavedTicket {
    #[serde(default = "default_target", alias = "selector")]
    target: String,
}

fn default_target() -> String {
    "sim".into()
}

/// Walk an MCP tools/call result for the connected account.
#[must_use]
pub fn parse_account_info(value: &Value) -> Option<AccountInfo> {
    let mut found = None;
    walk_account(value, &mut found);
    found
}

fn walk_account(value: &Value, found: &mut Option<AccountInfo>) {
    if found.is_some() {
        return;
    }
    match value {
        Value::Array(items) => {
            for item in items {
                walk_account(item, found);
            }
        }
        Value::Object(map) => {
            if let Some(info) = info_from_object(map) {
                *found = Some(info);
                return;
            }
            if let Some(Value::String(text)) = map.get("text")
                && let Ok(nested) = serde_json::from_str::<Value>(text)
            {
                walk_account(&nested, found);
            }
            for (key, nested) in map {
                if key == "text" {
                    continue;
                }
                walk_account(nested, found);
            }
        }
        Value::String(text) => {
            if let Ok(nested) = serde_json::from_str::<Value>(text) {
                walk_account(&nested, found);
            }
        }
        _ => {}
    }
}

fn info_from_object(map: &serde_json::Map<String, Value>) -> Option<AccountInfo> {
    let account = map.get("account").and_then(Value::as_object).unwrap_or(map);
    let mode = account
        .get("type")
        .and_then(Value::as_str)
        .and_then(AccountMode::from_mcp)?;
    let login = string_in(account, &["login", "Login", "id"]).unwrap_or_default();
    let server = string_in(account, &["server", "Server"]).unwrap_or_default();
    let broker = string_in(account, &["broker", "company", "name"]).unwrap_or_default();
    let trade_allowed = bool_in(map, &["mcp_trade_allowed"])
        .or_else(|| {
            map.get("terminal")
                .and_then(Value::as_object)
                .and_then(|terminal| bool_in(terminal, &["mcp_trade_allowed"]))
        })
        .unwrap_or(true);
    Some(AccountInfo {
        mode,
        login,
        server,
        broker,
        trade_allowed,
    })
}

fn string_in(map: &serde_json::Map<String, Value>, keys: &[&str]) -> Option<String> {
    for key in keys {
        match map.get(*key) {
            Some(Value::String(text)) if !text.is_empty() => return Some(text.clone()),
            Some(Value::Number(n)) => return Some(n.to_string()),
            _ => {}
        }
    }
    None
}

fn bool_in(map: &serde_json::Map<String, Value>, keys: &[&str]) -> Option<bool> {
    for key in keys {
        match map.get(*key) {
            Some(Value::Bool(value)) => return Some(*value),
            Some(Value::String(text)) => match text.to_ascii_lowercase().as_str() {
                "true" | "1" => return Some(true),
                "false" | "0" => return Some(false),
                _ => {}
            },
            _ => {}
        }
    }
    None
}

/// Volume the MCP tools accept, or `None` when it cannot be a finite lot size.
#[must_use]
pub fn volume_lots(volume: Decimal) -> Option<f64> {
    volume
        .to_f64()
        .filter(|value| value.is_finite() && *value > 0.0)
}

/// Absolute price the MCP tools accept.
#[must_use]
pub fn price_lots(price: Decimal) -> Option<f64> {
    price
        .to_f64()
        .filter(|value| value.is_finite() && *value > 0.0)
}

/// Whether a tools/call result is a refusal, and the words to show.
///
/// The MetaQuotes MCP reports a done market check/send as `ok: true` with
/// `retcode: 0` ("Done"). The classic MQL5 codes 10008–10010 are also success.
#[must_use]
pub fn tool_refusal(value: &Value) -> Option<String> {
    if value
        .get("isError")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Some(tool_message(value));
    }
    if trade_ok(value) {
        return None;
    }
    if let Some(code) = retcode(value)
        && !trade_retcode_ok(code)
    {
        return Some(tool_message(value));
    }
    None
}

fn trade_ok(value: &Value) -> bool {
    if value.get("ok").and_then(Value::as_bool) == Some(true) {
        return true;
    }
    if let Some(code) = retcode(value)
        && trade_retcode_ok(code)
    {
        return true;
    }
    match value {
        Value::Object(map) => {
            if let Some(Value::String(text)) = map.get("text")
                && let Ok(nested) = serde_json::from_str::<Value>(text)
            {
                return trade_ok(&nested);
            }
            map.values().any(trade_ok)
        }
        Value::Array(items) => items.iter().any(trade_ok),
        Value::String(text) => serde_json::from_str::<Value>(text)
            .ok()
            .is_some_and(|nested| trade_ok(&nested)),
        _ => false,
    }
}

fn trade_retcode_ok(code: u64) -> bool {
    matches!(code, 0 | 10008..=10010)
}

fn retcode(value: &Value) -> Option<u64> {
    match value {
        Value::Object(map) => {
            if let Some(Value::Number(n)) = map
                .get("retcode")
                .or_else(|| map.get("retCode"))
                .or_else(|| map.get("code"))
            {
                return n.as_u64();
            }
            for nested in map.values() {
                if let Some(code) = retcode(nested) {
                    return Some(code);
                }
            }
            None
        }
        Value::Array(items) => items.iter().find_map(retcode),
        Value::String(text) => serde_json::from_str::<Value>(text)
            .ok()
            .and_then(|v| retcode(&v)),
        _ => None,
    }
}

fn tool_message(value: &Value) -> String {
    if let Some(text) = value.get("content").and_then(|content| {
        content.as_array().and_then(|items| {
            items.iter().find_map(|item| {
                item.get("text")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned)
            })
        })
    }) {
        return text;
    }
    if let Some(comment) = value
        .get("comment")
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
    {
        return comment.to_owned();
    }
    value.to_string()
}
