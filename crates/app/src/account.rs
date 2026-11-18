//! Live account overlay and the CONTA plate: positions and account type from
//! the local MetaTrader MCP, plus close on the poller thread. Buy/Sell live on
//! the toolbar and route through [`ExecutionTarget`].
//!
//! The feed crate never sees this. Overlay paint stays read-only. Orders leave
//! only through [`AccountLink::submit`], after the Prod/Demo gate.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use eframe::egui;
use quantick_engine::Side;
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use tokio::sync::watch;

use crate::chart::PriceScale;
use crate::config::MetaTraderSettings;
use crate::paper_chrome::fmt_decimal;
use crate::theme;

mod plate;
mod poller;
mod venue;

#[cfg_attr(not(test), allow(unused_imports))]
pub use poller::parse_positions;
pub use venue::{
    AccountInfo, AccountMode, AccountSnapshot, ExecutionTarget, PendingAction, PendingTrade,
    TradeBlock, TradeCommand, TradeNotice, VenueTicket, allowed_command, close_command,
    parse_account_info, price_lots, trade_block, volume_lots,
};

pub(crate) use plate::{
    VenuePlateEnv, draw_conta_plate, draw_prod_confirm, request_venue_close, request_venue_entry,
    request_venue_reverse,
};

/// How often the poller asks the MCP, in milliseconds.
const POLL_MS: u64 = 1_000;
/// Give up on one HTTP round-trip rather than pile calls behind a hung MCP.
const HTTP_TIMEOUT: Duration = Duration::from_secs(2);
const MCP_PROTOCOL: &str = "2025-06-18";
const TOKEN_ENV: &str = "QUANTICK_MT5_MCP_TOKEN";

const BADGE_GAP_PX: f32 = 6.0;
const BADGE_FONT_PX: f32 = 10.0;
const BADGE_RADIUS_PX: f32 = 4.0;
const BADGE_PAD_X: f32 = 6.0;
const BADGE_PAD_Y: f32 = 4.0;
const BADGE_MARGIN_PX: f32 = 6.0;

/// One open ticket on the chart's symbol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenTicket {
    pub ticket: u64,
    pub side: Side,
    pub volume: Decimal,
    pub price_open: Decimal,
    pub profit: Decimal,
    pub currency: Option<String>,
    pub stop_loss: Option<Decimal>,
    pub take_profit: Option<Decimal>,
}

/// Limit or stop resting on the connected account.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingKind {
    Limit,
    Stop,
}

impl PendingKind {
    #[must_use]
    pub fn mcp_type(self, side: Side) -> &'static str {
        match (self, side) {
            (Self::Limit, Side::Buy) => "buy_limit",
            (Self::Limit, Side::Sell) => "sell_limit",
            (Self::Stop, Side::Buy) => "buy_stop",
            (Self::Stop, Side::Sell) => "sell_stop",
        }
    }

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Limit => "limit",
            Self::Stop => "stop",
        }
    }
}

/// One pending order on the chart's symbol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingOrder {
    pub ticket: u64,
    pub side: Side,
    pub kind: PendingKind,
    pub volume: Decimal,
    pub price: Decimal,
    pub stop_loss: Option<Decimal>,
    pub take_profit: Option<Decimal>,
}

/// Latest-wins picture of the open tickets and pending orders on the chart symbol.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PositionSnapshot {
    pub tickets: Vec<OpenTicket>,
    pub orders: Vec<PendingOrder>,
}

impl PositionSnapshot {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tickets.is_empty()
    }

    /// Volume-weighted average of [`OpenTicket::price_open`]. `None` while empty.
    #[must_use]
    pub fn entry_vwap(&self) -> Option<Decimal> {
        let mut weighted = Decimal::ZERO;
        let mut volume = Decimal::ZERO;
        for ticket in &self.tickets {
            weighted += ticket.price_open * ticket.volume;
            volume += ticket.volume;
        }
        (volume > Decimal::ZERO).then_some(weighted / volume)
    }

    /// Net signed volume: buy positive, sell negative. `None` while empty.
    #[must_use]
    pub fn net_side(&self) -> Option<Side> {
        let net = self.net_volume()?;
        if net > Decimal::ZERO {
            Some(Side::Buy)
        } else if net < Decimal::ZERO {
            Some(Side::Sell)
        } else {
            self.tickets.first().map(|ticket| ticket.side)
        }
    }

    /// Signed net volume (buy − sell). `None` while empty.
    #[must_use]
    pub fn net_volume(&self) -> Option<Decimal> {
        (!self.tickets.is_empty()).then(|| {
            self.tickets
                .iter()
                .fold(Decimal::ZERO, |net, ticket| match ticket.side {
                    Side::Buy => net + ticket.volume,
                    Side::Sell => net - ticket.volume,
                })
        })
    }

    /// Absolute net size for reverse/close labels.
    #[must_use]
    pub fn net_abs_volume(&self) -> Option<Decimal> {
        self.net_volume().map(|volume| volume.abs())
    }

    /// Toolbar close face while a venue position is open.
    #[must_use]
    pub fn close_label(&self) -> Option<String> {
        let side = self.net_side()?;
        let volume = self.net_abs_volume()?;
        let word = match side {
            Side::Buy => "LONG",
            Side::Sell => "SHORT",
        };
        Some(format!(
            "Close {} {word}",
            crate::paper_chrome::fmt_decimal(volume)
        ))
    }

    /// Sum of open profit across every ticket. Zero while empty, never invented.
    #[must_use]
    pub fn total_profit(&self) -> Decimal {
        self.tickets.iter().map(|ticket| ticket.profit).sum()
    }

    /// Account currency, when every ticket that named one agrees.
    #[must_use]
    pub fn currency(&self) -> Option<&str> {
        let mut named = self
            .tickets
            .iter()
            .filter_map(|ticket| ticket.currency.as_deref());
        let first = named.next()?;
        named.all(|other| other == first).then_some(first)
    }
}

/// Badge lines for the chart symbol: one per ticket, then the open P&L.
#[must_use]
pub fn badge_lines(snapshot: &PositionSnapshot) -> Vec<String> {
    if snapshot.is_empty() {
        return Vec::new();
    }
    let mut lines: Vec<String> = snapshot
        .tickets
        .iter()
        .map(|ticket| {
            let letter = match ticket.side {
                Side::Buy => 'C',
                Side::Sell => 'V',
            };
            format!(
                "{letter} {} @ {}",
                fmt_decimal(ticket.volume),
                fmt_decimal(ticket.price_open)
            )
        })
        .collect();
    let mut pnl = format!("aberto {}", fmt_open_pnl(snapshot.total_profit()));
    if let Some(currency) = snapshot.currency() {
        pnl.push(' ');
        pnl.push_str(currency);
    }
    lines.push(pnl);
    lines
}

fn fmt_open_pnl(value: Decimal) -> String {
    let body = fmt_decimal(value.abs());
    if value.is_zero() {
        "0".to_owned()
    } else if value.is_sign_negative() {
        format!("\u{2212}{body}")
    } else {
        format!("+{body}")
    }
}

/// Token the poller may send, or `None` when the overlay must stay dark.
#[must_use]
pub fn resolve_token(settings: &MetaTraderSettings) -> Option<String> {
    let env = std::env::var(TOKEN_ENV).ok();
    let raw = env
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| settings.mcp_token.clone());
    let token = raw.trim();
    if token.is_empty() || settings.mcp_url.trim().is_empty() {
        None
    } else {
        Some(token.to_owned())
    }
}

/// Poller handle plus the Demo/Prod ticket that outlives a dropped session.
pub struct VenueSession {
    pub link: Option<AccountLink>,
    pub ticket: VenueTicket,
}

impl Default for VenueSession {
    fn default() -> Self {
        Self {
            link: None,
            ticket: VenueTicket::load(),
        }
    }
}

/// The UI's handle on the poller: latest snapshot, symbol updates, stop on drop.
pub struct AccountLink {
    snapshot: Option<AccountSnapshot>,
    rx: watch::Receiver<Option<AccountSnapshot>>,
    symbol_tx: watch::Sender<String>,
    cmd_tx: std::sync::mpsc::Sender<TradeCommand>,
    notice_rx: std::sync::mpsc::Receiver<TradeNotice>,
    stop: Arc<AtomicBool>,
}

impl AccountLink {
    /// Latest positions for the overlay, draining any poller write since the last frame.
    pub fn poll(&mut self) -> Option<&PositionSnapshot> {
        self.refresh();
        self.snapshot
            .as_ref()
            .map(|snap| &snap.positions)
            .filter(|snap| !snap.is_empty())
    }

    /// Full latest-wins picture, including a flat book with a live account.
    pub fn view(&mut self) -> Option<&AccountSnapshot> {
        self.refresh();
        self.snapshot.as_ref()
    }

    /// Last drained snapshot without touching the watch — for read-only chrome.
    #[must_use]
    pub fn cached(&self) -> Option<&AccountSnapshot> {
        self.snapshot.as_ref()
    }

    fn refresh(&mut self) {
        if self.rx.has_changed().unwrap_or(false) {
            self.snapshot = self.rx.borrow_and_update().clone();
        }
    }

    pub fn set_symbol(&self, symbol: impl Into<String>) {
        let _ = self.symbol_tx.send(symbol.into());
    }

    pub fn submit(&self, command: TradeCommand) {
        let _ = self.cmd_tx.send(command);
    }

    pub fn drain_notices(&self) -> Vec<TradeNotice> {
        let mut out = Vec::new();
        while let Ok(notice) = self.notice_rx.try_recv() {
            out.push(notice);
        }
        out
    }
}

impl Drop for AccountLink {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Spawn the MCP poller, or `None` when url/token/symbol cannot be asked.
#[must_use]
pub fn spawn(settings: &MetaTraderSettings, symbol: String) -> Option<AccountLink> {
    let url = settings.mcp_url.trim().to_owned();
    let token = resolve_token(settings)?;
    if url.is_empty() || symbol.is_empty() {
        return None;
    }
    let (snap_tx, snap_rx) = watch::channel(None);
    let (symbol_tx, symbol_rx) = watch::channel(symbol);
    let (cmd_tx, cmd_rx) = std::sync::mpsc::channel();
    let (notice_tx, notice_rx) = std::sync::mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_thread = Arc::clone(&stop);
    let name = "mt5-account";
    let started = std::thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    tracing::warn!(
                        target: "quantick::app",
                        schema_version = 1_u8,
                        event_code = "MT5_MCP_RUNTIME",
                        error = %error,
                        "account poller could not start a runtime"
                    );
                    return;
                }
            };
            runtime.block_on(poller::run_poller(
                url,
                token,
                symbol_rx,
                snap_tx,
                cmd_rx,
                notice_tx,
                stop_thread,
            ));
        })
        .ok();
    started.map(|_| AccountLink {
        snapshot: None,
        rx: snap_rx,
        symbol_tx,
        cmd_tx,
        notice_rx,
        stop,
    })
}

/// Paint the badge under the legend and the dashed entry line at VWAP.
pub fn draw(
    painter: &egui::Painter,
    chart_rect: egui::Rect,
    legend: Option<egui::Rect>,
    legend_inset: f32,
    snapshot: &PositionSnapshot,
    scale: &PriceScale,
    axis_x: f32,
) {
    if snapshot.is_empty() {
        return;
    }
    draw_badge(painter, chart_rect, legend, legend_inset, snapshot);
    draw_entry_line(painter, chart_rect, snapshot, scale, axis_x);
}

fn draw_badge(
    painter: &egui::Painter,
    chart_rect: egui::Rect,
    legend: Option<egui::Rect>,
    legend_inset: f32,
    snapshot: &PositionSnapshot,
) {
    let lines = badge_lines(snapshot);
    if lines.is_empty() {
        return;
    }
    let appearance = theme::get(painter.ctx());
    let of = &appearance.resolved.orderflow;
    let background = rgba(of.legend_background);
    let ink = rgba(of.legend_text);
    let font = appearance.label_font(BADGE_FONT_PX);
    let clip = painter.with_clip_rect(chart_rect);
    let galleys: Vec<_> = lines
        .iter()
        .map(|line| clip.layout_no_wrap(line.clone(), font.clone(), ink))
        .collect();
    let width = galleys
        .iter()
        .map(|galley| galley.size().x)
        .fold(0.0_f32, f32::max)
        + BADGE_PAD_X * 2.0;
    let height = galleys.iter().map(|galley| galley.size().y).sum::<f32>()
        + BADGE_PAD_Y * 2.0
        + (galleys.len().saturating_sub(1) as f32);
    let top = match legend {
        Some(rect) => rect.bottom() + BADGE_GAP_PX,
        None => chart_rect.top() + BADGE_MARGIN_PX + legend_inset,
    };
    let panel = egui::Rect::from_min_size(
        egui::pos2(chart_rect.left() + BADGE_MARGIN_PX, top),
        egui::vec2(width, height),
    );
    let rounding = egui::Rounding::same(BADGE_RADIUS_PX);
    clip.rect_filled(panel, rounding, background);
    let mut y = panel.top() + BADGE_PAD_Y;
    for galley in galleys {
        let pos = egui::pos2(panel.left() + BADGE_PAD_X, y);
        y += galley.size().y + 1.0;
        clip.galley(pos, galley, ink);
    }
}

fn draw_entry_line(
    painter: &egui::Painter,
    chart_rect: egui::Rect,
    snapshot: &PositionSnapshot,
    scale: &PriceScale,
    axis_x: f32,
) {
    let Some(price) = snapshot.entry_vwap().and_then(|value| value.to_f64()) else {
        return;
    };
    let y = scale.y(price);
    if y < chart_rect.top() || y > chart_rect.bottom() {
        return;
    }
    let appearance = theme::get(painter.ctx());
    let side = snapshot.net_side().unwrap_or(Side::Buy);
    let color = match side {
        Side::Buy => appearance.buy(),
        Side::Sell => appearance.sell(),
    };
    painter.extend(egui::Shape::dashed_line(
        &[egui::pos2(chart_rect.left(), y), egui::pos2(axis_x, y)],
        egui::Stroke::new(1.0_f32, color.gamma_multiply(0.55)),
        4.0,
        4.0,
    ));
    let text = fmt_decimal(snapshot.entry_vwap().unwrap_or(Decimal::ZERO));
    let ink = crate::paper_chrome::trade_action_ink(&appearance, color);
    crate::pointer_compass::paint_price_tag(painter, axis_x, y, text, None, color, ink);
}

fn rgba([r, g, b, a]: [u8; 4]) -> egui::Color32 {
    egui::Color32::from_rgba_unmultiplied(r, g, b, a)
}

crate::hooks::declare_hooks!["QUANTICK_MT5_MCP_TOKEN"];

#[cfg(test)]
mod tests;
