//! The Settings window: the sections its table names on the left, the rows of
//! the one on screen on the right. Paints the model's facts and emits intents.
use eframe::egui;
use quantick_appearance::Builtin;
use quantick_control_schema::settings_sections::{SettingRow, SettingsSection};
use quantick_ui_model::UpdateSession;
use quantick_ui_model::settings_window::{
    AgentNote, ConnectionChoice, Control, Effects, Intent, Model, State,
};

use super::{HostEffect, SurfaceEnv, SurfaceResponse, ViewContext};
use crate::theme::{self, choice::record};
use crate::timezone::TzOffset;

const SETTINGS_SIZE_PX: [f32; 2] = [680.0, 400.0];
const SECTION_LIST_WIDTH_PX: f32 = 128.0;
const ROW_TEXT_WIDTH_PX: f32 = 380.0;
const ROW_GAP_PX: f32 = 8.0;
const ROW_HINT_PX: f32 = 12.0;
const DISABLED_DIM: f32 = 0.4;
const SWITCH_SIZE_PX: egui::Vec2 = egui::vec2(34.0, 18.0);
const SWITCH_INSET_PX: f32 = 2.0;

#[derive(Default)]
pub(crate) struct View;
impl super::View<Model> for View {
    fn id(&self) -> &'static str {
        "settings_window"
    }
    /// A frame can ask more than once (a switch flipped as the window closes),
    /// and every ask repaints, so the next frame shows what it changed.
    fn paint_updated(
        &mut self,
        session: &mut UpdateSession<'_, Model>,
        target: &mut ViewContext<'_>,
        _: Option<&SurfaceEnv<'_>>,
    ) {
        let asks = paint_window(session.state(), target.context());
        if !asks.is_empty() {
            target.context().request_repaint();
        }
        for ask in asks {
            session.update(ask);
        }
    }
}

fn paint_window(state: &State, ctx: &egui::Context) -> Vec<Intent> {
    let mut asks = Vec::new();
    let Some(shown) = state.shown() else {
        return asks;
    };
    let mut open = true;
    egui::Window::new("Settings")
        .id(egui::Id::new("window_settings"))
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .fixed_size(SETTINGS_SIZE_PX)
        .default_pos([16.0, super::chrome_title_bar::HEIGHT + 16.0])
        .show(ctx, |ui| {
            ui.horizontal_top(|ui| {
                // The list on the left: the one on screen is filled, as in the prototype.
                ui.vertical(|ui| {
                    ui.set_width(SECTION_LIST_WIDTH_PX);
                    for section in SettingsSection::ALL {
                        let label = ui.selectable_label(shown == section, section.title());
                        if label.clicked() {
                            asks.push(Intent::Show(section));
                        }
                    }
                });
                ui.separator();
                ui.vertical(|ui| {
                    ui.strong(shown.title());
                    ui.add_space(ROW_GAP_PX);
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        if shown == SettingsSection::Connections {
                            draw_connections(state, ui, &mut asks);
                        }
                        for row in shown.rows() {
                            draw_row(state, ui, *row, &mut asks);
                            ui.separator();
                        }
                    });
                });
            });
        });
    if !open {
        asks.push(Intent::Close);
    }
    asks
}

pub(crate) fn effects(effects: Effects) -> SurfaceResponse {
    let model_effects =
        (effects != Effects::default()).then_some(HostEffect::SettingsWindow(effects));
    SurfaceResponse {
        model_effects: model_effects.into_iter().collect(),
    }
}

/// The bridge the asset list loads, and the MCP the account asks.
fn draw_connections(state: &State, ui: &mut egui::Ui, intent: &mut Vec<Intent>) {
    let palette = theme::get(ui.ctx());
    let connections = &state.facts().connections;
    ui.label(
        egui::RichText::new(
            "Pick the Quantick bridge and the MCP once. The asset list then loads a symbol on that bridge.",
        )
        .size(ROW_HINT_PX)
        .color(palette.text_muted()),
    );
    ui.add_space(ROW_GAP_PX);
    if connections.is_empty() {
        ui.label("No MetaTrader connection is configured.");
        return;
    }
    ui.label("Quantick bridge");
    pick(ui, "settings_bridge", &state.facts().bridge_feed, connections, |id| {
        intent.push(Intent::PickBridge(id));
    });
    if let Some(chosen) = connections.iter().find(|row| row.id == state.facts().bridge_feed) {
        ui.label(faint_line(palette.text_muted(), &chosen.bridge));
    }
    ui.add_space(ROW_GAP_PX);
    ui.label("MCP");
    pick(ui, "settings_mcp", &state.facts().mcp_feed, connections, |id| {
        intent.push(Intent::PickMcp(id));
    });
    if let Some(chosen) = connections.iter().find(|row| row.id == state.facts().mcp_feed) {
        ui.label(faint_line(palette.text_muted(), &chosen.mcp));
    }
}

fn faint_line(color: egui::Color32, text: &str) -> egui::RichText {
    egui::RichText::new(text).size(ROW_HINT_PX).color(color)
}

fn pick(
    ui: &mut egui::Ui,
    id: &str,
    current: &str,
    connections: &[ConnectionChoice],
    mut chosen: impl FnMut(String),
) {
    let shown = connections
        .iter()
        .find(|row| row.id == current)
        .map(|row| row.name.as_str())
        .unwrap_or(current);
    let mut next = current.to_owned();
    egui::ComboBox::from_id_salt(id)
        .selected_text(shown)
        .show_ui(ui, |ui| {
            for row in connections {
                ui.selectable_value(&mut next, row.id.clone(), &row.name);
            }
        });
    if next != current {
        chosen(next);
    }
}

/// A row's name and line on the left, what it does on the right.
fn draw_row(state: &State, ui: &mut egui::Ui, row: SettingRow, intent: &mut Vec<Intent>) {
    let palette = theme::get(ui.ctx());
    ui.horizontal_top(|ui| {
        ui.vertical(|ui| {
            ui.set_width(ROW_TEXT_WIDTH_PX);
            ui.label(row.label());
            ui.label(
                egui::RichText::new(row.description())
                    .size(ROW_HINT_PX)
                    .color(palette.text_muted()),
            );
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
            row_control(state, ui, row, intent);
        });
    });
    let note = match row {
        SettingRow::AgentSwitch => state.facts().agent_note.as_ref().map(|note| match note {
            AgentNote::Error(error) => (palette.warn(), error.as_str()),
            AgentNote::Status(status) => (palette.text_muted(), status.as_str()),
        }),
        _ => None,
    };
    if let Some((colour, words)) = note {
        ui.colored_label(colour, words);
    }
}

fn row_control(state: &State, ui: &mut egui::Ui, row: SettingRow, intent: &mut Vec<Intent>) {
    match state.control(row) {
        control @ (Control::Button | Control::Appearance(_)) => {
            if ui.button(row.label()).clicked() {
                intent.push(Intent::Press(row));
            }
            if let Control::Appearance(current) = control {
                theme_menu(ui, state, current, intent);
            }
        }
        Control::Timezone(current) => {
            let mut tz = current;
            egui::ComboBox::from_id_salt("settings_timezone")
                .selected_text(tz.label())
                .show_ui(ui, |ui| {
                    for offset in TzOffset::ALL {
                        ui.selectable_value(&mut tz, offset, offset.label());
                    }
                });
            if tz != current {
                intent.push(Intent::PickTimezone(tz));
            }
        }
        Control::Switch { mut on, enabled } => {
            let mut response = switch(ui, &mut on, enabled, row);
            if row == SettingRow::AgentSwitch {
                response = response.on_hover_text(state.facts().agent.menu_label());
            }
            if response.changed() {
                intent.push(Intent::Flip(row, on));
            }
        }
    }
}

/// The bundled themes; a pick asks the appearance owner to apply it.
fn theme_menu(ui: &mut egui::Ui, state: &State, current: Option<Builtin>, intent: &mut Vec<Intent>) {
    let menu = ui.menu_button("Theme", |ui| {
        if state.theme_landed() {
            ui.close_menu();
            intent.push(Intent::ThemeMenuClosed);
        }
        if current.is_none() {
            ui.label("Custom theme");
        }
        for choice in Builtin::ALL {
            let response = ui.selectable_label(current == Some(choice), choice.label());
            record(&response, choice.label());
            if response.clicked() {
                intent.push(Intent::PickTheme(choice));
            }
        }
        if let Some(error) = state.theme_error() {
            ui.colored_label(theme::get(ui.ctx()).warn(), error);
        }
    });
    record(&menu.response, "Theme");
}

/// The prototype's switch, painted because egui has none.
fn switch(ui: &mut egui::Ui, on: &mut bool, enabled: bool, row: SettingRow) -> egui::Response {
    let palette = theme::get(ui.ctx());
    let sense = if enabled {
        egui::Sense::click()
    } else {
        egui::Sense::hover()
    };
    let (rect, mut response) = ui.allocate_exact_size(SWITCH_SIZE_PX, sense);
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    let value = *on;
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Checkbox, enabled, value, row.label())
    });
    // An off track in `border` is 1.5:1 on Light, under 3:1 for a control.
    let track = if value {
        palette.accent()
    } else {
        palette.text_faint()
    };
    let knob = palette.inset();
    let (track, knob) = if enabled {
        (track, knob)
    } else {
        (
            track.gamma_multiply(DISABLED_DIM),
            knob.gamma_multiply(DISABLED_DIM),
        )
    };
    let radius = rect.height() / 2.0;
    let centre = if value {
        rect.right_center() - egui::vec2(radius, 0.0)
    } else {
        rect.left_center() + egui::vec2(radius, 0.0)
    };
    ui.painter().rect_filled(rect, radius, track);
    ui.painter()
        .circle_filled(centre, radius - SWITCH_INSET_PX, knob);
    response
}
