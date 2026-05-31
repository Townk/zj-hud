//! Segments module for zj-statusbar.
//!
//! Provides right-side status segment functions. Each returns a `Segment` with
//! styled text, cell width, and background color for use by the render module.

use unicode_width::UnicodeWidthStr;
use zellij_tile::prelude::InputMode;

use crate::color::{contrast_ratio, Color};
use crate::config::{
    format_widget_number, resolve_widget_symbol, Config, DateTimeConfig, InfoWidget, MatchValue,
    SystemInfoBlock, WidgetKind,
};
use crate::icons;
use crate::state::{AppState, WidgetSample};

// ─── Segment ─────────────────────────────────────────────────────────────────

pub struct Segment {
    pub text: String, // ANSI-styled text
    pub width: usize, // cell width of the visible content
    pub bg: Color,    // background color (needed by divider rendering)
}

// ─── Core renderer ────────────────────────────────────────────────────────────

/// Format a segment body with auto-contrasted foreground on `bg`.
///
/// Body is `" {text}"` if `is_last`, otherwise `" {text} "`.
pub fn format_segment(bg: Color, text: &str, is_last: bool) -> Segment {
    // 1. Compute foreground: start with a dark variant
    let mut fg = bg.darken(0.8);
    if contrast_ratio(bg, fg) < 3.8 {
        fg = bg.lighten(0.6);
    }

    // 2. Build body
    let body = if is_last {
        format!(" {}", text)
    } else {
        format!(" {} ", text)
    };

    // 3. Measure width
    let width = UnicodeWidthStr::width(body.as_str());

    // 4. Build styled text
    let styled = format!("{}{}{}", bg.to_ansi_bg(), fg.to_ansi_fg(), body);

    Segment {
        text: styled,
        width,
        bg,
    }
}

// ─── Divider helpers ──────────────────────────────────────────────────────────

pub fn divider(left_bg: Color, right_bg: Color) -> String {
    format!(
        "{}{}{}",
        left_bg.to_ansi_bg(),
        right_bg.to_ansi_fg(),
        icons::PLE_LOWER_RIGHT_TRIANGLE
    )
}

pub fn divider_width() -> usize {
    UnicodeWidthStr::width(icons::PLE_LOWER_RIGHT_TRIANGLE)
}

// ─── Mode segment ─────────────────────────────────────────────────────────────

/// Returns a segment for the current input mode, or `None` if Normal.
pub fn mode_segment(mode: InputMode, bg: Color, is_last: bool, config: &Config) -> Option<Segment> {
    if mode == InputMode::Normal {
        return None;
    }

    let style = config.mode_style(mode)?;
    let text = format!("{} {}", style.icon, style.label);
    Some(format_segment(bg, &text, is_last))
}

// ─── Session segment ──────────────────────────────────────────────────────────

/// Returns a segment for the session name, or `None` if it's the default.
pub fn session_segment(
    session_name: &str,
    bg: Color,
    is_last: bool,
    config: &Config,
) -> Option<Segment> {
    if session_name.is_empty()
        || session_name.eq_ignore_ascii_case("main")
        || session_name == config.default_session_name
    {
        return None;
    }

    let text = format!(
        "{} {}",
        icons::MODE_SESSION,
        kebab_to_title_case(session_name)
    );
    Some(format_segment(bg, &text, is_last))
}

/// Converts a `kebab-case` name into `Title Case` for display, e.g.
/// `my-cool-session` → `My Cool Session`. Hyphens become spaces and each
/// resulting word is capitalized. Names without hyphens still get their first
/// letter capitalized. Empty segments (from leading/trailing/double hyphens)
/// are dropped so we never emit stray spaces.
fn kebab_to_title_case(name: &str) -> String {
    name.split('-')
        .filter(|word| !word.is_empty())
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

// ─── Time segment ─────────────────────────────────────────────────────────────

/// Returns a segment showing the current date and time, formatted per
/// `DateTimeConfig`.
///
/// `tz_offset_seconds` is the UTC offset to apply (positive east), sampled
/// by `system::maybe_refresh_tz_offset` from `date +%z`. We can't rely on
/// `chrono::Local::now()` here because Zellij plugins run as WASI guests
/// with no access to the host timezone database — chrono silently falls
/// back to UTC. So we take the UTC instant from chrono and re-anchor it
/// to a `FixedOffset` we trust.
pub fn time_segment(
    bg: Color,
    is_last: bool,
    dt: &DateTimeConfig,
    tz_offset_seconds: i32,
) -> Segment {
    let offset = chrono::FixedOffset::east_opt(tz_offset_seconds)
        .unwrap_or_else(|| chrono::FixedOffset::east_opt(0).expect("UTC is always valid"));
    let now = chrono::Utc::now().with_timezone(&offset);
    // chrono's `format` returns a `DelayedFormat` that panics on invalid format
    // strings the moment it's converted to a `String`. Catch that so a
    // misconfigured user format doesn't crash the plugin.
    let formatted =
        std::panic::catch_unwind(|| now.format(&dt.format).to_string()).unwrap_or_else(|_| {
            now.format(crate::config::DEFAULT_DATE_TIME_FORMAT)
                .to_string()
        });
    let text = if dt.symbol.is_empty() {
        formatted
    } else {
        format!("{} {}", dt.symbol, formatted)
    };
    format_segment(bg, &text, is_last)
}

// ─── Info widgets segment ─────────────────────────────────────────────────────

/// Per-widget click subregion within the info-widgets segment, expressed in
/// column offsets from the start of the segment text (so column 0 = leading
/// space, column 1 = first cell of widget content).
pub struct WidgetClickRegion {
    pub start: usize,
    pub end: usize,
    pub on_click: usize,
}

pub struct InfoWidgetsResult {
    pub segment: Segment,
    pub click_regions: Vec<WidgetClickRegion>,
}

/// Build a list of `(rendered_text, on_click_idx)` tuples for the widgets
/// that should appear in the segment. A widget is included when:
/// - its visibility passes,
/// - it has a sample (either `Value` or `Error`),
/// - the resolved render is non-empty (so `Empty` samples and widgets whose
///   `default` resolves to `""` simply disappear instead of contributing a
///   dangling separator).
fn renderable_widgets<'a>(
    state: &'a AppState,
    config: &'a Config,
    block: &'a SystemInfoBlock,
    cols: usize,
) -> Vec<(String, Option<usize>)> {
    let mut out: Vec<(String, Option<usize>)> = Vec::new();

    for widget in &block.widgets {
        if !crate::system::is_visible(widget.visibility, widget.min_cols, state, config, cols) {
            continue;
        }
        let Some(sample) = state
            .widgets
            .get(&widget.id)
            .and_then(|s| s.sample.as_ref())
        else {
            continue;
        };

        let Some(rendered) = render_one_widget(widget, sample) else {
            continue;
        };
        out.push((rendered, widget.on_click));
    }

    out
}

fn render_one_widget(widget: &InfoWidget, sample: &WidgetSample) -> Option<String> {
    let resolved = match sample {
        WidgetSample::Empty => return None,
        WidgetSample::Error => widget.default_symbol.replace("{value}", "ERR"),
        WidgetSample::Value(raw) => match widget.kind {
            WidgetKind::Number => match raw.parse::<f64>() {
                Ok(n) => {
                    let display = format_widget_number(n);
                    let symbol = resolve_widget_symbol(widget, &MatchValue::Number(n));
                    symbol.replace("{value}", &display)
                }
                Err(_) => widget.default_symbol.replace("{value}", "ERR"),
            },
            WidgetKind::String => {
                let symbol = resolve_widget_symbol(widget, &MatchValue::String(raw.clone()));
                symbol.replace("{value}", raw)
            }
        },
    };

    if resolved.is_empty() {
        None
    } else {
        Some(resolved)
    }
}

/// Build the styled info-widgets segment, joining each renderable widget with
/// the configured separator. Returns `None` if no widgets are visible / have
/// samples — the caller should skip the segment entirely in that case.
pub fn info_widgets_segment(
    state: &AppState,
    config: &Config,
    block: &SystemInfoBlock,
    bg: Color,
    is_last: bool,
    cols: usize,
) -> Option<InfoWidgetsResult> {
    let widgets = renderable_widgets(state, config, block, cols);
    if widgets.is_empty() {
        return None;
    }

    // Compose the body and per-widget click ranges. The `format_segment` call
    // below prepends a leading space, so widget content starts at column 1.
    let separator = block.separator.as_str();
    let separator_width = UnicodeWidthStr::width(separator);

    let mut body = String::new();
    let mut click_regions: Vec<WidgetClickRegion> = Vec::new();
    let mut col: usize = 1; // 0 is the leading space inserted by format_segment

    for (i, (text, on_click)) in widgets.iter().enumerate() {
        if i > 0 {
            body.push_str(separator);
            col += separator_width;
        }
        let w = UnicodeWidthStr::width(text.as_str());
        let start = col;
        body.push_str(text);
        col += w;
        let end = col;
        if let Some(idx) = on_click {
            click_regions.push(WidgetClickRegion {
                start,
                end,
                on_click: *idx,
            });
        }
    }

    let segment = format_segment(bg, &body, is_last);
    Some(InfoWidgetsResult {
        segment,
        click_regions,
    })
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    #[test]
    fn format_segment_has_leading_space() {
        let seg = format_segment(Color::new(100, 100, 100), "test", false);
        assert!(seg.width >= 6); // " test " = 6
    }

    #[test]
    fn format_segment_last_no_trailing_space() {
        let seg_last = format_segment(Color::new(100, 100, 100), "test", true);
        let seg_mid = format_segment(Color::new(100, 100, 100), "test", false);
        assert_eq!(seg_last.width, seg_mid.width - 1);
    }

    #[test]
    fn mode_segment_suppressed_for_normal() {
        let config = Config::default();
        assert!(
            mode_segment(InputMode::Normal, Color::new(100, 100, 100), false, &config).is_none()
        );
    }

    #[test]
    fn mode_segment_present_for_locked() {
        let config = Config::default();
        let seg = mode_segment(InputMode::Locked, Color::new(255, 102, 102), false, &config);
        assert!(seg.is_some());
        assert!(seg.unwrap().width > 0);
    }

    #[test]
    fn mode_segment_uses_configured_icon_and_label() {
        let mut map = std::collections::BTreeMap::new();
        map.insert(
            "modes".to_string(),
            r##"locked color="#fab387" icon="L" label="Passthrough""##.to_string(),
        );
        let config = Config::from_map(map);
        let seg =
            mode_segment(InputMode::Locked, Color::new(255, 102, 102), false, &config).unwrap();

        assert!(seg.text.contains("L Passthrough"));
    }

    #[test]
    fn session_segment_suppressed_for_main() {
        let config = Config::default();
        assert!(session_segment("main", Color::new(100, 100, 100), false, &config).is_none());
    }

    #[test]
    fn session_segment_suppresses_main_when_default_is_overridden() {
        let config = Config {
            default_session_name: "work".to_string(),
            ..Config::default()
        };
        assert!(session_segment("main", Color::new(100, 100, 100), false, &config).is_none());
    }

    #[test]
    fn session_segment_suppresses_capitalized_main() {
        let config = Config::default();
        assert!(session_segment("Main", Color::new(100, 100, 100), false, &config).is_none());
    }

    #[test]
    fn session_segment_shown_for_other() {
        let config = Config::default();
        let seg = session_segment("dev", Color::new(100, 100, 100), false, &config);
        assert!(seg.is_some());
    }

    #[test]
    fn session_segment_converts_kebab_to_title_case() {
        let config = Config::default();
        let seg =
            session_segment("my-cool-session", Color::new(100, 100, 100), false, &config).unwrap();
        assert!(seg.text.contains("My Cool Session"));
        assert!(!seg.text.contains("my-cool-session"));
    }

    #[test]
    fn kebab_to_title_case_handles_edge_cases() {
        assert_eq!(kebab_to_title_case("dev"), "Dev");
        assert_eq!(kebab_to_title_case("my-cool-session"), "My Cool Session");
        assert_eq!(
            kebab_to_title_case("-leading-trailing-"),
            "Leading Trailing"
        );
        assert_eq!(kebab_to_title_case("double--hyphen"), "Double Hyphen");
    }

    // ── Info widgets ─────────────────────────────────────────────────────

    fn build_widget(matches: Vec<crate::config::SymbolRule>) -> InfoWidget {
        InfoWidget {
            id: "w".to_string(),
            kind: WidgetKind::Number,
            command: "true".to_string(),
            interval: std::time::Duration::ZERO,
            default_symbol: "D {value}".to_string(),
            matches,
            on_click: None,
            visibility: crate::config::Visibility::Always,
            min_cols: None,
        }
    }

    #[test]
    fn render_one_widget_substitutes_value() {
        let widget = build_widget(vec![crate::config::SymbolRule::Range {
            from: Some(0.0),
            to: Some(100.0),
            glyph: "G {value}%".to_string(),
        }]);
        let out = render_one_widget(&widget, &WidgetSample::Value("75".to_string()));
        assert_eq!(out.as_deref(), Some("G 75%"));
    }

    #[test]
    fn render_one_widget_renders_error_with_default() {
        let widget = build_widget(vec![]);
        let out = render_one_widget(&widget, &WidgetSample::Error);
        assert_eq!(out.as_deref(), Some("D ERR"));
    }

    #[test]
    fn render_one_widget_unparseable_number_becomes_error() {
        let widget = build_widget(vec![]);
        let out = render_one_widget(&widget, &WidgetSample::Value("oops".to_string()));
        assert_eq!(out.as_deref(), Some("D ERR"));
    }

    #[test]
    fn render_one_widget_empty_sample_returns_none() {
        let widget = build_widget(vec![]);
        let out = render_one_widget(&widget, &WidgetSample::Empty);
        assert!(out.is_none());
    }

    #[test]
    fn render_one_widget_empty_resolved_returns_none() {
        // Default of "" + Error → resolves to "" → skip.
        let widget = InfoWidget {
            id: "w".to_string(),
            kind: WidgetKind::Number,
            command: "true".to_string(),
            interval: std::time::Duration::ZERO,
            default_symbol: "".to_string(),
            matches: vec![],
            on_click: None,
            visibility: crate::config::Visibility::Always,
            min_cols: None,
        };
        assert!(render_one_widget(&widget, &WidgetSample::Error).is_none());
        assert!(render_one_widget(&widget, &WidgetSample::Empty).is_none());
    }

    #[test]
    fn info_widgets_segment_skips_error_widget_with_empty_default_in_middle() {
        // Mirrors the user's setup: three widgets where the FIRST one has
        // Error+default="" (so it should be skipped) and the next two render.
        // The body must NOT start with a separator from the skipped widget.
        let mut state = AppState::default();
        state.widgets.insert(
            "power".to_string(),
            crate::state::WidgetState {
                sample: Some(WidgetSample::Error),
                last_updated: None,
                in_flight: false,
            },
        );
        state.widgets.insert(
            "battery".to_string(),
            crate::state::WidgetState {
                sample: Some(WidgetSample::Value("85".to_string())),
                last_updated: None,
                in_flight: false,
            },
        );
        state.widgets.insert(
            "wifi".to_string(),
            crate::state::WidgetState {
                sample: Some(WidgetSample::Value("1".to_string())),
                last_updated: None,
                in_flight: false,
            },
        );

        let power = InfoWidget {
            id: "power".to_string(),
            kind: WidgetKind::Number,
            command: "true".to_string(),
            interval: std::time::Duration::ZERO,
            default_symbol: "".to_string(),
            matches: vec![],
            on_click: None,
            visibility: crate::config::Visibility::Always,
            min_cols: None,
        };
        let battery = InfoWidget {
            id: "battery".to_string(),
            kind: WidgetKind::Number,
            command: "true".to_string(),
            interval: std::time::Duration::ZERO,
            default_symbol: "".to_string(),
            matches: vec![crate::config::SymbolRule::Range {
                from: Some(40.0),
                to: Some(89.0),
                glyph: "B{value}".to_string(),
            }],
            on_click: None,
            visibility: crate::config::Visibility::Always,
            min_cols: None,
        };
        let wifi = InfoWidget {
            id: "wifi".to_string(),
            kind: WidgetKind::Number,
            command: "true".to_string(),
            interval: std::time::Duration::ZERO,
            default_symbol: "W".to_string(),
            matches: vec![crate::config::SymbolRule::Exact {
                value: crate::config::MatchValue::Number(1.0),
                glyph: "W1".to_string(),
            }],
            on_click: None,
            visibility: crate::config::Visibility::Always,
            min_cols: None,
        };

        let block = SystemInfoBlock {
            separator: "  ".to_string(),
            visibility: crate::config::Visibility::Always,
            min_cols: None,
            widgets: vec![power, battery, wifi],
        };

        let config = Config::default();
        let result =
            info_widgets_segment(&state, &config, &block, Color::new(50, 50, 50), true, 80)
                .expect("segment");

        // The visible part of the body should be exactly " B85  W1"
        // (1 leading space + battery + 2-space separator + wifi).
        // Strip ANSI escapes by finding the ' ' before 'B85'.
        assert!(
            result.segment.text.contains("B85  W1"),
            "expected `B85  W1` in segment text: {:?}",
            result.segment.text
        );
        assert!(
            !result.segment.text.contains("  B85"),
            "leading separator before battery: {:?}",
            result.segment.text
        );
        assert!(
            !result.segment.text.contains("B85    W1"),
            "duplicated separator between widgets: {:?}",
            result.segment.text
        );
    }

    #[test]
    fn info_widgets_segment_skips_empty_widgets_without_dangling_separator() {
        // Two widgets configured: the first has Empty sample (skipped), the
        // second has a value. The rendered segment must NOT begin with the
        // separator that would normally precede the second widget.
        let mut state = AppState::default();
        state.widgets.insert(
            "skip_me".to_string(),
            crate::state::WidgetState {
                sample: Some(WidgetSample::Empty),
                last_updated: None,
                in_flight: false,
            },
        );
        state.widgets.insert(
            "show_me".to_string(),
            crate::state::WidgetState {
                sample: Some(WidgetSample::Value("42".to_string())),
                last_updated: None,
                in_flight: false,
            },
        );

        let mk = |id: &str| InfoWidget {
            id: id.to_string(),
            kind: WidgetKind::Number,
            command: "true".to_string(),
            interval: std::time::Duration::ZERO,
            default_symbol: "X{value}".to_string(),
            matches: vec![],
            on_click: None,
            visibility: crate::config::Visibility::Always,
            min_cols: None,
        };

        let block = SystemInfoBlock {
            separator: "|".to_string(),
            visibility: crate::config::Visibility::Always,
            min_cols: None,
            widgets: vec![mk("skip_me"), mk("show_me")],
        };

        let config = Config::default();
        let result =
            info_widgets_segment(&state, &config, &block, Color::new(50, 50, 50), true, 80)
                .expect("segment");

        // Body should be " X42" (leading space + the surviving widget),
        // *not* "|X42" or " |X42".
        assert!(
            result.segment.text.contains("X42"),
            "expected rendered widget in {:?}",
            result.segment.text
        );
        assert!(
            !result.segment.text.contains("|X42"),
            "separator leaked into rendering: {:?}",
            result.segment.text
        );
    }

    #[test]
    fn info_widgets_segment_returns_none_when_no_data() {
        let state = AppState::default();
        let config = Config::default();
        let block = SystemInfoBlock {
            separator: " ".to_string(),
            visibility: crate::config::Visibility::Always,
            min_cols: None,
            widgets: vec![build_widget(vec![])],
        };
        let result =
            info_widgets_segment(&state, &config, &block, Color::new(50, 50, 50), true, 80);
        assert!(result.is_none());
    }

    #[test]
    fn info_widgets_segment_emits_per_widget_clicks() {
        let mut state = AppState::default();
        // Two widgets with samples, both with on_click handlers.
        state.widgets.insert(
            "a".to_string(),
            crate::state::WidgetState {
                sample: Some(WidgetSample::Value("1".to_string())),
                last_updated: None,
                in_flight: false,
            },
        );
        state.widgets.insert(
            "b".to_string(),
            crate::state::WidgetState {
                sample: Some(WidgetSample::Value("2".to_string())),
                last_updated: None,
                in_flight: false,
            },
        );

        let mk = |id: &str, on_click: Option<usize>| InfoWidget {
            id: id.to_string(),
            kind: WidgetKind::Number,
            command: "true".to_string(),
            interval: std::time::Duration::ZERO,
            default_symbol: "X{value}".to_string(),
            matches: vec![],
            on_click,
            visibility: crate::config::Visibility::Always,
            min_cols: None,
        };

        let block = SystemInfoBlock {
            separator: "|".to_string(),
            visibility: crate::config::Visibility::Always,
            min_cols: None,
            widgets: vec![mk("a", Some(0)), mk("b", Some(1))],
        };

        let config = Config::default();
        let result =
            info_widgets_segment(&state, &config, &block, Color::new(50, 50, 50), true, 80)
                .expect("segment");

        assert_eq!(result.click_regions.len(), 2);
        // First widget rendering is "X1" → starts at col 1, ends at col 3.
        assert_eq!(result.click_regions[0].start, 1);
        assert_eq!(result.click_regions[0].end, 3);
        assert_eq!(result.click_regions[0].on_click, 0);
        // Separator "|" is one cell wide; second widget "X2" starts at col 4.
        assert_eq!(result.click_regions[1].start, 4);
        assert_eq!(result.click_regions[1].end, 6);
        assert_eq!(result.click_regions[1].on_click, 1);
    }
}
