//! The indicator settings dialog, generated from [`InputSpec`]s.
//!
//! Every widget the user sees maps to exactly one spec variant — the crate
//! declares, the app renders, nothing is hand-tailored per indicator. A
//! numeric input that declares both `minval` and `maxval` renders as a
//! slider; dragging any widget previews the draft live through the worker
//! (construct anew → replace → replay, the same path Apply takes), so a
//! running indicator never observes a value changing mid-stream — and the
//! dialog stays open, because tuning is a nudge-and-look loop (audit M2).
//! Apply is still the only commit: a preview never touches the state file,
//! and Close reverts the chart to the last committed values.

use eframe::egui;
use quantick_indicators::{InputSpec, InputValue, PlotSpec, Rgba8, SourceId};

use crate::indicator_style::{
    LineKind, MAX_PLOT_WIDTH_PX, MIN_PLOT_WIDTH_PX, ResolvedPlot, StyleOverride,
};
use crate::theme;

/// Drag sensitivity of a float input that declares no `step`.
const DEFAULT_FLOAT_DRAG_SPEED: f64 = 0.1;
/// Width of a numeric input with no declared range, in pixels. Wide enough to
/// look like a box you can type into and to hold a five-figure value.
const NUMBER_FIELD_WIDTH_PX: f32 = 72.0;
/// Size of the line preview at the end of a Style row, in pixels: long enough
/// to read a width from, one text line tall so the row does not grow.
const PLOT_PREVIEW_SIZE: egui::Vec2 = egui::vec2(64.0, 16.0);
/// How much of its colour a switched-off plot's preview keeps — enough to
/// still be a line, faded enough to read as off, matching the legend dot.
const PREVIEW_OFF_FADE: f32 = 0.4;
/// Width the preview draws a switched-off plot at: a hairline, because the
/// number beside it describes a line nobody is drawing.
const PREVIEW_OFF_WIDTH_PX: f32 = 1.0;
/// Drag sensitivity of a plot's width, in pixels per pointer pixel. The whole
/// usable range is eight pixels wide, so a 1:1 drag would cross it in a flick.
const PLOT_WIDTH_DRAG_SPEED: f64 = 0.02;

/// Width of the preset-name field, in pixels — room for a MAX_NAME_LEN-ish
/// name without pushing the save button off the row.
const PRESET_NAME_FIELD_WIDTH_PX: f32 = 110.0;

use quantick_ui_model::UpdateSession;
use quantick_ui_model::indicator_settings::{
    DEFAULT_PRESET, Intent, Model, SettingsOutcome, row_order,
};
pub(crate) use quantick_ui_model::indicator_settings::{
    DISPLAY_SECTION, SettingsTab, split_section,
};

/// Paint native widgets against the registered owner's readonly projection.
/// The caller settles the final outcome after all native precedence is known.
pub(crate) fn draw(
    ctx: &egui::Context,
    session: &mut UpdateSession<'_, Model>,
    specs: &[InputSpec],
    plots: &[PlotSpec],
    style: &mut StyleOverride,
    presets: Option<&[String]>,
    mates: Option<&[WindowMate]>,
) -> Intent {
    let palette = theme::get(ctx);
    let mut outcome = SettingsOutcome::Open;
    let mut open = true;
    let mut changed = false;
    let mut style_changed = false;
    let dialog = session.state().dialog().expect("resolved editor");
    let previewed = dialog.previewed;
    let slot_id = dialog.target.slot.0;
    let has_style = !plots.is_empty();
    let window = egui::Window::new(format!("Settings — {}", dialog.title));
    crate::popup::centred(window, egui::Id::new(("indicator-settings", slot_id)), ctx)
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .show(ctx, |ui| {
            if has_style {
                ui.horizontal(|ui| {
                    for tab in [SettingsTab::Inputs, SettingsTab::Style] {
                        let label = match tab {
                            SettingsTab::Inputs => "Inputs",
                            SettingsTab::Style => "Style",
                        };
                        if ui
                            .selectable_label(session.state().dialog().unwrap().tab == tab, label)
                            .clicked()
                        {
                            session.update(Intent::SelectTab(tab));
                        }
                    }
                });
                ui.separator();
            }
            if session.state().dialog().unwrap().tab == SettingsTab::Style {
                style_changed = draw_style(ui, plots, style);
                draw_buttons(ui, previewed, &mut outcome);
                return;
            }
            if let Some(names) = presets {
                let dialog = session.state().dialog().unwrap();
                let mut name = dialog.preset_name_draft.clone();
                preset_row(
                    ui,
                    slot_id,
                    names,
                    &dialog.preset_label,
                    &mut name,
                    &mut outcome,
                );
                if name != session.state().dialog().unwrap().preset_name_draft {
                    session.update(Intent::EditPresetName(name));
                }
                ui.separator();
            }
            if let Some(mates) = mates {
                if let Some(join) = window_row(ui, slot_id, mates) {
                    outcome = SettingsOutcome::JoinWindow(join);
                }
                ui.separator();
            }
            let order = row_order(specs);
            egui::Grid::new("indicator-settings-grid")
                .num_columns(2)
                .spacing([12.0, 4.0])
                .show(ui, |ui| {
                    let mut current_section: Option<&str> = None;
                    let mut cursor = 0;
                    while cursor < order.len() {
                        let index = order[cursor];
                        let (section, label) = split_section(specs[index].title());
                        if let Some(space) = auto_space_pair(specs, &order, cursor) {
                            changed |= edit_labeled(ui, session, specs, index, "Auto", true);
                            changed |= edit_labeled(ui, session, specs, space, "Space", false);
                            ui.end_row();
                            cursor += 2;
                            continue;
                        }
                        if reading_index(section) {
                            let start = cursor;
                            cursor += 1;
                            while cursor < order.len()
                                && split_section(specs[order[cursor]].title()).0 == section
                            {
                                cursor += 1;
                            }
                            changed |=
                                draw_reading(ui, session, specs, &order[start..cursor], section);
                            continue;
                        }
                        if !section.is_empty() && current_section != Some(section) {
                            current_section = Some(section);
                            ui.label(
                                egui::RichText::new(section)
                                    .strong()
                                    .color(palette.text_muted()),
                            );
                            ui.label("");
                            ui.end_row();
                        }
                        changed |= edit_input(ui, session, specs, index, label);
                        ui.end_row();
                        cursor += 1;
                    }
                });
            if specs.is_empty() {
                ui.label(
                    egui::RichText::new("this indicator declares no settings")
                        .color(palette.text_muted()),
                );
            }
            draw_buttons(ui, previewed, &mut outcome);
        });
    Intent::FinishPaint {
        changed,
        style_changed,
        outcome,
        window_closed: !open,
        // Read after fields have consumed Escape to cancel their own edits.
        escape: ctx.input(|input| input.key_pressed(egui::Key::Escape)),
    }
}

/// The button row, shared by both tabs so the way out of the dialog is in the
/// same place whichever half you are on.
fn draw_buttons(ui: &mut egui::Ui, previewed: bool, outcome: &mut SettingsOutcome) {
    let palette = theme::get(ui.ctx());
    ui.separator();
    ui.horizontal(|ui| {
        if ui
            .button("Apply")
            .on_hover_text("apply and keep tuning - the dialog stays open")
            .clicked()
        {
            *outcome = SettingsOutcome::Apply;
        }
        // While a preview is pending the same button destroys work, and its
        // name must say so (trader-ux review).
        let (close_label, close_hover) = if previewed {
            (
                "Discard",
                "close and revert the chart to the applied values",
            )
        } else {
            ("Close", "close without applying the current edits")
        };
        if ui.button(close_label).on_hover_text(close_hover).clicked() {
            *outcome = SettingsOutcome::Close;
        }
        // Honesty about the gap between chart and disk: while a preview is
        // live, the chart shows values Apply has not committed yet.
        if previewed {
            ui.label(
                egui::RichText::new("previewing — Apply keeps, Discard reverts")
                    .color(palette.text_muted()),
            );
        }
    });
}

/// The Style tab: one row per declared plot — draw it, its colour, its width,
/// its line type, and a preview of the stroke those produce.
///
/// Writes straight through to the app-side [`StyleOverride`] rather than into
/// a draft. Style needs no draft and no Apply: it is render-side, it costs a
/// repaint rather than a rebuild and a replay, and the chart already shows the
/// answer. Returns whether anything moved, which is all the caller needs to
/// know to write the state file.
fn draw_style(ui: &mut egui::Ui, plots: &[PlotSpec], style: &mut StyleOverride) -> bool {
    let mut changed = false;
    egui::Grid::new("indicator-style-grid")
        .num_columns(5)
        .spacing([10.0, 8.0])
        .show(ui, |ui| {
            for (index, spec) in plots.iter().enumerate() {
                let resolved = style.resolve(index, spec, None);
                let mut over = style.get(index);

                let mut visible = resolved.visible;
                if ui.checkbox(&mut visible, &spec.title).changed() {
                    // Recorded only when it differs from the declaration, so
                    // ticking a box back on leaves no override behind.
                    over.visible = (!visible).then_some(false);
                    changed = true;
                }

                let mut rgba = [
                    resolved.color.r,
                    resolved.color.g,
                    resolved.color.b,
                    resolved.color.a,
                ];
                if ui.color_edit_button_srgba_unmultiplied(&mut rgba).changed() {
                    over.color = Some(Rgba8::new(rgba[0], rgba[1], rgba[2], rgba[3]));
                    changed = true;
                }

                let mut width = resolved.width;
                if ui
                    .add(
                        egui::DragValue::new(&mut width)
                            .range(MIN_PLOT_WIDTH_PX..=MAX_PLOT_WIDTH_PX)
                            .speed(PLOT_WIDTH_DRAG_SPEED)
                            .suffix(" px"),
                    )
                    .on_hover_text("line width")
                    .changed()
                {
                    over.width = Some(width);
                    changed = true;
                }

                if matches!(
                    spec.style,
                    quantick_indicators::PlotStyle::Line
                        | quantick_indicators::PlotStyle::StepLine
                        | quantick_indicators::PlotStyle::Area
                ) {
                    if let Some(kind) = line_kind_combo(ui, index, resolved.line) {
                        over.line = (kind != LineKind::Solid).then_some(kind);
                        changed = true;
                    }
                } else {
                    ui.label("");
                }

                // What the controls beside it add up to. A swatch alone
                // says nothing about width, and a number alone says nothing
                // about how thin 0.5 px looks over candles.
                style.set(index, over);
                draw_plot_preview(ui, style.resolve(index, spec, None), visible);
                ui.end_row();
            }
        });
    // The way back to the defaults: the surface's ink for a plot (its colour as
    // it arrives here), else its declared colour. Disabled with nothing to undo.
    ui.add_space(4.0);
    if ui
        .add_enabled(!style.is_default(), egui::Button::new("Reset style"))
        .on_hover_text(
            "back to the default colours (the theme's, else the script's), widths and line types",
        )
        .on_disabled_hover_text("already showing the defaults")
        .clicked()
    {
        style.clear();
        changed = true;
    }
    changed
}

/// A short line in the plot's resolved colour and width — greyed to a hairline
/// when the plot is switched off, because a preview of a line nobody is drawing
/// has to look like one.
fn draw_plot_preview(ui: &mut egui::Ui, resolved: ResolvedPlot, visible: bool) {
    let palette = theme::get(ui.ctx());
    let (rect, _) = ui.allocate_exact_size(PLOT_PREVIEW_SIZE, egui::Sense::hover());
    let color = egui::Color32::from_rgba_unmultiplied(
        resolved.color.r,
        resolved.color.g,
        resolved.color.b,
        resolved.color.a,
    );
    let (color, width) = if visible {
        (color, resolved.width)
    } else {
        (
            palette.text_muted().gamma_multiply(PREVIEW_OFF_FADE),
            PREVIEW_OFF_WIDTH_PX,
        )
    };
    let y = rect.center().y;
    let points = [egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)];
    let stroke = egui::Stroke::new(width, color);
    match resolved.line.pattern(width) {
        None => {
            ui.painter().line_segment(points, stroke);
        }
        Some((dash, gap)) => {
            for shape in egui::Shape::dashed_line(&points, stroke, dash, gap) {
                ui.painter().add(shape);
            }
        }
    }
}

/// Solid, dashed or dotted. `None` when the row was not touched.
fn line_kind_combo(ui: &mut egui::Ui, index: usize, current: LineKind) -> Option<LineKind> {
    let mut picked = None;
    egui::ComboBox::from_id_salt(("plot-line", index))
        .selected_text(current.label())
        .width(78.0)
        .show_ui(ui, |ui| {
            for kind in [LineKind::Solid, LineKind::Dashed, LineKind::Dotted] {
                if ui
                    .selectable_label(kind == current, kind.label())
                    .on_hover_text("line type")
                    .clicked()
                {
                    picked = Some(kind);
                }
            }
        });
    picked
}

/// A numeric input with no declared range, drawn as a *field* rather than a
/// bare number.
///
/// egui's `DragValue` has always been typeable — click it and it becomes a text
/// box — but nothing said so, so it read as a number you could only nudge, and
/// nudging a period from 9 to 200 is not a thing anyone will do. A fixed width
/// gives it the shape of a field and the hover says the rest, bounds included:
/// a trader who types 500 into a field capped at 100 deserves to know that
/// before the value silently clamps. (A spec with *both* bounds is a slider
/// instead, and carries its own hover.)
fn number_field(
    ui: &mut egui::Ui,
    drag: egui::DragValue<'_>,
    bounds: Option<String>,
) -> egui::Response {
    let response = ui.add_sized([NUMBER_FIELD_WIDTH_PX, ui.spacing().interact_size.y], drag);
    match bounds {
        Some(bounds) => response.on_hover_text(format!("click to type, or drag  ({bounds})")),
        None => response.on_hover_text("click to type, or drag"),
    }
}

/// The half-open bound a numeric spec declares, spelled for a tooltip. `None`
/// when it declares neither — an empty parenthesis is chrome saying nothing.
fn range_hint<T: Copy>(
    min: Option<T>,
    max: Option<T>,
    show: impl Fn(T) -> String,
) -> Option<String> {
    match (min, max) {
        (Some(lo), Some(hi)) => Some(format!("{} to {}", show(lo), show(hi))),
        (Some(lo), None) => Some(format!("{} or more", show(lo))),
        (None, Some(hi)) => Some(format!("up to {}", show(hi))),
        (None, None) => None,
    }
}

/// A numeric input the spec closed to a fixed set: a dropdown of exactly those
/// values, which is the only control that cannot produce one outside it.
fn number_options<T: Copy + PartialEq>(
    ui: &mut egui::Ui,
    salt: (&str, &str),
    current: &mut T,
    options: &[T],
    show: impl Fn(T) -> String,
) -> bool {
    let mut changed = false;
    egui::ComboBox::from_id_salt(salt)
        .selected_text(show(*current))
        .show_ui(ui, |ui| {
            for option in options {
                changed |= ui
                    .selectable_value(current, *option, show(*option))
                    .clicked();
            }
        });
    changed
}

/// The preset picker: "Default" (the declared defaults) plus every saved
/// setup for this indicator's kind, a name field and a save button.
/// Picking one is a *preview*, exactly like dragging a slider — Apply is
/// still the only commit, so browsing presets mid-session is free.
fn preset_row(
    ui: &mut egui::Ui,
    slot_id: u64,
    names: &[String],
    preset_label: &Option<String>,
    preset_name_draft: &mut String,
    outcome: &mut SettingsOutcome,
) {
    let palette = theme::get(ui.ctx());
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new("Preset")
                .strong()
                .color(palette.text_muted()),
        );
        egui::ComboBox::from_id_salt(("indicator-preset", slot_id))
            .selected_text(preset_label.as_deref().unwrap_or("custom"))
            .show_ui(ui, |ui| {
                if ui
                    .selectable_label(
                        preset_label.as_deref() == Some(DEFAULT_PRESET),
                        DEFAULT_PRESET,
                    )
                    .on_hover_text("the script's declared defaults")
                    .clicked()
                {
                    *outcome = SettingsOutcome::LoadPreset(None);
                }
                for name in names {
                    let row = ui
                        .selectable_label(preset_label.as_deref() == Some(name.as_str()), name)
                        .on_hover_text("click previews; right-click deletes");
                    if row.clicked() {
                        *outcome = SettingsOutcome::LoadPreset(Some(name.clone()));
                    }
                    if row.secondary_clicked() {
                        *outcome = SettingsOutcome::DeletePreset(name.clone());
                    }
                }
            });
        ui.add(
            egui::TextEdit::singleline(preset_name_draft)
                .desired_width(PRESET_NAME_FIELD_WIDTH_PX)
                .hint_text("preset name"),
        );
        let name_ok = !preset_name_draft.trim().is_empty();
        if ui
            .add_enabled(name_ok, egui::Button::new("save"))
            .on_hover_text("save the current values under this name")
            .clicked()
        {
            *outcome = SettingsOutcome::SavePreset(preset_name_draft.trim().to_owned());
        }
    });
}

/// Another pane indicator this one can share a window with.
pub(crate) struct WindowMate {
    pub slot: u64,
    pub label: String,
    /// Already drawn in this indicator's window.
    pub shared: bool,
}

/// `Some(None)` is "own window". `Some(Some(slot))` joins that indicator.
/// `None` means the row was not touched.
fn window_row(ui: &mut egui::Ui, slot_id: u64, mates: &[WindowMate]) -> Option<Option<u64>> {
    let palette = theme::get(ui.ctx());
    let mut choice = None;
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new("Window")
                .strong()
                .color(palette.text_muted()),
        );
        let selected = mates
            .iter()
            .find(|mate| mate.shared)
            .map(|mate| format!("With {}", mate.label))
            .unwrap_or_else(|| "Own window".to_owned());
        egui::ComboBox::from_id_salt(("indicator-window", slot_id))
            .selected_text(selected)
            .show_ui(ui, |ui| {
                if ui
                    .selectable_label(mates.iter().all(|mate| !mate.shared), "Own window")
                    .on_hover_text("a pane of its own")
                    .clicked()
                {
                    choice = Some(None);
                }
                for mate in mates {
                    if ui
                        .selectable_label(mate.shared, format!("With {}", mate.label))
                        .on_hover_text(
                            "same pane, one zero line; each indicator keeps its own scale, the axis shows the first",
                        )
                        .clicked()
                    {
                        choice = Some(Some(mate.slot));
                    }
                }
            });
    });
    choice
}

/// `Auto` followed by `Space` is one row: the checkbox in front of the margin.
fn auto_space_pair(specs: &[InputSpec], order: &[usize], cursor: usize) -> Option<usize> {
    let next = *order.get(cursor + 1)?;
    let auto = matches!(&specs[order[cursor]], InputSpec::Bool { title, .. } if title.eq_ignore_ascii_case("Auto"));
    let space = matches!(
        &specs[next],
        InputSpec::Int { title, .. } | InputSpec::Float { title, .. }
            if title.eq_ignore_ascii_case("Space")
    );
    (auto && space).then_some(next)
}

/// A section whose name is only a reading number (`1`, `2`, …). Those share
/// one line for their checkboxes and keep the number in the gutter, instead
/// of spending a row on the number alone.
fn reading_index(section: &str) -> bool {
    !section.is_empty() && section.bytes().all(|b| b.is_ascii_digit())
}

/// Width of the reading number, in pixels. The rows under it leave the same
/// gap so their labels line up with the checkboxes, not with the number.
const READING_GUTTER_PX: f32 = 18.0;

fn reading_gutter(ui: &mut egui::Ui, number: &str) {
    let height = ui.spacing().interact_size.y;
    if number.is_empty() {
        ui.allocate_exact_size(egui::vec2(READING_GUTTER_PX, height), egui::Sense::hover());
        return;
    }
    let palette = theme::get(ui.ctx());
    ui.add_sized(
        [READING_GUTTER_PX, height],
        egui::Label::new(
            egui::RichText::new(number)
                .strong()
                .color(palette.text_muted()),
        ),
    );
}

/// One numbered reading: the number sits on the checkbox line, every checkbox
/// of that reading shares the line, and each other input keeps a line of its
/// own under the same indent.
fn draw_reading(
    ui: &mut egui::Ui,
    session: &mut UpdateSession<'_, Model>,
    specs: &[InputSpec],
    indices: &[usize],
    number: &str,
) -> bool {
    let mut toggles = Vec::new();
    let mut rest = Vec::new();
    for &index in indices {
        let label = split_section(specs[index].title()).1;
        if matches!(specs[index], InputSpec::Bool { .. }) {
            toggles.push((index, label));
        } else {
            rest.push((index, label));
        }
    }
    let mut changed = false;
    if toggles.is_empty() {
        if let Some((index, label)) = rest.first().copied() {
            reading_gutter(ui, number);
            changed |= edit_labeled(ui, session, specs, index, label, false);
            ui.end_row();
            for (index, label) in rest.into_iter().skip(1) {
                reading_gutter(ui, "");
                changed |= edit_labeled(ui, session, specs, index, label, false);
                ui.end_row();
            }
        }
        return changed;
    }
    reading_gutter(ui, number);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 16.0;
        for (index, label) in toggles {
            changed |= edit_labeled(ui, session, specs, index, label, true);
        }
    });
    ui.end_row();
    for (index, label) in rest {
        reading_gutter(ui, "");
        changed |= edit_labeled(ui, session, specs, index, label, false);
        ui.end_row();
    }
    changed
}

/// An ordinary settings row: the label and the control are the grid's two columns.
fn edit_input(
    ui: &mut egui::Ui,
    session: &mut UpdateSession<'_, Model>,
    specs: &[InputSpec],
    index: usize,
    label: &str,
) -> bool {
    let (mut value, applied) = {
        let dialog = session.state().dialog().expect("resolved editor");
        (dialog.draft[index].clone(), dialog.committed[index].clone())
    };
    let changed = input_row(ui, &specs[index], &mut value, &applied, label, false);
    if value != session.state().dialog().unwrap().draft[index] {
        session.update(Intent::EditInput { index, value });
    }
    changed
}

/// Draws one input and writes a changed draft back. `beside_bool` puts a
/// checkbox's words on the box itself so several of them fit one line. A
/// reading's other inputs pass `false` from inside one grid cell, so the
/// label and the control share that cell instead of taking two columns.
fn edit_labeled(
    ui: &mut egui::Ui,
    session: &mut UpdateSession<'_, Model>,
    specs: &[InputSpec],
    index: usize,
    label: &str,
    beside_bool: bool,
) -> bool {
    let (mut value, applied) = {
        let dialog = session.state().dialog().expect("resolved editor");
        (dialog.draft[index].clone(), dialog.committed[index].clone())
    };
    let changed = if beside_bool {
        ui.horizontal(|ui| input_row(ui, &specs[index], &mut value, &applied, label, true))
            .inner
    } else if matches!(specs[index], InputSpec::Bool { .. }) {
        input_row(ui, &specs[index], &mut value, &applied, label, false)
    } else {
        ui.horizontal(|ui| input_row(ui, &specs[index], &mut value, &applied, label, false))
            .inner
    };
    if value != session.state().dialog().unwrap().draft[index] {
        session.update(Intent::EditInput { index, value });
    }
    changed
}

/// One label + widget row. The value enum always matches its spec variant
/// (both come from the same declaration); a mismatch draws nothing rather
/// than panicking a frame. Returns whether the value changed this frame —
/// the signal the caller turns into a live preview. Free text is the one
/// widget that never reports a change: its intermediate states ("E", "EM")
/// are not values anyone asked to run. `applied` is the last committed
/// value: double-clicking a slider snaps back to it — the per-parameter
/// undo a live preview owes the hand that slipped. `label` is the title
/// with its section prefix stripped; widget ids keep using the full title,
/// which is the stable name. `beside_bool` draws a checkbox as one control
/// (`show` next to the box) so a reading can put its toggles on one line.
fn input_row(
    ui: &mut egui::Ui,
    spec: &InputSpec,
    value: &mut InputValue,
    applied: &InputValue,
    label: &str,
    beside_bool: bool,
) -> bool {
    let palette = theme::get(ui.ctx());
    match (spec, value) {
        (
            InputSpec::Int {
                min,
                max,
                step,
                options,
                title,
                ..
            },
            InputValue::Int(current),
        ) => {
            ui.label(label);
            // A declared option list is a *closed* set. Drawing a free field or
            // a slider over one let the dialog produce a number the indicator
            // does not take, which the build then silently replaced with
            // something else — a value shown as bound that never was.
            if !options.is_empty() {
                return number_options(ui, ("input-int", title), current, options, |v: i64| {
                    v.to_string()
                });
            }
            // A declared range on both ends is the author saying "this axis
            // is worth sweeping" — render it as a slider. A half-open range
            // has no lower/upper anchor to draw, so it stays a DragValue.
            if let (Some(lo), Some(hi)) = (min, max) {
                let mut slider = egui::Slider::new(current, *lo..=*hi);
                if let Some(step) = step {
                    slider = slider.step_by(*step as f64);
                }
                let response = ui
                    .add(slider)
                    .on_hover_text("double-click: back to applied");
                if response.double_clicked()
                    && let InputValue::Int(applied) = applied
                {
                    *current = *applied;
                    return true;
                }
                response.changed()
            } else {
                let mut drag = egui::DragValue::new(current);
                if let Some(lo) = min {
                    drag = drag.range(*lo..=i64::MAX);
                } else if let Some(hi) = max {
                    drag = drag.range(i64::MIN..=*hi);
                }
                if let Some(step) = step {
                    drag = drag.speed(*step as f64);
                }
                number_field(ui, drag, range_hint(*min, *max, |v: i64| v.to_string())).changed()
            }
        }
        (
            InputSpec::Float {
                min,
                max,
                step,
                options,
                title,
                ..
            },
            InputValue::Float(current),
        ) => {
            ui.label(label);
            if !options.is_empty() {
                return number_options(ui, ("input-float", title), current, options, |v: f64| {
                    format!("{v}")
                });
            }
            if let (Some(lo), Some(hi)) = (min, max) {
                let mut slider = egui::Slider::new(current, *lo..=*hi);
                if let Some(step) = step {
                    slider = slider.step_by(*step);
                }
                let response = ui
                    .add(slider)
                    .on_hover_text("double-click: back to applied");
                if response.double_clicked()
                    && let InputValue::Float(applied) = applied
                {
                    *current = *applied;
                    return true;
                }
                response.changed()
            } else {
                let mut drag = egui::DragValue::new(current);
                if let Some(lo) = min {
                    drag = drag.range(*lo..=f64::MAX);
                } else if let Some(hi) = max {
                    drag = drag.range(f64::MIN..=*hi);
                }
                drag = drag.speed(step.unwrap_or(DEFAULT_FLOAT_DRAG_SPEED));
                number_field(ui, drag, range_hint(*min, *max, |v: f64| format!("{v}"))).changed()
            }
        }
        (InputSpec::Bool { title, .. }, InputValue::Bool(current)) => {
            if beside_bool {
                ui.push_id(title, |ui| ui.checkbox(current, label))
                    .inner
                    .changed()
            } else {
                ui.label(label);
                ui.checkbox(current, "").changed()
            }
        }
        (InputSpec::Color { .. }, InputValue::Color(current)) => {
            ui.label(label);
            let mut rgba = [current.r, current.g, current.b, current.a];
            if ui.color_edit_button_srgba_unmultiplied(&mut rgba).changed() {
                *current = Rgba8::new(rgba[0], rgba[1], rgba[2], rgba[3]);
                true
            } else {
                false
            }
        }
        (InputSpec::Str { title, options, .. }, InputValue::Str(current)) => {
            ui.label(label);
            if options.is_empty() {
                ui.text_edit_singleline(current);
                false
            } else {
                let mut changed = false;
                egui::ComboBox::from_id_salt(("input-str", title))
                    .selected_text(current.clone())
                    .show_ui(ui, |ui| {
                        for option in options {
                            changed |= ui
                                .selectable_value(current, option.clone(), option)
                                .changed();
                        }
                    });
                changed
            }
        }
        (InputSpec::Source { title, .. }, InputValue::Source(current)) => {
            ui.label(label);
            let mut changed = false;
            egui::ComboBox::from_id_salt(("input-source", title))
                .selected_text(current.as_str())
                .show_ui(ui, |ui| {
                    for source in SourceId::ALL {
                        changed |= ui
                            .selectable_value(current, source, source.as_str())
                            .changed();
                    }
                });
            changed
        }
        // Spec/value variant mismatch: impossible through the worker's
        // plumbing; drawing nothing beats panicking the frame.
        _ => {
            ui.label(egui::RichText::new("(unrenderable input)").color(palette.text_muted()));
            ui.label("");
            false
        }
    }
}

#[cfg(test)]
mod tests;
