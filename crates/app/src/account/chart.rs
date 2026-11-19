//! Chart overlay for the MCP account: entry / SL / TP lines, the position
//! tag on the entry, and the drag that creates or amends a bracket.

use eframe::egui;
use quantick_engine::Side;
use quantick_paper::desk::geometry::LINE_GRAB_RADIUS_PX;
use rust_decimal::Decimal;
use rust_decimal::prelude::{FromPrimitive, ToPrimitive};

use super::plate::request_venue_bracket;
use super::{OpenTicket, PendingKind, PendingOrder, PositionSnapshot, VenueSession};
use crate::chart::PriceScale;
use crate::paper_chrome::fmt_decimal;
use crate::theme;

const LINE_WIDTH: f32 = 1.5;
const HANDLE_PX: f32 = 28.0;
const HANDLE_H: f32 = 16.0;

/// Which protective leg the pointer is shaping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BracketLeg {
    Stop,
    Target,
}

/// In-flight chart gesture against the MCP position.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum VenueChartDrag {
    None,
    /// Pulling away from the entry to birth a missing leg.
    Create {
        leg: BracketLeg,
        price: Decimal,
    },
    /// Moving an existing SL or TP line.
    Amend {
        leg: BracketLeg,
        price: Decimal,
    },
}

impl Default for VenueChartDrag {
    fn default() -> Self {
        Self::None
    }
}

/// Frame geometry the overlay paints into.
pub struct VenueChartFrame<'a> {
    pub painter: &'a egui::Painter,
    pub chart_rect: egui::Rect,
    pub legend: Option<egui::Rect>,
    pub legend_inset: f32,
    pub scale: &'a PriceScale,
    pub axis_x: f32,
    pub tag_right: f32,
    pub pointer: Option<egui::Pos2>,
    /// When false, paint only — other panes show the same facts without grab.
    pub takes_input: bool,
    /// `PROD` / `DEMO` — same register as the simulator's `SIM` chip.
    pub route_word: &'a str,
    /// Declared / catalog point value for SL/TP money labels; `None` keeps price.
    pub point_value: Option<Decimal>,
    /// Fallback currency when the MCP ticket omits one (catalog / account).
    pub currency_fallback: Option<&'a str>,
}

/// Paint badge, entry (with on-line tag), SL/TP, pending orders; returns whether
/// a grab is under the pointer (for the cursor).
pub fn paint(
    frame: &VenueChartFrame<'_>,
    snapshot: &PositionSnapshot,
    drag: VenueChartDrag,
) -> bool {
    if snapshot.is_empty() {
        return false;
    }
    super::draw_badge(
        frame.painter,
        frame.chart_rect,
        frame.legend,
        frame.legend_inset,
        snapshot,
    );
    let Some(ticket) = snapshot.tickets.first() else {
        return false;
    };
    let mut hovering = false;
    hovering |= paint_position(frame, snapshot, ticket, drag);
    for order in &snapshot.orders {
        hovering |= paint_pending(frame, order);
    }
    hovering
}

fn money_currency<'a>(
    snapshot: &'a PositionSnapshot,
    frame: &VenueChartFrame<'a>,
) -> Option<&'a str> {
    snapshot
        .currency()
        .or(frame.currency_fallback)
        .map(super::display_currency)
}

fn paint_position(
    frame: &VenueChartFrame<'_>,
    snapshot: &PositionSnapshot,
    ticket: &OpenTicket,
    drag: VenueChartDrag,
) -> bool {
    let appearance = theme::get(frame.painter.ctx());
    let side = snapshot.net_side().unwrap_or(ticket.side);
    let color = match side {
        Side::Buy => appearance.buy(),
        Side::Sell => appearance.sell(),
    };
    let Some(entry) = snapshot.entry_vwap() else {
        return false;
    };
    let Some(entry_f) = entry.to_f64() else {
        return false;
    };
    let entry_y = frame.scale.y(entry_f);
    let mut hovering = false;
    if in_range(frame, entry_y) {
        let line_hover = hovers(frame, entry_y);
        hovering |= line_hover;
        level_line(frame, entry_y, color, false, line_hover, false);
        gutter_chip(frame, entry_y, color, &fmt_decimal(entry));
        let volume = snapshot.net_abs_volume().unwrap_or(ticket.volume);
        let word = match side {
            Side::Buy => "LONG",
            Side::Sell => "SHORT",
        };
        // Same chip grammar as the simulator: `SIM LONG 1` → `PROD LONG 1`.
        let side_text = format!("{} {word} {}", frame.route_word, fmt_decimal(volume));
        let profit = snapshot.total_profit();
        let currency = money_currency(snapshot, frame);
        let points = Some((
            format_profit(profit, currency),
            if profit >= Decimal::ZERO {
                appearance.buy()
            } else {
                appearance.sell()
            },
        ));
        let tag = position_tag(frame, entry_y, color, &side_text, points);
        let over_tag = frame.pointer.is_some_and(|p| tag.expand(4.0).contains(p));
        let reveal = line_hover || over_tag;
        hovering |= over_tag;

        let (sl_price, tp_price) = live_brackets(ticket, drag);
        if let Some(price) = sl_price {
            hovering |= paint_leg(
                frame,
                entry,
                volume,
                side,
                price,
                BracketLeg::Stop,
                appearance.sell(),
                currency,
                matches!(
                    drag,
                    VenueChartDrag::Amend {
                        leg: BracketLeg::Stop,
                        ..
                    } | VenueChartDrag::Create {
                        leg: BracketLeg::Stop,
                        ..
                    }
                ),
            );
        } else if reveal && frame.takes_input {
            hovering |= paint_handle(frame, entry_y, BracketLeg::Stop, appearance.sell(), side);
        }
        if let Some(price) = tp_price {
            hovering |= paint_leg(
                frame,
                entry,
                volume,
                side,
                price,
                BracketLeg::Target,
                appearance.buy(),
                currency,
                matches!(
                    drag,
                    VenueChartDrag::Amend {
                        leg: BracketLeg::Target,
                        ..
                    } | VenueChartDrag::Create {
                        leg: BracketLeg::Target,
                        ..
                    }
                ),
            );
        } else if reveal && frame.takes_input {
            hovering |= paint_handle(frame, entry_y, BracketLeg::Target, appearance.buy(), side);
        }
    }
    hovering
}

fn live_brackets(ticket: &OpenTicket, drag: VenueChartDrag) -> (Option<Decimal>, Option<Decimal>) {
    let mut sl = ticket.stop_loss;
    let mut tp = ticket.take_profit;
    match drag {
        VenueChartDrag::None => {}
        VenueChartDrag::Create { leg, price } | VenueChartDrag::Amend { leg, price } => match leg {
            BracketLeg::Stop => sl = Some(price),
            BracketLeg::Target => tp = Some(price),
        },
    }
    (sl, tp)
}

#[allow(clippy::too_many_arguments)]
fn paint_leg(
    frame: &VenueChartFrame<'_>,
    entry: Decimal,
    volume: Decimal,
    side: Side,
    price: Decimal,
    leg: BracketLeg,
    color: egui::Color32,
    currency: Option<&str>,
    dragged: bool,
) -> bool {
    let Some(raw) = price.to_f64() else {
        return false;
    };
    let y = frame.scale.y(raw);
    if !in_range(frame, y) {
        return false;
    }
    let hovered = hovers(frame, y);
    level_line(frame, y, color, false, hovered, dragged);
    gutter_chip(frame, y, color, &fmt_decimal(price));
    // Pill = money (SIM grammar); price stays on the gutter chip.
    let word = match leg {
        BracketLeg::Stop => "SL",
        BracketLeg::Target => "TP",
    };
    let label = match frame.point_value {
        Some(point_value) => {
            let points = quantick_sim::signed_points(side, entry, price, volume);
            let money = points * point_value;
            format!("{word} {}", format_profit(money, currency))
        }
        None => format!("{word} {}", fmt_decimal(price)),
    };
    chip_tag(frame, y, color, &label);
    hovered || dragged
}

fn paint_handle(
    frame: &VenueChartFrame<'_>,
    entry_y: f32,
    leg: BracketLeg,
    color: egui::Color32,
    side: Side,
) -> bool {
    let rect = handle_rect(frame.tag_right, entry_y, leg, side);
    let hover = frame.pointer.is_some_and(|p| rect.contains(p));
    let appearance = theme::get(frame.painter.ctx());
    let fill = if hover {
        color
    } else {
        color.gamma_multiply(0.55)
    };
    let ink = crate::paper_chrome::trade_action_ink(&appearance, fill);
    frame.painter.rect_filled(rect, 3.0, fill);
    let text = match leg {
        BracketLeg::Stop => "SL",
        BracketLeg::Target => "TP",
    };
    frame.painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        text,
        appearance.mono_font(10.0),
        ink,
    );
    hover
}

fn paint_pending(frame: &VenueChartFrame<'_>, order: &PendingOrder) -> bool {
    let appearance = theme::get(frame.painter.ctx());
    // Same colours and short kind as the simulator's resting entry tag.
    let color = match order.side {
        Side::Buy => appearance.buy(),
        Side::Sell => appearance.sell(),
    };
    let Some(raw) = order.price.to_f64() else {
        return false;
    };
    let y = frame.scale.y(raw);
    if !in_range(frame, y) {
        return false;
    }
    let hovered = hovers(frame, y);
    level_line(frame, y, color, true, hovered, false);
    gutter_chip(frame, y, color, &fmt_decimal(order.price));
    let side = match order.side {
        Side::Buy => "BUY",
        Side::Sell => "SELL",
    };
    let kind = match order.kind {
        PendingKind::Limit => "LMT",
        PendingKind::Stop => "STP",
    };
    let label = format!("{side} {kind} {}", fmt_decimal(order.volume));
    chip_tag(frame, y, color, &label);
    let currency = frame.currency_fallback.map(super::display_currency);
    if let Some(sl) = order.stop_loss {
        let _ = paint_leg(
            frame,
            order.price,
            order.volume,
            order.side,
            sl,
            BracketLeg::Stop,
            appearance.sell(),
            currency,
            false,
        );
    }
    if let Some(tp) = order.take_profit {
        let _ = paint_leg(
            frame,
            order.price,
            order.volume,
            order.side,
            tp,
            BracketLeg::Target,
            appearance.buy(),
            currency,
            false,
        );
    }
    hovered
}

/// Pointer input for the MCP position overlay. Returns true when the gesture
/// owns the hand (chart must not pan).
pub fn handle_input(
    session: &mut VenueSession,
    snapshot: &PositionSnapshot,
    symbol: &str,
    source_ok: bool,
    scale: &PriceScale,
    chart: egui::Rect,
    pointer: Option<egui::Pos2>,
    primary_pressed: bool,
    primary_down: bool,
    primary_released: bool,
) -> bool {
    let Some(ticket) = snapshot.tickets.first().cloned() else {
        session.chart_drag = VenueChartDrag::None;
        return false;
    };
    let Some(entry) = snapshot.entry_vwap() else {
        return false;
    };
    let Some(entry_f) = entry.to_f64() else {
        return false;
    };
    let entry_y = scale.y(entry_f);
    let side = snapshot.net_side().unwrap_or(ticket.side);
    let tag_right = chart.right() - 8.0;

    if primary_pressed
        && let Some(pos) = pointer.filter(|p| chart.contains(*p))
        && session.chart_drag == VenueChartDrag::None
    {
        if let Some(leg) = hit_existing_leg(&ticket, scale, pos.y) {
            if let Some(price) = price_at(scale, pos.y) {
                session.chart_drag = VenueChartDrag::Amend { leg, price };
                return true;
            }
        } else if let Some(leg) = hit_handle(tag_right, entry_y, side, pos) {
            if let Some(price) = price_at(scale, pos.y) {
                session.chart_drag = VenueChartDrag::Create { leg, price };
                return true;
            }
        } else if (pos.y - entry_y).abs() <= LINE_GRAB_RADIUS_PX {
            // Drag away from the entry: side of the move picks the leg.
            if let Some(price) = price_at(scale, pos.y) {
                let leg = create_leg_for_side(side, entry, price);
                session.chart_drag = VenueChartDrag::Create { leg, price };
                return true;
            }
        }
    }

    if primary_down
        && let Some(pos) = pointer
        && let Some(price) = price_at(scale, pos.y)
    {
        match session.chart_drag {
            VenueChartDrag::Create { leg, .. } => {
                let price = clamp_leg(side, entry, leg, price);
                session.chart_drag = VenueChartDrag::Create { leg, price };
                return true;
            }
            VenueChartDrag::Amend { leg, .. } => {
                let price = clamp_leg(side, entry, leg, price);
                session.chart_drag = VenueChartDrag::Amend { leg, price };
                return true;
            }
            VenueChartDrag::None => {}
        }
    }

    if primary_released {
        let drag = std::mem::take(&mut session.chart_drag);
        if let VenueChartDrag::Create { leg, price } | VenueChartDrag::Amend { leg, price } = drag {
            let (stop_loss, take_profit) = match leg {
                BracketLeg::Stop => (Some(price), ticket.take_profit),
                BracketLeg::Target => (ticket.stop_loss, Some(price)),
            };
            request_venue_bracket(
                session,
                source_ok,
                ticket.ticket,
                symbol.to_owned(),
                stop_loss,
                take_profit,
            );
            return true;
        }
    }

    !matches!(session.chart_drag, VenueChartDrag::None)
}

fn hit_existing_leg(ticket: &OpenTicket, scale: &PriceScale, y: f32) -> Option<BracketLeg> {
    if let Some(sl) = ticket.stop_loss.and_then(|p| p.to_f64()) {
        if (y - scale.y(sl)).abs() <= LINE_GRAB_RADIUS_PX {
            return Some(BracketLeg::Stop);
        }
    }
    if let Some(tp) = ticket.take_profit.and_then(|p| p.to_f64()) {
        if (y - scale.y(tp)).abs() <= LINE_GRAB_RADIUS_PX {
            return Some(BracketLeg::Target);
        }
    }
    None
}

fn hit_handle(tag_right: f32, entry_y: f32, side: Side, pos: egui::Pos2) -> Option<BracketLeg> {
    for leg in [BracketLeg::Stop, BracketLeg::Target] {
        if handle_rect(tag_right, entry_y, leg, side).contains(pos) {
            return Some(leg);
        }
    }
    None
}

fn handle_rect(tag_right: f32, entry_y: f32, leg: BracketLeg, side: Side) -> egui::Rect {
    // SL handle below entry for long (above for short); TP the opposite.
    let down = matches!(
        (leg, side),
        (BracketLeg::Stop, Side::Buy) | (BracketLeg::Target, Side::Sell)
    );
    let y = if down { entry_y + 22.0 } else { entry_y - 22.0 };
    let x = tag_right - HANDLE_PX - 4.0;
    egui::Rect::from_center_size(egui::pos2(x, y), egui::vec2(HANDLE_PX, HANDLE_H))
}

fn create_leg_for_side(side: Side, entry: Decimal, price: Decimal) -> BracketLeg {
    match side {
        Side::Buy if price < entry => BracketLeg::Stop,
        Side::Buy => BracketLeg::Target,
        Side::Sell if price > entry => BracketLeg::Stop,
        Side::Sell => BracketLeg::Target,
    }
}

fn clamp_leg(side: Side, entry: Decimal, leg: BracketLeg, price: Decimal) -> Decimal {
    match (side, leg) {
        (Side::Buy, BracketLeg::Stop) => price.min(entry),
        (Side::Buy, BracketLeg::Target) => price.max(entry),
        (Side::Sell, BracketLeg::Stop) => price.max(entry),
        (Side::Sell, BracketLeg::Target) => price.min(entry),
    }
}

fn price_at(scale: &PriceScale, y: f32) -> Option<Decimal> {
    Decimal::from_f64(scale.price_at(y)).filter(|value| *value > Decimal::ZERO)
}

fn in_range(frame: &VenueChartFrame<'_>, y: f32) -> bool {
    y >= frame.chart_rect.top() && y <= frame.chart_rect.bottom()
}

fn hovers(frame: &VenueChartFrame<'_>, y: f32) -> bool {
    frame.pointer.is_some_and(|pointer| {
        frame.chart_rect.contains(pointer) && (pointer.y - y).abs() <= LINE_GRAB_RADIUS_PX
    })
}

fn level_line(
    frame: &VenueChartFrame<'_>,
    y: f32,
    color: egui::Color32,
    dashed: bool,
    hovered: bool,
    dragged: bool,
) {
    let width = if dragged || hovered { 2.5 } else { LINE_WIDTH };
    let points = [
        egui::pos2(frame.chart_rect.left(), y),
        egui::pos2(frame.axis_x, y),
    ];
    let stroke = egui::Stroke::new(width, color);
    if dashed {
        frame
            .painter
            .extend(egui::Shape::dashed_line(&points, stroke, 4.0, 4.0));
    } else {
        frame.painter.line_segment(points, stroke);
    }
}

fn gutter_chip(frame: &VenueChartFrame<'_>, y: f32, color: egui::Color32, text: &str) {
    let appearance = theme::get(frame.painter.ctx());
    let ink = crate::paper_chrome::trade_action_ink(&appearance, color);
    crate::pointer_compass::paint_price_tag(
        frame.painter,
        frame.axis_x,
        y,
        text.to_owned(),
        None,
        color,
        ink,
    );
}

fn chip_tag(frame: &VenueChartFrame<'_>, y: f32, fill: egui::Color32, text: &str) {
    let appearance = theme::get(frame.painter.ctx());
    let ink = crate::paper_chrome::trade_action_ink(&appearance, fill);
    let galley = frame
        .painter
        .layout_no_wrap(text.to_owned(), appearance.mono_font(11.0), ink);
    let size = egui::vec2(galley.size().x + 10.0, 18.0);
    let right = frame.tag_right.min(frame.axis_x - 4.0);
    let rect = egui::Rect::from_center_size(
        egui::pos2(
            right - size.x / 2.0,
            y.clamp(
                frame.chart_rect.top() + 10.0,
                frame.chart_rect.bottom() - 10.0,
            ),
        ),
        size,
    );
    frame.painter.rect_filled(rect, 3.0, fill);
    frame
        .painter
        .galley(rect.left_top() + egui::vec2(5.0, 2.0), galley, ink);
}

fn position_tag(
    frame: &VenueChartFrame<'_>,
    y: f32,
    fill: egui::Color32,
    side_text: &str,
    points: Option<(String, egui::Color32)>,
) -> egui::Rect {
    let appearance = theme::get(frame.painter.ctx());
    let ink = crate::paper_chrome::trade_action_ink(&appearance, fill);
    let mut text = side_text.to_owned();
    if let Some((pnl, _)) = &points {
        text = format!("{side_text}  {pnl}");
    }
    let galley = frame
        .painter
        .layout_no_wrap(text, appearance.mono_font(11.0), ink);
    let size = egui::vec2(galley.size().x + 12.0, 18.0);
    let right = frame.tag_right.min(frame.axis_x - 4.0);
    let center_y = y.clamp(
        frame.chart_rect.top() + 10.0,
        frame.chart_rect.bottom() - 10.0,
    );
    let rect = egui::Rect::from_center_size(egui::pos2(right - size.x / 2.0, center_y), size);
    frame.painter.rect_filled(rect, 3.0, fill);
    frame
        .painter
        .galley(rect.left_top() + egui::vec2(6.0, 2.0), galley, ink);
    rect
}

fn format_profit(profit: Decimal, currency: Option<&str>) -> String {
    let sign = if profit > Decimal::ZERO { "+" } else { "" };
    match currency {
        Some(c) => format!("{sign}{} {c}", fmt_decimal(profit)),
        None => format!("{sign}{}", fmt_decimal(profit)),
    }
}

#[cfg(test)]
mod currency_tests {
    #[test]
    fn brl_reads_as_real_on_the_chart() {
        assert_eq!(super::super::display_currency("BRL"), "R$");
        assert_eq!(super::super::display_currency("brl"), "R$");
        assert_eq!(super::super::display_currency("USD"), "USD");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal::Decimal;

    #[test]
    fn long_drag_below_entry_is_stop() {
        let entry = Decimal::from(100);
        assert_eq!(
            create_leg_for_side(Side::Buy, entry, Decimal::from(95)),
            BracketLeg::Stop
        );
        assert_eq!(
            create_leg_for_side(Side::Buy, entry, Decimal::from(105)),
            BracketLeg::Target
        );
    }

    #[test]
    fn clamp_keeps_long_stop_below_entry() {
        let entry = Decimal::from(100);
        assert_eq!(
            clamp_leg(Side::Buy, entry, BracketLeg::Stop, Decimal::from(110)),
            entry
        );
    }
}
