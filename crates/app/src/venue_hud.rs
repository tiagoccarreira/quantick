//! Position HUD for the MCP Prod/Demo route — same card as the simulator's
//! [`crate::paper_hud`], wired to venue close/reverse.

use eframe::egui;
use egui_phosphor::regular as icons;
use quantick_engine::Side;
use rust_decimal::prelude::ToPrimitive;

use crate::account::{
    ExecutionTarget, PositionSnapshot, VenueSession, display_currency, request_venue_close,
    request_venue_reverse,
};
use crate::chart::PriceScale;
use crate::paper_chrome::{financial_text, fmt_decimal, points_color, position_word};
use crate::theme;

const HUD_MARGIN_PX: f32 = 8.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EntryDrift {
    OnScreen,
    Above,
    Below,
}

fn entry_drift(entry_y: f32, top: f32, bottom: f32) -> EntryDrift {
    if entry_y < top {
        EntryDrift::Above
    } else if entry_y > bottom {
        EntryDrift::Below
    } else {
        EntryDrift::OnScreen
    }
}

/// Draw the venue position HUD over `chart_rect`. No-op while flat.
pub fn draw(
    ctx: &egui::Context,
    chart_rect: egui::Rect,
    session: &mut VenueSession,
    snapshot: &PositionSnapshot,
    scale: &PriceScale,
    symbol: &str,
    source_ok: bool,
) {
    let palette = theme::get(ctx);
    let Some(entry) = snapshot.entry_vwap() else {
        return;
    };
    let Some(side) = snapshot.net_side() else {
        return;
    };
    let Some(quantity) = snapshot.net_abs_volume() else {
        return;
    };
    let side_color = match side {
        Side::Buy => palette.buy(),
        Side::Sell => palette.sell(),
    };
    let entry_y = scale.y(entry.to_f64().unwrap_or_default());
    let drift = entry_drift(entry_y, chart_rect.top(), chart_rect.bottom());
    let word = position_word(side);
    let opposite = position_word(match side {
        Side::Buy => Side::Sell,
        Side::Sell => Side::Buy,
    });
    let qty = fmt_decimal(quantity);
    let route = match session.ticket.target {
        ExecutionTarget::Demo => "DEMO",
        _ => "PROD",
    };
    let chip = format!("{route} {word} {qty}");
    let profit = snapshot.total_profit();
    let pnl_text = {
        let sign = if profit > rust_decimal::Decimal::ZERO {
            "+"
        } else {
            ""
        };
        match snapshot.currency().map(display_currency) {
            Some(currency) => format!("{sign}{} {currency}", fmt_decimal(profit)),
            None => format!("{sign}{}", fmt_decimal(profit)),
        }
    };
    egui::Area::new(egui::Id::new("venue_position_hud"))
        .fixed_pos(chart_rect.left_top() + egui::vec2(HUD_MARGIN_PX, HUD_MARGIN_PX))
        .order(egui::Order::Middle)
        .show(ctx, |ui| {
            let card = egui::Frame::none()
                .fill(palette.inset())
                .stroke(egui::Stroke::new(1.0_f32, palette.border()))
                .rounding(egui::Rounding::same(4.0))
                .inner_margin(egui::Margin {
                    left: 12.0,
                    right: 10.0,
                    top: 7.0,
                    bottom: 7.0,
                })
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 5.0;
                    ui.horizontal(|ui| {
                        egui::Frame::none()
                            .fill(side_color)
                            .rounding(egui::Rounding::same(2.0))
                            .inner_margin(egui::Margin::symmetric(5.0, 1.0))
                            .show(ui, |ui| {
                                ui.label(
                                    egui::RichText::new(chip)
                                        .color(crate::paper_chrome::trade_action_ink(
                                            &palette, side_color,
                                        ))
                                        .strong()
                                        .small(),
                                );
                            });
                        ui.label(
                            egui::RichText::new(format!("@ {}", fmt_decimal(entry)))
                                .monospace()
                                .color(palette.text_primary()),
                        );
                        ui.label(financial_text(
                            &palette,
                            &pnl_text,
                            palette.mono_font(12.0),
                            points_color(&palette, profit),
                        ))
                        .on_hover_text("open profit from MetaTrader");
                    });
                    match drift {
                        EntryDrift::OnScreen => {}
                        EntryDrift::Above => {
                            ui.label(
                                egui::RichText::new(format!(
                                    "{} entry {} above the view",
                                    icons::CARET_UP,
                                    fmt_decimal(entry),
                                ))
                                .small()
                                .color(palette.text_support()),
                            );
                        }
                        EntryDrift::Below => {
                            ui.label(
                                egui::RichText::new(format!(
                                    "{} entry {} below the view",
                                    icons::CARET_DOWN,
                                    fmt_decimal(entry),
                                ))
                                .small()
                                .color(palette.text_support()),
                            );
                        }
                    }
                    let reverse = format!("{} Reverse", icons::ARROWS_LEFT_RIGHT);
                    ui.horizontal(|ui| {
                        let quiet = |label: &str| {
                            egui::Button::new(
                                egui::RichText::new(label)
                                    .color(palette.text_primary())
                                    .small(),
                            )
                            .fill(palette.control())
                            .stroke(egui::Stroke::new(1.0_f32, palette.border()))
                            .rounding(egui::Rounding::same(3.0))
                        };
                        if ui
                            .add(quiet("× Close"))
                            .on_hover_text(format!("exit the {word} {qty} at market via MT5"))
                            .clicked()
                        {
                            request_venue_close(session, source_ok, symbol.to_owned());
                        }
                        if ui
                            .add(quiet(&reverse))
                            .on_hover_text(format!(
                                "close the {word} {qty} and open {opposite} {qty} at market via MT5"
                            ))
                            .clicked()
                        {
                            request_venue_reverse(
                                session,
                                source_ok,
                                symbol.to_owned(),
                                None,
                                None,
                                None,
                            );
                        }
                    });
                });
            let rect = card.response.rect;
            ui.painter().rect_filled(
                egui::Rect::from_min_max(
                    rect.min + egui::vec2(1.0, 1.0),
                    egui::pos2(rect.min.x + 4.0, rect.max.y - 1.0),
                ),
                egui::Rounding {
                    nw: 3.0,
                    sw: 3.0,
                    ne: 0.0,
                    se: 0.0,
                },
                side_color,
            );
        });
}
