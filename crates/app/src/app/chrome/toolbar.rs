//! The window side of the toolbar (§6).
//!
//! [`crate::toolbar`] owns the grouping and the overflow rule and returns
//! a [`crate::toolbar::ToolbarAction`]; these three methods build the model
//! it draws from and carry out what it asked. The split is the port: the
//! toolbar module never touches `QuantickApp`, and this file never decides
//! what a toolbar looks like.

use eframe::egui;
use quantick_control_handlers::market_palette;
use quantick_engine::Side;
use quantick_paper::route::RoutePress;
use quantick_session::paper::TradeCommand;

use crate::app::indicator_manager::IndicatorEdit;
use crate::indicator_worker::SlotId;
use crate::tab::layout;
use crate::toolbar::{self, ToolbarAction};

use crate::app::{QuantickApp, TabsMutPort};

// A failed recut still owns older prints after venue paging is exhausted.
// The history button retries those retained pages through its ordinary action.
fn toolbar_caps(
    tab: &crate::tab::Tab,
    config: &crate::config::AppConfig,
) -> crate::config::FeedCapabilities {
    let mut caps = tab.capabilities(config);
    caps.history_paging |= tab.panes().any(|(pane, _)| pane.history_failed());
    caps
}

/// Build the toolbar's model from the app's state, draw it, and carry
/// out whatever it asked (§6 — the toolbar module owns grouping and the
/// overflow rule; this method owns the side effects).
pub(in crate::app) fn draw(app: &mut QuantickApp, ctx: &egui::Context) {
    let chip = app.config.venue_chip(&app.active_tab().feed_id);
    let feed = &app.active_tab().link;
    let replay = feed.replay.as_ref().map(|link| toolbar::ReplaySource {
        label: link.label(),
        hover: format!(
            "Replaying {}\nSide source: {}",
            link.session.path.display(),
            link.session
                .header
                .side_source
                .as_deref()
                .unwrap_or("not recorded"),
        ),
    });
    let capabilities = toolbar_caps(app.active_tab(), &app.config);
    let candles_held = app.active_tab().session.venue_candles_held();
    let older_candles = app.active_tab().older_candles(capabilities);
    let dock_visible = app.surfaces.dock.visible();
    // The History button names this tab's target and run.
    let history = toolbar::HistoryButton {
        main: app.active_tab().session.main_history_reach(),
        status: app.active_tab().session.history_status(),
        tz: app.runtime.tz,
    };

    let capsule = crate::app::chrome::capsule::snapshot(app, ctx);

    let deal_recording = app.active_tab().deal_recording_view();
    #[cfg(any(feature = "scenario-harness", test))]
    let deal_recording_menu =
        deal_recording.is_some() && app.chrome.harness.take_deal_recording_menu();
    #[cfg(not(any(feature = "scenario-harness", test)))]
    let deal_recording_menu = false;
    #[cfg(any(feature = "scenario-harness", test))]
    if app.chrome.harness.take_bars_menu() {
        let popup = crate::pane_header::menu_id(app.focused_pane().id);
        ctx.memory_mut(|memory| memory.open_popup(popup));
    }
    let scope = app.runtime.tabs.windows().presented().0;
    app.surfaces
        .retain_slot_scopes(quantick_ui_model::Slot::Toolbar, |scope| {
            app.runtime
                .tabs
                .windows()
                .window(quantick_workspace::arrangement::windows::WindowId(scope))
                .is_some()
        });
    let notice = app.runtime.broker.sheet().map(|sheet| sheet.notice.clone());
    {
        let tab = app.active_tab_mut();
        tab.paper.venue_armed_route = tab.account.ticket.target.account_mode().is_some();
    }
    let tab = app.active_tab();
    let source_ok = {
        let caps = tab.capabilities(&app.config);
        caps.open_positions && tab.link.replay.is_none()
    };
    let account_info = tab
        .account
        .link
        .as_ref()
        .and_then(crate::account::AccountLink::cached)
        .and_then(|snap| snap.info.clone());
    let venue_close = tab
        .account
        .link
        .as_ref()
        .and_then(crate::account::AccountLink::cached)
        .and_then(|snap| snap.positions.close_label());
    let execution = execution_strip(
        tab.account.ticket.target,
        tab.account.ticket.one_click,
        account_info.as_ref(),
        source_ok,
        venue_close,
    );
    let snapshot = quantick_ui_model::chrome_toolbar::Snapshot {
        capsule: capsule.clone(),
        market: (chip, tab.chip_label().to_owned()),
        replay,
        deal_recording,
        focused_bars: tab.focused_pane().spec.spec(),
        history,
        history_candles: candles_held,
        older_candles,
        capabilities,
        dock_visible,
        paper: tab
            .paper
            .trade_bar(&ctx.format_shortcut(&crate::app::menu_bar::PAPER_REVERSE_SHORTCUT))
            .with_notice(notice),
        execution,
    };
    app.surfaces.update_scoped(
        scope,
        crate::surfaces::HostIntent::ChromeToolbar(Box::new(
            quantick_ui_model::chrome_toolbar::Intent::Refresh(Box::new(snapshot)),
        )),
    );
    app.surfaces.chrome_toolbar.scoped(scope).1.recording_menu = deal_recording_menu;
    app.surfaces.chrome_toolbar.scoped(scope).1.popovers =
        Some(app.surfaces.canvas_popovers.model_state.projection());
    let response = app.surfaces.draw_slot_scoped(
        scope,
        quantick_ui_model::Slot::Toolbar,
        &mut crate::surfaces::ViewContext { ctx, ui: None },
    );
    let view = app.surfaces.chrome_toolbar.scoped(scope).1;
    app.chrome.history_menu_rect = view.history;
    let (capsule_rects, toolbar_door) = (view.capsule, view.door);
    let mut actions = Vec::new();
    for effect in response.model_effects {
        if let quantick_ui_model::HostEffect::ChromeToolbar(effects) = effect {
            for effect in effects.0 {
                use quantick_ui_model::chrome_toolbar::Effect;
                match effect {
                    Effect::Action(ToolbarAction::ToggleLayoutPopover) => {
                        app.surfaces
                            .canvas_popovers
                            .update(quantick_ui_model::canvas_popovers::Intent::ToggleLayout);
                    }
                    Effect::Action(ToolbarAction::ToggleAddMenu) => {
                        app.surfaces
                            .canvas_popovers
                            .update(quantick_ui_model::canvas_popovers::Intent::ToggleAdd);
                    }
                    Effect::Action(action) => actions.push(action),
                }
            }
        }
    }
    let settle = crate::app::chrome::capsule::settle;
    for action in settle(app, ctx, &capsule, capsule_rects, toolbar_door, actions) {
        app.apply_toolbar_action(action);
    }
    crate::account::draw_prod_confirm(ctx, &mut app.active_tab_mut().account);
}

impl QuantickApp {
    /// One trade, sent to the active tab's desk as every surface sends it.
    pub(in crate::app) fn trade(&mut self, command: TradeCommand) {
        self.active_tab_mut().paper.send(command);
    }

    /// One toolbar side effect.
    pub(in crate::app) fn apply_toolbar_action(&mut self, action: ToolbarAction) {
        match action {
            ToolbarAction::LoadHistory(reach) => {
                let active = self.runtime.tabs.active_index();
                self.tabs_mut().press_history(active, reach);
            }
            ToolbarAction::CancelHistory => _ = self.active_tab_mut().cancel_history(),
            ToolbarAction::SwitchMarket => {
                _ = market_palette::show(self as &mut super::super::ControlWindow, "", true);
            }
            ToolbarAction::DealRecording(action) => {
                self.active_tab_mut().apply_deal_recording(action);
            }
            ToolbarAction::LoadOlderCandles => self.tabs_mut().load_older_candles(),
            ToolbarAction::SetLayout(preset) => _ = layout::pick(self.active_tab_mut(), preset),
            // Carried out by `chrome::capsule::settle`, which knows the window.
            ToolbarAction::ToggleWindowMenu
            | ToolbarAction::ToggleLayoutPopover
            | ToolbarAction::ToggleAddMenu
            | ToolbarAction::ToggleWorkspaceMenu
            | ToolbarAction::SaveWorkspace => {}
            ToolbarAction::ToggleDock => self.surfaces.dock.toggle_visible(),
            ToolbarAction::OpenAccountList => {
                self.surfaces.dock.open_tab(crate::dock::DockTab::Trading);
                self.runtime.route(&self.config).press(RoutePress::OpenList);
            }
            // Every indicator command lands on the focused pane (§11), which
            // is the flow pane whenever the canvas is not split.
            // Adding an indicator by hand is the plainest possible request to
            // see one, so it opens a folded legend rather than letting the new
            // row land inside the puck — the trader would get one more dot and
            // no way to tell the add from a no-op. Not the auto-collapse the
            // design ruled out: that rule protects against hiding what nobody
            // asked to hide, and unfolding hides nothing. It lives on this
            // path, the trader's own, and not in `ChartPane::add_indicator`,
            // which the workspace restore and the harness hooks also travel —
            // there it would erase the fold on every launch.
            ToolbarAction::AddNative(id) => {
                crate::app::indicator_manager::IndicatorState::set_legend_collapsed(
                    self.focused_pane_mut(),
                    false,
                );
                let target = (
                    self.runtime.tabs.active_id(),
                    self.active_tab().focused_side(),
                );
                let tabs = &mut self.runtime.tabs;
                let pane = tabs.runtime_mut(tabs.active_index()).pane_mut(target.1);
                let attached = self.runtime.indicators.attach_native(pane, target, id);
                self.indicator_wiring()
                    .apply_indicator_edit(IndicatorEdit::Attached(attached));
            }
            ToolbarAction::ToggleIndicatorHidden(slot) => {
                let mut wiring = self.indicator_wiring();
                let target = wiring.runtime.target_slot(SlotId(slot));
                wiring.apply_indicator_legend_action(
                    target.tab,
                    target.side,
                    crate::indicator_legend::LegendAction::ToggleHidden(target.slot),
                );
            }
            ToolbarAction::RemoveIndicator(slot) => {
                let mut wiring = self.indicator_wiring();
                let target = wiring.runtime.target_slot(SlotId(slot));
                wiring.apply_indicator_edit(IndicatorEdit::Remove(target));
            }
            ToolbarAction::AddScriptIndicator(index) => {
                self.indicator_wiring().add_library_script_at(index);
            }
            ToolbarAction::OpenIndicatorSettings(slot) => {
                let mut wiring = self.indicator_wiring();
                let target = wiring.runtime.target_slot(SlotId(slot));
                wiring.apply_indicator_legend_action(
                    target.tab,
                    target.side,
                    crate::indicator_legend::LegendAction::OpenSettings(target.slot),
                );
            }
            // The toolbar acts on the market it is showing: the active tab's
            // simulator, whose tape the buttons' price came from.
            ToolbarAction::PaperBuy => self.trade(TradeCommand::Market { side: Side::Buy }),
            ToolbarAction::PaperSell => self.trade(TradeCommand::Market { side: Side::Sell }),
            ToolbarAction::PaperClose => self.trade(TradeCommand::Close),
            ToolbarAction::PaperReverse => self.trade(TradeCommand::Reverse),
            ToolbarAction::VenueBuy => self.venue_entry(Side::Buy),
            ToolbarAction::VenueSell => self.venue_entry(Side::Sell),
            ToolbarAction::VenueClose => self.venue_close(),
            ToolbarAction::VenueReverse => self.venue_reverse(),
            ToolbarAction::SetExecutionTarget(target) => self.set_execution_target(target),
            ToolbarAction::SetOneClick(on) => {
                self.active_tab_mut().account.ticket.set_one_click(on);
            }
        }
    }

    fn set_execution_target(&mut self, target: quantick_ui_model::chrome_toolbar::ExecutionTarget) {
        let tab = self.active_tab_mut();
        tab.account.ticket.target = match target {
            quantick_ui_model::chrome_toolbar::ExecutionTarget::Sim => {
                crate::account::ExecutionTarget::Sim
            }
            quantick_ui_model::chrome_toolbar::ExecutionTarget::Prod => {
                crate::account::ExecutionTarget::Prod
            }
            quantick_ui_model::chrome_toolbar::ExecutionTarget::Demo => {
                crate::account::ExecutionTarget::Demo
            }
        };
        tab.paper.venue_armed_route = tab.account.ticket.target.account_mode().is_some();
        if !tab.paper.venue_armed_route {
            tab.paper.desk.armed = None;
            let _ = tab.paper.take_venue_click();
        }
        tab.account.ticket.persist();
    }

    fn venue_source_ok(&self) -> bool {
        let tab = self.active_tab();
        let caps = tab.capabilities(&self.config);
        caps.open_positions && tab.link.replay.is_none()
    }

    fn venue_entry(&mut self, side: Side) {
        let source_ok = self.venue_source_ok();
        let (symbol, volume, order_type, mark, stop_offset, profit_offset) = {
            let tab = self.active_tab();
            let symbol = tab.symbol.clone();
            let form = tab.paper.desk.ticket.form();
            let volume = form.quantity.ok();
            let order_type = tab.paper.desk.ticket.order_type;
            let mark = tab.paper.mark_price();
            let (stop_offset, profit_offset) = form.offsets.unwrap_or((None, None));
            (symbol, volume, order_type, mark, stop_offset, profit_offset)
        };
        let Some(volume) = volume else {
            self.active_tab_mut()
                .account
                .ticket
                .announce("MT5: quantity must be a positive number");
            return;
        };
        match order_type {
            quantick_sim::EntryKind::Limit | quantick_sim::EntryKind::Stop => {
                let tab = self.active_tab_mut();
                tab.paper.venue_armed_route = true;
                tab.paper.desk.armed = Some(quantick_paper::desk::ArmedPlacement {
                    side,
                    kind: order_type,
                });
                tab.account
                    .ticket
                    .set_status("MT5: click the chart at your price · Esc cancels");
            }
            quantick_sim::EntryKind::Market => {
                let tab = self.active_tab_mut();
                crate::account::request_venue_entry(
                    &mut tab.account,
                    source_ok,
                    symbol,
                    side,
                    volume,
                    None,
                    mark,
                    None,
                    stop_offset,
                    profit_offset,
                );
            }
        }
    }

    fn venue_close(&mut self) {
        let source_ok = self.venue_source_ok();
        let symbol = self.active_tab().symbol.clone();
        let tab = self.active_tab_mut();
        crate::account::request_venue_close(&mut tab.account, source_ok, symbol);
    }

    fn venue_reverse(&mut self) {
        let source_ok = self.venue_source_ok();
        let (symbol, mark, stop_offset, profit_offset) = {
            let tab = self.active_tab();
            let form = tab.paper.desk.ticket.form();
            let (stop_offset, profit_offset) = form.offsets.unwrap_or((None, None));
            (
                tab.symbol.clone(),
                tab.paper.mark_price(),
                stop_offset,
                profit_offset,
            )
        };
        let tab = self.active_tab_mut();
        crate::account::request_venue_reverse(
            &mut tab.account,
            source_ok,
            symbol,
            stop_offset,
            profit_offset,
            mark,
        );
    }
}

fn execution_strip(
    target: crate::account::ExecutionTarget,
    one_click: bool,
    info: Option<&crate::account::AccountInfo>,
    source_ok: bool,
    venue_close: Option<String>,
) -> quantick_ui_model::chrome_toolbar::ExecutionStrip {
    let target = target.reconcile(info);
    let to_ui = |target: crate::account::ExecutionTarget| match target {
        crate::account::ExecutionTarget::Sim => {
            quantick_ui_model::chrome_toolbar::ExecutionTarget::Sim
        }
        crate::account::ExecutionTarget::Prod => {
            quantick_ui_model::chrome_toolbar::ExecutionTarget::Prod
        }
        crate::account::ExecutionTarget::Demo => {
            quantick_ui_model::chrome_toolbar::ExecutionTarget::Demo
        }
    };
    let venue_blocked = target.account_mode().and_then(|mode| {
        crate::account::trade_block(source_ok, mode, info).map(|block| block.explanation())
    });
    quantick_ui_model::chrome_toolbar::ExecutionStrip {
        target: to_ui(target),
        targets: crate::account::ExecutionTarget::visible(info)
            .into_iter()
            .map(to_ui)
            .collect(),
        venue_blocked,
        venue_close: target.account_mode().and(venue_close),
        one_click,
    }
}
