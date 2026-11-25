use super::*;
use quantick_indicator_session::SlotId;
use quantick_indicators::{
    IndicatorDescriptor, InputSpec, InputValue, PlotId, PlotSpec, PlotStyle, PreviewFrame, Rgba8,
};

fn marker_view(columns: Vec<Vec<f64>>, marker_at: usize) -> IndicatorView {
    let mut v = view(columns, None);
    v.descriptor.plots[marker_at].marker = Some(quantick_indicators::MarkerSpec {
        shape: quantick_indicators::MarkerShape::Circle,
        location: quantick_indicators::MarkerLocation::AboveBar,
        text: None,
    });
    v
}

fn view(columns: Vec<Vec<f64>>, preview: Option<Vec<f64>>) -> IndicatorView {
    IndicatorView {
        slot: SlotId(0),
        kind: std::sync::Arc::from("test.indicator"),
        ordinal: 0,
        style: crate::indicator_style::StyleOverride::default(),
        label: std::sync::Arc::from("test"),
        descriptor: IndicatorDescriptor {
            title: "test".to_owned(),
            short_title: None,
            overlay: false,
            plots: (0..columns.len())
                .map(|i| PlotSpec {
                    id: PlotId::new(i),
                    title: format!("p{i}"),
                    style: PlotStyle::Line,
                    base_color: Rgba8::opaque(255, 255, 255),
                    width: 1.0,
                    offset: 0,
                    marker: None,
                })
                .collect(),
            fills: Vec::new(),
            inputs: Vec::new(),
        },
        rows: columns.first().map_or(0, Vec::len),
        columns,
        bar_paints: Vec::new(),
        preview: preview.map(PreviewFrame::new),
        lane: Vec::new(),
        objects: quantick_indicators::ObjectSnapshot::default(),
        input_values: Vec::new(),
        stale: None,
        error: None,
        hidden: false,
        mouse_vertical_line: false,
        scale: crate::price_view::PriceView::new(),
        sizing: crate::indicator_views::PaneSizing::Auto,
        inks: Vec::new(),
        last_auto: None,
        window: 0,
    }
}

#[test]
fn value_range_ignores_warmup_and_reports_none_when_all_of_it_is() {
    let all_nan = view(vec![vec![f64::NAN, f64::NAN]], None);
    assert!(
        value_range(&all_nan, 0, 2).is_none(),
        "a pane with nothing computed yet has no range to fit"
    );

    let mixed = view(vec![vec![f64::NAN, 3.0, -1.0, f64::NAN]], None);
    assert_eq!(value_range(&mixed, 0, 4), Some((-1.0, 3.0)));

    // The forming bar's value belongs to the fit; the window does not
    // clip it away.
    let with_preview = view(vec![vec![1.0, 2.0]], Some(vec![9.0]));
    assert_eq!(value_range(&with_preview, 0, 2), Some((1.0, 9.0)));
}

#[test]
fn a_marker_column_is_a_flag_and_never_scales_the_pane() {
    // The staged 1.0 marks "the condition fired here"; it is never used
    // as a y, because a non-absolute marker anchors to the pane edges.
    let mixed = marker_view(vec![vec![1.0, 1.0], vec![1.0e6, 1.1e6]], 0);
    assert_eq!(
        value_range(&mixed, 0, 2),
        Some((1.0e6, 1.1e6)),
        "the real plot keeps the pane"
    );
}

#[test]
fn last_value_prefers_the_preview_and_skips_trailing_warmup() {
    let previewed = view(vec![vec![1.0, 2.0]], Some(vec![7.5]));
    assert_eq!(last_value(&previewed), Some(7.5));

    // A NaN preview cell is "nothing to draw", not a value.
    let nan_preview = view(vec![vec![1.0, 2.0]], Some(vec![f64::NAN]));
    assert_eq!(last_value(&nan_preview), Some(2.0));

    let trailing_nan = view(vec![vec![1.0, 2.0, f64::NAN]], None);
    assert_eq!(last_value(&trailing_nan), Some(2.0));

    assert_eq!(last_value(&view(vec![vec![f64::NAN]], None)), None);
}

#[test]
fn the_auto_range_pads_the_values_and_has_nothing_to_pad_during_warmup() {
    let pane = view(vec![vec![0.0, 10.0]], None);
    let (lo, hi) = pane_auto_range(&pane, 0, 2).expect("two values are a range");
    assert!(lo < 0.0 && hi > 10.0, "the trace never touches the edges");
    assert!(
        (lo + 0.8).abs() < 1e-9 && (hi - 10.8).abs() < 1e-9,
        "8% of the span on each side: {lo}..{hi}"
    );

    assert!(
        pane_auto_range(&view(vec![vec![f64::NAN]], None), 0, 1).is_none(),
        "nothing computed yet is nothing to scale — and no axis to draw"
    );
}

#[test]
fn a_shared_window_puts_zero_on_one_line() {
    // 0..100 and 0..50, each padded 8%. The dip below zero is the pad.
    let rsi = (-8.0, 108.0);
    let adx = (-4.0, 54.0);
    assert_eq!(
        shared_zero_from_top(&[rsi, adx]),
        Some(1.0),
        "padding past zero is not a negative side, so 0 sits on the bottom"
    );
    let macd = (-2.4, 3.4);
    assert_eq!(
        shared_zero_from_top(&[rsi, macd, adx]),
        Some(0.5),
        "a series that really crosses zero pulls the line to the middle"
    );
    assert_eq!(shared_zero_from_top(&[rsi]), None);
}

#[test]
fn the_middle_of_the_limits_is_what_lines_up_with_zero() {
    let mut rsi = view(vec![vec![55.0]], None);
    rsi.descriptor.inputs = vec![
        length("Length", 14),
        length("Upper", 70),
        length("Lower", 30),
    ];
    rsi.input_values = vec![
        InputValue::Int(14),
        InputValue::Int(70),
        InputValue::Int(30),
    ];
    assert_eq!(align_value(&rsi), 50.0);
    // 20..100 around 50 crosses that middle. 4..60 never crosses 0.
    assert_eq!(
        shared_zero_from_top(&[(20.0 - 50.0, 100.0 - 50.0), (4.0, 60.0)]),
        Some(0.5),
        "the oscillator's middle pulls the shared line to the centre"
    );
}

#[test]
fn auto_keeps_space_past_the_limits_and_the_middle_centred() {
    let mut rsi = view(vec![vec![40.0, 60.0]], None);
    rsi.descriptor.inputs = vec![
        length("Upper", 70),
        length("Lower", 30),
        flag("Auto", true),
        length("Space", 10),
    ];
    rsi.input_values = vec![
        InputValue::Int(70),
        InputValue::Int(30),
        InputValue::Bool(true),
        InputValue::Int(10),
    ];
    let (lo, hi) = pane_auto_range(&rsi, 0, 2).expect("two values");
    assert!(
        (lo - 20.0).abs() < 1e-9 && (hi - 80.0).abs() < 1e-9,
        "30 and 70 with 10 of room: {lo}..{hi}"
    );

    rsi.columns = vec![vec![15.0, 100.0]];
    let (lo, hi) = pane_auto_range(&rsi, 0, 2).expect("a trace past the room");
    assert!(
        (lo - 0.0).abs() < 1e-9 && (hi - 100.0).abs() < 1e-9,
        "past the room the scale widens and 50 stays in the middle: {lo}..{hi}"
    );

    rsi.input_values[2] = InputValue::Bool(false);
    let (lo, hi) = pane_auto_range(&rsi, 0, 2).expect("auto off still fits");
    assert!(
        lo < 15.0 && hi > 100.0,
        "off keeps the fractional pad: {lo}..{hi}"
    );
}

fn flag(title: &str, default: bool) -> InputSpec {
    InputSpec::Bool {
        name: title.to_owned(),
        title: title.to_owned(),
        default,
    }
}

fn length(title: &str, default: i64) -> InputSpec {
    InputSpec::Int {
        name: title.to_owned(),
        title: title.to_owned(),
        default,
        min: Some(1),
        max: Some(500),
        step: None,
        options: Vec::new(),
    }
}

#[test]
fn the_title_line_colours_each_series_and_names_its_length() {
    let mut rsi = view(vec![vec![55.0]], None);
    rsi.label = std::sync::Arc::from("RSI");
    rsi.descriptor.inputs = vec![length("Length", 14)];
    rsi.input_values = vec![InputValue::Int(14)];
    rsi.descriptor.plots[0].title = "RSI".to_owned();
    rsi.descriptor.plots[0].base_color = Rgba8::opaque(128, 0, 128);
    let bits = heading_bits(&rsi);
    assert_eq!(
        bits[0].text,
        format!("RSI (14) {}", crate::indicator_views::format_value(55.0))
    );
    assert_eq!(bits[0].color, Rgba8::opaque(128, 0, 128));

    let mut force = view(
        vec![vec![1.25], vec![f64::NAN], vec![f64::NAN], vec![-0.5]],
        None,
    );
    force.label = std::sync::Arc::from("Force Index");
    for (index, title) in ["1 line", "1 bars", "2 line", "2 bars"]
        .into_iter()
        .enumerate()
    {
        force.descriptor.plots[index].title = title.to_owned();
    }
    force.descriptor.plots[0].base_color = Rgba8::opaque(0, 0, 255);
    force.descriptor.plots[3].base_color = Rgba8::opaque(255, 128, 0);
    force.descriptor.inputs = vec![length("1: length", 13), length("2: length", 2)];
    force.input_values = vec![InputValue::Int(13), InputValue::Int(2)];
    let bits = heading_bits(&force);
    assert_eq!(
        bits.len(),
        2,
        "the na twin of a line or bars plot stays off the title"
    );
    assert_eq!(
        bits[0].text,
        format!(
            "Force Index (13) {}",
            crate::indicator_views::format_value(1.25)
        )
    );
    assert_eq!(
        bits[1].text,
        format!("(2) {}", crate::indicator_views::format_value(-0.5))
    );
    assert_eq!(bits[0].color, Rgba8::opaque(0, 0, 255));
    assert_eq!(bits[1].color, Rgba8::opaque(255, 128, 0));

    let mut adx = view(vec![vec![20.0], vec![15.0], vec![25.0]], None);
    adx.label = std::sync::Arc::from("Directional Movement / ADX");
    for (index, title) in ["+DI", "-DI", "ADX"].into_iter().enumerate() {
        adx.descriptor.plots[index].title = title.to_owned();
    }
    adx.descriptor.inputs = vec![length("Length", 14), length("ADX smoothing", 14)];
    adx.input_values = vec![InputValue::Int(14), InputValue::Int(14)];
    let bits = heading_bits(&adx);
    let show = |value| crate::indicator_views::format_value(value);
    assert_eq!(
        bits[0].text,
        format!("Directional Movement / ADX (14, 14) +DI {}", show(20.0))
    );
    assert_eq!(bits[1].text, format!("-DI {}", show(15.0)));
    assert_eq!(bits[2].text, format!("ADX {}", show(25.0)));

    let mut macd = view(vec![vec![1.0], vec![0.8], vec![0.2]], None);
    macd.label = std::sync::Arc::from("MACD");
    for (index, title) in ["MACD", "Signal", "Histogram"].into_iter().enumerate() {
        macd.descriptor.plots[index].title = title.to_owned();
    }
    macd.descriptor.inputs = vec![
        length("Fast length", 12),
        length("Slow length", 26),
        length("Signal length", 9),
    ];
    macd.input_values = vec![InputValue::Int(12), InputValue::Int(26), InputValue::Int(9)];
    let bits = heading_bits(&macd);
    assert_eq!(bits[0].text, format!("MACD (12) {}", show(1.0)));
    assert_eq!(bits[1].text, format!("Signal (26) {}", show(0.8)));
    assert_eq!(bits[2].text, format!("Histogram (9) {}", show(0.2)));
}
