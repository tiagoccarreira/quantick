//! CONTA plate on the Trading dock: account, SL/TP, pending cancel, Prod confirm.
//!
//! Buy/Sell live on the toolbar with the Prod|Sim selector.

use eframe::egui;
use quantick_engine::Side;
use rust_decimal::Decimal;

use super::{
    AccountInfo, AccountLink, AccountMode, ExecutionTarget, OpenTicket, PendingAction, PendingKind,
    PendingOrder, PendingTrade, TradeCommand, VenueSession, allowed_command, close_command,
    trade_block,
};
use crate::paper_chrome::{caption, fmt_decimal};
use crate::theme;

/// What the Trading tab needs to draw and arm the live ticket.
pub struct VenuePlateEnv<'a> {
    pub session: &'a mut VenueSession,
    pub symbol: &'a str,
    pub source_ok: bool,
}

pub(crate) fn draw_conta_plate(ui: &mut egui::Ui, env: &mut VenuePlateEnv<'_>) {
    let snapshot = env
        .session
        .link
        .as_mut()
        .and_then(AccountLink::view)
        .cloned();
    let info = snapshot.as_ref().and_then(|snap| snap.info.clone());
    env.session.ticket.reconcile(info.as_ref());
    let tickets = snapshot
        .as_ref()
        .map(|snap| snap.positions.tickets.as_slice())
        .unwrap_or(&[]);
    let orders = snapshot
        .as_ref()
        .map(|snap| snap.positions.orders.as_slice())
        .unwrap_or(&[]);
    seed_bracket_fields(env, tickets);
    let block = close_block(env.source_ok, info.as_ref());
    let palette = theme::get(ui.ctx());
    let edge = palette.draw_violet();

    egui::Frame::none()
        .stroke(egui::Stroke::new(1.0_f32, edge.gamma_multiply(0.55)))
        .rounding(egui::Rounding::same(6.0_f32))
        .inner_margin(egui::Margin::same(8.0_f32))
        .show(ui, |ui| {
            ui.label(caption(&palette, "CONTA").color(edge));
            ui.label(
                egui::RichText::new(account_line(info.as_ref()))
                    .color(palette.text_primary())
                    .strong(),
            );
            ui.label(
                egui::RichText::new(format!("toolbar · {}", env.session.ticket.target.label()))
                    .color(palette.text_muted())
                    .small(),
            );
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("pending @").small());
                ui.add(
                    egui::TextEdit::singleline(&mut env.session.ticket.pending_price)
                        .desired_width(72.0)
                        .hint_text("price"),
                );
            });
            ui.label(
                egui::RichText::new("limit/stop Buy/Sell use this price when Prod/Demo")
                    .color(palette.text_muted())
                    .small(),
            );
            if let Some(first) = tickets.first() {
                draw_bracket_row(ui, env, first, info.as_ref(), &block);
            }
            if !tickets.is_empty() {
                ui.add_space(4.0);
                let close = close_command(
                    &tickets
                        .iter()
                        .map(|ticket| ticket.ticket)
                        .collect::<Vec<_>>(),
                    env.symbol.to_owned(),
                );
                let enabled = close.is_some() && block.is_none();
                let response = ui.add_enabled(enabled, egui::Button::new("Fechar"));
                if let Some(reason) = block.as_ref() {
                    response
                        .clone()
                        .on_disabled_hover_text(reason.explanation());
                }
                if response.clicked()
                    && let Some(TradeCommand::Close { tickets, symbol }) = close
                {
                    request_action(
                        env.session,
                        PendingAction::Close { tickets, symbol },
                        block.clone(),
                    );
                }
            }
            if !orders.is_empty() {
                ui.add_space(6.0);
                ui.label(caption(&palette, "PENDING").color(edge));
                for order in orders {
                    draw_pending_row(ui, env, order, info.as_ref(), &block);
                }
            }
            if let Some(reason) = block.as_ref() {
                ui.label(
                    egui::RichText::new(reason.explanation())
                        .color(palette.warn())
                        .small(),
                );
            }
            if let Some(status) = env.session.ticket.status.as_deref() {
                ui.label(egui::RichText::new(status).color(palette.warn()).small());
            }
        });
}

fn seed_bracket_fields(env: &mut VenuePlateEnv<'_>, tickets: &[OpenTicket]) {
    let Some(first) = tickets.first() else {
        return;
    };
    if env.session.ticket.sl_text.is_empty()
        && let Some(sl) = first.stop_loss
    {
        env.session.ticket.sl_text = fmt_decimal(sl);
    }
    if env.session.ticket.tp_text.is_empty()
        && let Some(tp) = first.take_profit
    {
        env.session.ticket.tp_text = fmt_decimal(tp);
    }
}

fn draw_bracket_row(
    ui: &mut egui::Ui,
    env: &mut VenuePlateEnv<'_>,
    first: &OpenTicket,
    info: Option<&AccountInfo>,
    block: &Option<super::TradeBlock>,
) {
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("SL").small());
        ui.add(
            egui::TextEdit::singleline(&mut env.session.ticket.sl_text)
                .desired_width(64.0)
                .hint_text("stop"),
        );
        ui.label(egui::RichText::new("TP").small());
        ui.add(
            egui::TextEdit::singleline(&mut env.session.ticket.tp_text)
                .desired_width(64.0)
                .hint_text("target"),
        );
        let enabled = block.is_none();
        let response = ui.add_enabled(enabled, egui::Button::new("SL/TP"));
        if let Some(reason) = block.as_ref() {
            response
                .clone()
                .on_disabled_hover_text(reason.explanation());
        }
        if response.clicked() {
            let stop_loss = super::VenueTicket::parsed_level(&env.session.ticket.sl_text);
            let take_profit = super::VenueTicket::parsed_level(&env.session.ticket.tp_text);
            let action_block = venue_block(env.source_ok, env.session.ticket.target, info);
            request_action(
                env.session,
                PendingAction::ModifySlTp {
                    position_ticket: first.ticket,
                    symbol: env.symbol.to_owned(),
                    stop_loss,
                    take_profit,
                },
                action_block,
            );
        }
    });
}

fn draw_pending_row(
    ui: &mut egui::Ui,
    env: &mut VenuePlateEnv<'_>,
    order: &PendingOrder,
    info: Option<&AccountInfo>,
    block: &Option<super::TradeBlock>,
) {
    ui.horizontal(|ui| {
        let letter = match order.side {
            Side::Buy => 'C',
            Side::Sell => 'V',
        };
        ui.label(format!(
            "#{ticket} {letter} {} {} @ {}",
            order.kind.label(),
            fmt_decimal(order.volume),
            fmt_decimal(order.price),
            ticket = order.ticket,
        ));
        let enabled = block.is_none();
        let response = ui.add_enabled(enabled, egui::Button::new("×"));
        if let Some(reason) = block.as_ref() {
            response
                .clone()
                .on_disabled_hover_text(reason.explanation());
        }
        if response.clicked() {
            let action_block = venue_block(env.source_ok, env.session.ticket.target, info);
            request_action(
                env.session,
                PendingAction::DeleteOrder {
                    order_ticket: order.ticket,
                    symbol: env.symbol.to_owned(),
                },
                action_block,
            );
        }
    });
}

/// Arm a market or pending send from the toolbar / chart (Prod/Demo).
///
/// `click_price` is the chart arm price; when absent, pending falls back to
/// the CONTA `pending @` box.
#[allow(clippy::too_many_arguments)]
pub(crate) fn request_venue_entry(
    session: &mut VenueSession,
    source_ok: bool,
    symbol: String,
    side: Side,
    volume: Decimal,
    kind: Option<PendingKind>,
    mark: Option<Decimal>,
    click_price: Option<Decimal>,
    stop_offset: Option<Decimal>,
    profit_offset: Option<Decimal>,
) {
    let info = session
        .link
        .as_mut()
        .and_then(AccountLink::view)
        .and_then(|snap| snap.info.clone());
    session.ticket.reconcile(info.as_ref());
    let Some(mode) = session.ticket.target.account_mode() else {
        session
            .ticket
            .announce("MT5: switch the toolbar to Prod (or Demo) to send");
        return;
    };
    let block = trade_block(source_ok, mode, info.as_ref());
    let pending_price = click_price.or_else(|| session.ticket.parsed_pending_price());
    let reference = match kind {
        None => mark,
        Some(_) => pending_price,
    };
    let (stop_loss, take_profit) = match reference {
        Some(price) => absolute_bracket(side, price, stop_offset, profit_offset),
        None => (None, None),
    };
    let action = match kind {
        None => PendingAction::Market {
            side,
            volume,
            symbol,
            stop_loss,
            take_profit,
        },
        Some(kind) => {
            let Some(price) = pending_price else {
                session
                    .ticket
                    .announce("MT5: click a price on the chart (or type pending @ on CONTA)");
                return;
            };
            PendingAction::Pending {
                side,
                kind,
                volume,
                price,
                symbol,
                stop_loss,
                take_profit,
            }
        }
    };
    request_action(session, action, block);
}

/// Close every open ticket on the chart symbol (toolbar ×).
/// Chart (or plate) ask to set SL/TP on an open ticket.
pub(crate) fn request_venue_bracket(
    session: &mut VenueSession,
    source_ok: bool,
    position_ticket: u64,
    symbol: String,
    stop_loss: Option<Decimal>,
    take_profit: Option<Decimal>,
) {
    let info = session
        .link
        .as_ref()
        .and_then(AccountLink::cached)
        .and_then(|snap| snap.info.clone());
    let block = venue_block(source_ok, session.ticket.target, info.as_ref());
    request_action(
        session,
        PendingAction::ModifySlTp {
            position_ticket,
            symbol,
            stop_loss,
            take_profit,
        },
        block,
    );
}

pub(crate) fn request_venue_close(session: &mut VenueSession, source_ok: bool, symbol: String) {
    let info = session
        .link
        .as_mut()
        .and_then(AccountLink::view)
        .and_then(|snap| snap.info.clone());
    session.ticket.reconcile(info.as_ref());
    let tickets = session
        .link
        .as_mut()
        .and_then(AccountLink::view)
        .map(|snap| {
            snap.positions
                .tickets
                .iter()
                .map(|ticket| ticket.ticket)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let Some(command) = close_command(&tickets, symbol) else {
        session.ticket.announce("MT5: no open position to close");
        return;
    };
    let TradeCommand::Close { tickets, symbol } = command else {
        return;
    };
    let block = venue_block(source_ok, session.ticket.target, info.as_ref());
    request_action(session, PendingAction::Close { tickets, symbol }, block);
}

/// Close the open tickets and open the other way at market (toolbar Reverse).
pub(crate) fn request_venue_reverse(
    session: &mut VenueSession,
    source_ok: bool,
    symbol: String,
    stop_offset: Option<Decimal>,
    profit_offset: Option<Decimal>,
    mark: Option<Decimal>,
) {
    let snapshot = session.link.as_mut().and_then(AccountLink::view).cloned();
    let info = snapshot.as_ref().and_then(|snap| snap.info.clone());
    session.ticket.reconcile(info.as_ref());
    let positions = snapshot
        .as_ref()
        .map(|snap| &snap.positions)
        .cloned()
        .unwrap_or_default();
    let Some(side) = positions.net_side() else {
        session.ticket.announce("MT5: no open position to reverse");
        return;
    };
    let Some(volume) = positions.net_abs_volume() else {
        session.ticket.announce("MT5: no open position to reverse");
        return;
    };
    let tickets: Vec<u64> = positions
        .tickets
        .iter()
        .map(|ticket| ticket.ticket)
        .collect();
    if tickets.is_empty() {
        session.ticket.announce("MT5: no open position to reverse");
        return;
    }
    let flip = match side {
        Side::Buy => Side::Sell,
        Side::Sell => Side::Buy,
    };
    let (stop_loss, take_profit) = match mark {
        Some(price) => absolute_bracket(flip, price, stop_offset, profit_offset),
        None => (None, None),
    };
    let block = venue_block(source_ok, session.ticket.target, info.as_ref());
    request_action(
        session,
        PendingAction::Reverse {
            tickets,
            symbol,
            side: flip,
            volume,
            stop_loss,
            take_profit,
        },
        block,
    );
}

/// Absolute SL/TP around `reference` from the paper ticket's point offsets.
#[must_use]
pub fn absolute_bracket(
    side: Side,
    reference: Decimal,
    stop_offset: Option<Decimal>,
    profit_offset: Option<Decimal>,
) -> (Option<Decimal>, Option<Decimal>) {
    match side {
        Side::Buy => (
            stop_offset.map(|offset| reference.saturating_sub(offset)),
            profit_offset.map(|offset| reference.saturating_add(offset)),
        ),
        Side::Sell => (
            stop_offset.map(|offset| reference.saturating_add(offset)),
            profit_offset.map(|offset| reference.saturating_sub(offset)),
        ),
    }
}

/// Prod confirm window — drawn from the chrome so a toolbar Buy still sees it
/// when the Trading dock is closed.
pub(crate) fn draw_prod_confirm(ctx: &egui::Context, session: &mut VenueSession) {
    let info = session
        .link
        .as_mut()
        .and_then(AccountLink::view)
        .and_then(|snap| snap.info.clone());
    draw_confirm(ctx, session, info.as_ref());
}

fn account_line(info: Option<&AccountInfo>) -> String {
    info.map(AccountInfo::caption)
        .unwrap_or_else(|| "aguardando a conta".into())
}

fn close_block(source_ok: bool, info: Option<&AccountInfo>) -> Option<super::TradeBlock> {
    let mode = info.map(|info| info.mode).unwrap_or(AccountMode::Prod);
    trade_block(source_ok, mode, info)
}

fn venue_block(
    source_ok: bool,
    target: ExecutionTarget,
    info: Option<&AccountInfo>,
) -> Option<super::TradeBlock> {
    let Some(mode) = target.account_mode() else {
        return Some(super::TradeBlock::NoAccount);
    };
    trade_block(source_ok, mode, info)
}

fn request_action(
    session: &mut VenueSession,
    action: PendingAction,
    block: Option<super::TradeBlock>,
) {
    let needs_confirm = matches!(
        (session.ticket.target, &action),
        (
            ExecutionTarget::Prod,
            PendingAction::Market { .. }
                | PendingAction::Pending { .. }
                | PendingAction::ModifySlTp { .. }
                | PendingAction::DeleteOrder { .. }
                | PendingAction::Close { .. }
                | PendingAction::Reverse { .. }
        )
    );
    match allowed_command(block, action.clone()) {
        Err(block) => {
            session
                .ticket
                .announce(format!("MT5: {}", block.explanation()));
        }
        Ok(_) if needs_confirm => {
            session.ticket.set_status("MT5: confirm Prod to send");
            session.ticket.pending = Some(PendingTrade { action });
        }
        Ok(command) => submit_or_explain(session, command),
    }
}

fn submit_or_explain(session: &mut VenueSession, command: TradeCommand) {
    let Some(link) = session.link.as_ref() else {
        session
            .ticket
            .announce("MT5: account poller is not running");
        return;
    };
    session.ticket.set_status("MT5: sending…");
    link.submit(command);
}

fn draw_confirm(ctx: &egui::Context, session: &mut VenueSession, info: Option<&AccountInfo>) {
    let Some(pending) = session.ticket.pending.clone() else {
        return;
    };
    let mut open = true;
    egui::Window::new("Confirmar Prod")
        .collapsible(false)
        .resizable(false)
        .open(&mut open)
        .show(ctx, |ui| {
            for line in confirm_lines(info, &pending.action) {
                ui.label(line);
            }
            ui.horizontal(|ui| {
                if ui.button("Cancelar").clicked() {
                    session.ticket.pending = None;
                }
                if ui.button("Enviar").clicked() {
                    submit_or_explain(session, pending.action.clone().into_command());
                    session.ticket.pending = None;
                }
            });
        });
    if !open {
        session.ticket.pending = None;
    }
}

fn confirm_lines(info: Option<&AccountInfo>, action: &PendingAction) -> Vec<String> {
    let mut lines = Vec::new();
    if let Some(info) = info {
        lines.push(format!("servidor {}", info.server));
        lines.push(format!("login {}", info.login));
        lines.push(format!("tipo {}", info.mode.connected_word()));
    }
    lines.push(action.summary());
    lines
}
