//! Indicator headline and value-range calculations shared by renderers.
use quantick_indicators::{InputSpec, InputValue, Rgba8};

use super::{IndicatorView, format_value};

/// One coloured piece of a pane's title line, left to right.
pub struct HeadingBit {
    /// `Name (period) value`, or just the value's own period when it is not
    /// the first series.
    pub text: String,
    /// The colour that series draws with.
    pub color: Rgba8,
}

/// Vertical padding inside a pane, as a fraction of the value range.
const PANE_PAD_FRAC: f64 = 0.08;

/// The value that sits on a shared window's zero line.
///
/// `0` unless the indicator declares an `Upper` and a `Lower` limit. Then it
/// is the middle of those two, so a 30/70 oscillator lines its 50 up with a
/// series that really crosses zero.
pub fn align_value(view: &IndicatorView) -> f64 {
    match (named_number(view, "Lower"), named_number(view, "Upper")) {
        (Some(lower), Some(upper)) => (lower + upper) / 2.0,
        _ => 0.0,
    }
}

/// `Auto` is on: the pane centres on the limits and keeps `Space` past them.
///
/// Absent on an indicator that does not declare the input, so a script
/// without it keeps the fractional pad.
pub fn centres_scale(view: &IndicatorView) -> bool {
    let Some((index, spec)) = named(view, "Auto") else {
        return false;
    };
    match view.input_values.get(index) {
        Some(InputValue::Bool(value)) => *value,
        _ => matches!(spec, InputSpec::Bool { default: true, .. }),
    }
}

/// Room past each limit, in the indicator's own units. `0` when undeclared.
pub fn scale_space(view: &IndicatorView) -> f64 {
    named_number(view, "Space").unwrap_or(0.0).max(0.0)
}

fn named<'a>(view: &'a IndicatorView, title: &str) -> Option<(usize, &'a InputSpec)> {
    view.descriptor
        .inputs
        .iter()
        .enumerate()
        .find(|(_, spec)| spec.title().eq_ignore_ascii_case(title))
}

fn named_number(view: &IndicatorView, title: &str) -> Option<f64> {
    let (index, spec) = named(view, title)?;
    numeric_input(view, index, spec)
}

fn numeric_input(view: &IndicatorView, index: usize, spec: &InputSpec) -> Option<f64> {
    match view.input_values.get(index) {
        Some(InputValue::Int(value)) => Some(*value as f64),
        Some(InputValue::Float(value)) => Some(*value),
        _ => match spec {
            InputSpec::Int { default, .. } => Some(*default as f64),
            InputSpec::Float { default, .. } => Some(*default),
            _ => None,
        },
    }
}

/// Where a shared window puts value 0, as a fraction of the pane from the top.
///
/// `None` for a window of one: that pane keeps a linear scale. Two or more
/// put 0 on one line — the middle when any of them really crosses it, the
/// bottom when every one stays at or above it, the top when every one stays
/// at or below it. Padding past zero does not count as crossing.
pub fn shared_zero_from_top(ranges: &[(f64, f64)]) -> Option<f32> {
    if ranges.len() < 2 {
        return None;
    }
    let mut above = false;
    let mut below = false;
    for &(lo, hi) in ranges {
        let (neg, pos) = crate::geometry::zero_side_anchors(lo, hi);
        below |= neg > 0.0;
        above |= pos > 0.0;
    }
    if above && below {
        Some(0.5)
    } else if below {
        Some(0.0)
    } else if above {
        Some(1.0)
    } else {
        None
    }
}

/// The value range a pane auto-fits to.
///
/// With `Auto`, the middle of `Upper` and `Lower` is the centre of the pane
/// and `Space` sits beyond each limit. A trace past that still widens the
/// scale, equally, so the middle stays put. Without `Auto`, the visible
/// values get a fraction of their span above and below. `None` while
/// everything visible is still NaN warmup — nothing to scale to, and no axis.
pub fn pane_auto_range(view: &IndicatorView, start: usize, end: usize) -> Option<(f64, f64)> {
    let (lo, hi) = value_range(view, start, end)?;
    if centres_scale(view) {
        return Some(centred_range(view, lo, hi));
    }
    let pad = (hi - lo).max(f64::EPSILON) * PANE_PAD_FRAC;
    Some((lo - pad, hi + pad))
}

fn centred_range(view: &IndicatorView, lo: f64, hi: f64) -> (f64, f64) {
    let space = scale_space(view);
    let Some(center) = named_number(view, "Lower")
        .zip(named_number(view, "Upper"))
        .map(|(lower, upper)| {
            let low = lower.min(upper);
            let high = lower.max(upper);
            (low - space, high + space, (lower + upper) / 2.0)
        })
    else {
        return (lo - space, hi + space);
    };
    let (floor, ceiling, center) = center;
    let lo = lo.min(floor);
    let hi = hi.max(ceiling);
    let half = (center - lo)
        .abs()
        .max((hi - center).abs())
        .max(f64::EPSILON);
    (center - half, center + half)
}

/// The title line: one piece per series that is actually drawn, in plot order.
///
/// The first piece is `name (period) value`, and a series whose title is its
/// own name (`+DI`, `Histogram`) puts that name in front of the value. A
/// numbered reading (`1 line`) does not: the period already names it. A length
/// input titled `1: length` belongs to the plots titled `1 …`. One length
/// belongs to a one-series indicator. Several lengths and the same number of
/// series pair in order. Anything else lists its lengths once, on the first
/// piece.
pub fn heading_bits(view: &IndicatorView) -> Vec<HeadingBit> {
    let series = drawn_series(view);
    if series.is_empty() {
        return Vec::new();
    }
    let periods = assign_periods(&length_inputs(view), &series);
    series
        .into_iter()
        .enumerate()
        .map(|(index, series)| HeadingBit {
            text: heading_text(
                view.label(),
                index,
                &series.title,
                periods[index].as_deref(),
                series.value,
            ),
            color: series.color,
        })
        .collect()
}

fn heading_text(
    label: &str,
    index: usize,
    title: &str,
    period: Option<&str>,
    value: f64,
) -> String {
    let value = format_value(value);
    let name = series_label(title, index, label);
    match (index == 0, period, name) {
        (true, Some(period), Some(name)) => format!("{label} ({period}) {name} {value}"),
        (true, Some(period), None) => format!("{label} ({period}) {value}"),
        (true, None, Some(name)) => format!("{label} {name} {value}"),
        (true, None, None) => format!("{label} {value}"),
        (false, Some(period), Some(name)) => format!("{name} ({period}) {value}"),
        (false, Some(period), None) => format!("({period}) {value}"),
        (false, None, Some(name)) => format!("{name} {value}"),
        (false, None, None) => format!("{value}"),
    }
}

/// The plot title, when it names the series rather than repeating the
/// indicator or numbering a reading (`1 line`).
fn series_label<'a>(title: &'a str, index: usize, indicator: &str) -> Option<&'a str> {
    let first = title.split_whitespace().next().unwrap_or("");
    if first.is_empty() || first.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    if index == 0 && title.eq_ignore_ascii_case(indicator) {
        return None;
    }
    Some(title)
}

struct DrawnSeries {
    key: Option<String>,
    title: String,
    value: f64,
    color: Rgba8,
}

fn drawn_series(view: &IndicatorView) -> Vec<DrawnSeries> {
    view.descriptor
        .plots
        .iter()
        .enumerate()
        .filter_map(|(index, spec)| {
            if spec.marker.as_ref().is_some_and(|marker| {
                marker.location != quantick_indicators::MarkerLocation::Absolute
            }) {
                return None;
            }
            let resolved = view.plot_style(index)?;
            if !resolved.visible {
                return None;
            }
            let value = plot_last(view, index)?;
            let key = spec.title.split_whitespace().next().and_then(|token| {
                (!token.is_empty() && token.bytes().all(|byte| byte.is_ascii_digit()))
                    .then(|| token.to_owned())
            });
            Some(DrawnSeries {
                key,
                title: spec.title.clone(),
                value,
                color: resolved.color,
            })
        })
        .collect()
}

fn plot_last(view: &IndicatorView, index: usize) -> Option<f64> {
    view.preview
        .as_ref()
        .and_then(|frame| frame.values.get(index).copied())
        .filter(|value| !value.is_nan())
        .or_else(|| {
            view.columns
                .get(index)?
                .iter()
                .rev()
                .copied()
                .find(|value| !value.is_nan())
        })
}

struct LengthInput {
    key: Option<String>,
    value: i64,
}

fn length_inputs(view: &IndicatorView) -> Vec<LengthInput> {
    view.descriptor
        .inputs
        .iter()
        .enumerate()
        .filter_map(|(index, spec)| {
            let InputSpec::Int { title, default, .. } = spec else {
                return None;
            };
            if title.eq_ignore_ascii_case("Upper")
                || title.eq_ignore_ascii_case("Lower")
                || title.eq_ignore_ascii_case("Space")
            {
                return None;
            }
            let value = match view.input_values.get(index) {
                Some(InputValue::Int(value)) => *value,
                _ => *default,
            };
            let key = title.split_once(": ").and_then(|(section, _)| {
                (!section.is_empty() && section.bytes().all(|byte| byte.is_ascii_digit()))
                    .then(|| section.to_owned())
            });
            Some(LengthInput { key, value })
        })
        .collect()
}

fn assign_periods(ints: &[LengthInput], series: &[DrawnSeries]) -> Vec<Option<String>> {
    let mut used = vec![false; ints.len()];
    let mut periods = vec![None; series.len()];
    for (slot, series) in series.iter().enumerate() {
        let Some(key) = &series.key else {
            continue;
        };
        let Some(index) = ints
            .iter()
            .enumerate()
            .find(|(index, input)| !used[*index] && input.key.as_deref() == Some(key.as_str()))
            .map(|(index, _)| index)
        else {
            continue;
        };
        used[index] = true;
        periods[slot] = Some(ints[index].value.to_string());
    }
    let open_series: Vec<usize> = series
        .iter()
        .enumerate()
        .filter(|(slot, series)| series.key.is_none() && periods[*slot].is_none())
        .map(|(slot, _)| slot)
        .collect();
    let open_ints: Vec<i64> = ints
        .iter()
        .enumerate()
        .filter(|(index, input)| !used[*index] && input.key.is_none())
        .map(|(_, input)| input.value)
        .collect();
    if !open_series.is_empty() && open_series.len() == open_ints.len() {
        for (slot, value) in open_series.into_iter().zip(open_ints) {
            periods[slot] = Some(value.to_string());
        }
    } else if let Some(first) = open_series.first()
        && !open_ints.is_empty()
    {
        periods[*first] = Some(
            open_ints
                .iter()
                .map(i64::to_string)
                .collect::<Vec<_>>()
                .join(", "),
        );
    }
    periods
}

/// The newest value of the view's first plot (preview first, else the last
/// committed row) — the pane's headline number.
pub fn last_value(view: &IndicatorView) -> Option<f64> {
    let previewed = view
        .preview
        .as_ref()
        .and_then(|frame| frame.values.first().copied())
        .filter(|v| !v.is_nan());
    previewed.or_else(|| {
        view.columns
            .first()
            .and_then(|column| column.iter().rev().find(|v| !v.is_nan()).copied())
    })
}

/// Min/max over every plot's visible cells (preview included); `None` when
/// everything visible is still NaN warmup.
fn value_range(view: &IndicatorView, start: usize, end: usize) -> Option<(f64, f64)> {
    let mut lo = f64::INFINITY;
    let mut hi = f64::NEG_INFINITY;
    for (index, column) in view.columns.iter().enumerate() {
        // A non-absolute marker column stages 1.0 as a flag, not as a value:
        // it anchors to the pane's own edges and is never read as a y. Folding
        // it into the fit collapses a real plot into a sliver — a CVD pane
        // spanning 1e6..1.1e6 becomes 1.0..1.1e6.
        let is_flag_column = view.descriptor.plots.get(index).is_some_and(|spec| {
            spec.marker.as_ref().is_some_and(|marker| {
                marker.location != quantick_indicators::MarkerLocation::Absolute
            })
        });
        if is_flag_column {
            continue;
        }
        let preview = view
            .preview
            .as_ref()
            .and_then(|frame| frame.values.get(index).copied());
        for value in column[start.min(column.len())..end.min(column.len())]
            .iter()
            .copied()
            .chain(preview)
        {
            if value.is_nan() {
                continue;
            }
            lo = lo.min(value);
            hi = hi.max(value);
        }
    }
    (lo.is_finite() && hi.is_finite()).then_some((lo, hi))
}

#[cfg(test)]
#[path = "pane_values_tests.rs"]
mod tests;
