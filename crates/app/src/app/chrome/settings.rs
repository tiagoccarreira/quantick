//! Feed the Settings model its owners' values and carry out what its rows ask.
use crate::config::{NamedMcp, ProviderKind};
use eframe::egui;
use quantick_control_handlers::market_tab::MarketTab;
use quantick_control_handlers::settings::SettleError;
use quantick_control_handlers::settings::{SettingsAct, SettingsOwner, SettingsReadings};
use quantick_control_schema::settings_sections::SettingsSection;
use quantick_ui_model::settings_window::{
    Action, AgentNote, ConnectionChoice, Effects, Facts, Intent, Model,
};
use quantick_ui_model::{HostEffect, Slot};

use crate::app::chart_layers_wiring::LayerWiring;
use crate::app::{deal_recording_wiring as deals, workspace_calls::WorkspaceCalls};
use crate::control::ControlAccess;
use crate::surfaces::{Adapter, ViewContext, settings_window::View};

/// Bring Settings up on `section`, on this window.
pub(in crate::app) fn open(
    menu: &mut crate::app::menu_bar::WindowMenuState,
    settings: &mut Adapter<Model, View>,
    viewport: egui::ViewportId,
    section: SettingsSection,
) {
    menu.windows_viewport = Some(viewport);
    settings.update(Intent::Open(section));
}

/// Draw Settings, which is up, on the window that owns it.
pub(in crate::app) fn draw(desk: &mut WorkspaceCalls, ctx: &egui::Context) {
    let access = desk.control.control_access.as_ref();
    let facts = Box::new(Facts {
        readings: desk.readings(),
        agent: access.map(ControlAccess::panel_facts).unwrap_or_default(),
        agent_note: access.and_then(|gate| AgentNote::of(gate.start_error(), gate.status_note())),
        connections: connection_rows(desk.config),
        bridge_feed: desk.chrome.bridge_feed.clone(),
        mcp_feed: desk.chrome.mcp_feed.clone(),
    });
    desk.surfaces.settings_window.update(Intent::Observe(facts));
    let response =
        (desk.surfaces).draw_slot_unscoped(Slot::Settings, &mut ViewContext { ctx, ui: None });
    for effect in response.model_effects {
        if let HostEffect::SettingsWindow(Effects { action: Some(act) }) = effect {
            settle(desk, ctx, act);
        }
    }
}

/// Carry out what a row asked. The Agent access acts are the window's own: the
/// desk door a connection holds cannot reach them.
pub(in crate::app) fn settle(desk: &mut WorkspaceCalls, ctx: &egui::Context, action: Action) {
    match (action, desk.control.control_access.as_mut()) {
        (Action::Row(act), _) => {
            if let Err(SettleError::Theme(error)) = desk.settle(act) {
                (desk.surfaces.settings_window).update(Intent::ThemeFailed(error));
            }
        }
        (Action::OpenAgentAccess, Some(_)) => {
            (desk.surfaces.access_panel).update(quantick_ui_model::access_panel::Intent::Open);
        }
        (Action::AgentSwitch(on), Some(access)) => access.set_switch(ctx, on),
        (Action::Bridge(id), _) => apply_bridge(desk, &id),
        (Action::Mcp(id), _) => apply_mcp(desk, &id),
        (_, None) => {}
    }
}

/// The MetaTrader feeds, with the address each bridge dials and the MCP it asks.
fn connection_rows(config: &crate::config::AppConfig) -> Vec<ConnectionChoice> {
    config
        .feeds
        .iter()
        .filter(|feed| feed.provider == ProviderKind::MetaTrader)
        .map(|feed| ConnectionChoice {
            id: feed.id.clone(),
            name: feed.name.clone(),
            bridge: config.metatrader.bridge_host_for(&feed.id).to_owned(),
            mcp: mcp_label(&config.metatrader, &feed.id),
        })
        .collect()
}

fn mcp_label(settings: &crate::config::MetaTraderSettings, feed_id: &str) -> String {
    match settings.named_mcp(feed_id) {
        NamedMcp::Own { url, .. } => url.to_owned(),
        NamedMcp::Shared if !settings.mcp_token.trim().is_empty() => settings.mcp_url.clone(),
        NamedMcp::Shared | NamedMcp::Dark => "not configured".to_owned(),
    }
}

/// The feed a new window treats as both bridge and MCP, until Settings says otherwise.
pub(in crate::app) fn initial_connection(
    config: &crate::config::AppConfig,
    feed_id: &str,
) -> String {
    let ids: Vec<&str> = config
        .feeds
        .iter()
        .filter(|feed| feed.provider == ProviderKind::MetaTrader)
        .map(|feed| feed.id.as_str())
        .collect();
    if ids.contains(&feed_id) {
        feed_id.to_owned()
    } else {
        ids.first().copied().unwrap_or("").to_owned()
    }
}

fn apply_bridge(desk: &mut WorkspaceCalls, id: &str) {
    desk.chrome.bridge_feed = id.to_owned();
    desk.surfaces
        .market_palette
        .update(quantick_ui_model::market_palette::Intent::Source(
            id.to_owned(),
        ));
    let symbol = desk.runtime.active_tab().symbol.clone();
    let offers = desk
        .config
        .feed(id)
        .is_some_and(|feed| feed.symbols.iter().any(|symbol_id| symbol_id == &symbol));
    if offers {
        desk.runtime
            .active_tab_mut()
            .switch_market(desk.config, id, &symbol);
    }
}

fn apply_mcp(desk: &mut WorkspaceCalls, id: &str) {
    desk.chrome.mcp_feed = id.to_owned();
    let tab = desk.runtime.active_tab_mut();
    tab.account.set_mcp_feed(id);
    tab.sync_account_poller(desk.config);
}

impl SettingsOwner for WorkspaceCalls<'_> {
    fn readings(&self) -> SettingsReadings {
        let tab = self.runtime.tabs.get(self.runtime.tabs.active_index());
        let feed = tab.map_or(self.config.default_feed.as_str(), |tab| &tab.feed_id);
        SettingsReadings {
            timezone: self.runtime.tz,
            theme: self.runtime.appearance.builtin(),
            show_perf: self.health.show_perf,
            progressive_history: self.runtime.history.progressive_history,
            venue_lead_in: self.runtime.history.venue_lead_in,
            record_deals: (self.config).records_by_default(self.chrome.record_deals, feed),
        }
    }

    fn settle(&mut self, act: SettingsAct) -> Result<(), SettleError> {
        match act {
            SettingsAct::OpenAppearance => {
                let chart_settings = &mut self.surfaces.chart_settings.model_state;
                (LayerWiring::of(self.runtime, chart_settings))
                    .open_settings(crate::surfaces::ChartSettingsTab::Candles);
            }
            SettingsAct::Timezone(tz) => self.runtime.tz = tz,
            SettingsAct::PerfReadings(on) => self.health.show_perf = on,
            SettingsAct::ProgressiveHistory(on) => self.runtime.history.progressive_history = on,
            SettingsAct::VenueLeadIn(on) => self.runtime.history.venue_lead_in = on,
            SettingsAct::RecordDeals(on) => {
                deals::set_default(&mut self.chrome.record_deals, &mut self.runtime.tabs, on);
            }
            SettingsAct::Theme(choice) => {
                let applied = self.runtime.appearance.apply_builtin(choice, true);
                applied.map_err(|error| SettleError::Theme(error.to_string()))?;
                let ctx = self.presentation.context.as_ref();
                crate::app::frame::sync_appearance(ctx, self.runtime, &mut self.drawings.presets);
            }
        }
        Ok(())
    }
}
