//! Config module for zj-statusbar.
//!
//! Parses plugin configuration from Zellij's `BTreeMap<String, String>` into
//! a typed `Config` struct with sensible defaults.

use kdl::{KdlDocument, KdlNode, KdlValue};
use std::collections::BTreeMap;
use std::time::Duration;
use zellij_tile::prelude::InputMode;

use crate::color::Color;
use crate::icons;

// ─── Defaults shared across widget/status parsing ─────────────────────────────

/// chrono strftime format for the built-in date-time segment.
pub const DEFAULT_DATE_TIME_FORMAT: &str = "%a %b %-e %-l:%M%P";

/// Joins widget renderings inside the system_info segment.
pub const DEFAULT_WIDGET_SEPARATOR: &str = " ";

// ─── Constants ────────────────────────────────────────────────────────────────

pub const DEFAULT_TAB_MAX_WIDTH: usize = 40;
pub const DEFAULT_TAB_MIN_SHRINK_WIDTH: usize = 20;
pub const DEFAULT_TRUNCATION_POINT: f32 = 0.4;
pub const DEFAULT_FULLSCREEN_MIN_COLS: usize = 120;
pub const DEFAULT_SESSION_NAME: &str = "main";
pub const DEFAULT_PROJECT_MARKERS: &[&str] = &[
    ".git",
    ".jj",
    "package.json",
    "Cargo.toml",
    "go.mod",
    "pyproject.toml",
    "flake.nix",
    "Makefile",
    ".project-root",
];

// ─── IconLibrary ──────────────────────────────────────────────────────────────

pub struct IconLibrary {
    pub tab_dir: String,
    pub tab_home: String,
    pub tab_process: String,
    pub tab_icon: String,
    pub zoom_icon: String,
    pub calendar: String,
}

impl Default for IconLibrary {
    fn default() -> Self {
        IconLibrary {
            tab_dir: icons::TAB_DIR.to_string(),
            tab_home: icons::TAB_HOME.to_string(),
            tab_process: icons::TAB_PROCESS.to_string(),
            tab_icon: icons::TAB_ICON.to_string(),
            zoom_icon: icons::ZOOM_ICON.to_string(),
            calendar: icons::CALENDAR.to_string(),
        }
    }
}

// ─── Status / widgets ────────────────────────────────────────────────────────

/// Visibility gating shared by `date_time`, `system_info`, and individual
/// widgets. `Fullscreen` defers to the existing `should_show_system_segments`
/// predicate (zoomed pane / non-graphical / Ghostty fullscreen / cols ≥ N).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Visibility {
    Always,
    #[default]
    Fullscreen,
    Never,
}

impl Visibility {
    fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "always" => Some(Visibility::Always),
            "fullscreen" => Some(Visibility::Fullscreen),
            "never" => Some(Visibility::Never),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct DateTimeConfig {
    pub visibility: Visibility,
    pub min_cols: Option<usize>,
    pub symbol: String,
    pub format: String,
    pub on_click: Option<usize>,
}

impl Default for DateTimeConfig {
    fn default() -> Self {
        Self {
            visibility: Visibility::Fullscreen,
            min_cols: None,
            symbol: icons::CALENDAR.to_string(),
            format: DEFAULT_DATE_TIME_FORMAT.to_string(),
            on_click: None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct SystemInfoBlock {
    pub separator: String,
    pub visibility: Visibility,
    pub min_cols: Option<usize>,
    pub widgets: Vec<InfoWidget>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WidgetKind {
    Number,
    String,
}

#[derive(Clone, Debug)]
pub enum MatchValue {
    Number(f64),
    String(String),
}

#[derive(Clone, Debug)]
pub enum SymbolRule {
    /// Exact equality match. For `Number` widgets, only `MatchValue::Number`
    /// rules participate; for `String` widgets, only `MatchValue::String`.
    Exact { value: MatchValue, glyph: String },
    /// Inclusive range (`from <= v <= to`). `None` bound = unbounded on that
    /// side. Range rules apply only to `Number` widgets.
    Range {
        from: Option<f64>,
        to: Option<f64>,
        glyph: String,
    },
}

#[derive(Clone, Debug)]
pub struct InfoWidget {
    pub id: String,
    pub kind: WidgetKind,
    pub command: String,
    /// `Duration::ZERO` means "fire once and cache forever".
    pub interval: Duration,
    pub default_symbol: String,
    pub matches: Vec<SymbolRule>,
    pub on_click: Option<usize>,
    pub visibility: Visibility,
    pub min_cols: Option<usize>,
}

#[derive(Clone, Debug, Default)]
pub struct StatusConfig {
    pub date_time: DateTimeConfig,
    pub system_info: Option<SystemInfoBlock>,
}

// ─── Config ───────────────────────────────────────────────────────────────────

pub struct Config {
    pub tab_max_width: usize,
    pub tab_min_shrink_width: usize,
    pub tab_truncation_point: f32,
    pub tab_hide_single: bool,
    pub fullscreen_min_cols: usize,
    pub default_session_name: String,
    pub project_markers: Vec<String>,
    pub mode_styles: Vec<(InputMode, ModeStyle)>,
    pub icons: IconLibrary,
    pub status: StatusConfig,
    /// Storage for `on_click` shell commands. `ClickAction::RunCommand(idx)`
    /// indexes into this vector, keeping `ClickAction: Copy`.
    pub click_commands: Vec<String>,
    /// URL of this plugin's own `.wasm`, used to spawn the floating search
    /// pane. Defaults to the conventional install path; override with the
    /// `search_plugin_url` config key if you install it elsewhere. Must match
    /// the `location=` your layout uses, or Zellij will load a second copy.
    pub search_plugin_url: String,
}

/// Conventional install location of the plugin (mirrors the `justfile`).
fn default_search_plugin_url() -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    format!("file:{home}/.config/zellij/plugins/zj-statusbar.wasm")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModeStyle {
    pub color: Color,
    pub icon: String,
    pub label: String,
}

impl ModeStyle {
    fn new(color: Color, icon: &str, label: &str) -> Self {
        Self {
            color,
            icon: icon.to_string(),
            label: label.to_string(),
        }
    }
}

fn default_mode_styles() -> Vec<(InputMode, ModeStyle)> {
    vec![
        (
            InputMode::Locked,
            ModeStyle::new(Color::new(255, 102, 102), icons::MODE_LOCKED, "Locked"),
        ),
        (
            InputMode::Resize,
            ModeStyle::new(Color::new(255, 204, 102), icons::MODE_RESIZE, "Resize"),
        ),
        (
            InputMode::Pane,
            ModeStyle::new(Color::new(102, 204, 255), icons::MODE_PANE, "Pane"),
        ),
        (
            InputMode::Tab,
            ModeStyle::new(Color::new(204, 153, 255), icons::MODE_TAB, "Tab"),
        ),
        (
            InputMode::Scroll,
            ModeStyle::new(Color::new(153, 255, 204), icons::MODE_SCROLL, "Scroll"),
        ),
        (
            InputMode::EnterSearch,
            ModeStyle::new(Color::new(255, 255, 102), icons::MODE_SEARCH, "Search"),
        ),
        (
            InputMode::Search,
            ModeStyle::new(Color::new(255, 255, 102), icons::MODE_SEARCH, "Search"),
        ),
        (
            InputMode::RenameTab,
            ModeStyle::new(Color::new(255, 204, 153), icons::MODE_RENAME, "Rename"),
        ),
        (
            InputMode::RenamePane,
            ModeStyle::new(Color::new(255, 204, 153), icons::MODE_RENAME, "Rename"),
        ),
        (
            InputMode::Session,
            ModeStyle::new(Color::new(255, 153, 204), icons::MODE_SESSION, "Session"),
        ),
        (
            InputMode::Move,
            ModeStyle::new(Color::new(102, 255, 204), icons::MODE_MOVE, "Move"),
        ),
        (
            InputMode::Prompt,
            ModeStyle::new(Color::new(153, 204, 255), icons::MODE_PROMPT, "Prompt"),
        ),
        (
            InputMode::Tmux,
            ModeStyle::new(Color::new(204, 102, 255), icons::MODE_TMUX, "Command"),
        ),
    ]
}

impl Default for Config {
    fn default() -> Self {
        Config {
            tab_max_width: DEFAULT_TAB_MAX_WIDTH,
            tab_min_shrink_width: DEFAULT_TAB_MIN_SHRINK_WIDTH,
            tab_truncation_point: DEFAULT_TRUNCATION_POINT,
            tab_hide_single: false,
            fullscreen_min_cols: DEFAULT_FULLSCREEN_MIN_COLS,
            default_session_name: DEFAULT_SESSION_NAME.to_string(),
            project_markers: DEFAULT_PROJECT_MARKERS
                .iter()
                .map(|s| s.to_string())
                .collect(),
            mode_styles: default_mode_styles(),
            icons: IconLibrary::default(),
            status: StatusConfig::default(),
            click_commands: Vec::new(),
            search_plugin_url: default_search_plugin_url(),
        }
    }
}

impl Config {
    /// Build a `Config` from Zellij plugin configuration map. Unknown keys are
    /// silently ignored; malformed values fall back to defaults.
    pub fn from_map(map: BTreeMap<String, String>) -> Self {
        let mut cfg = Config::default();

        // ── Scalar fields ──────────────────────────────────────────────────
        if let Some(v) = map.get("tab_max_width") {
            if let Ok(n) = v.parse::<usize>() {
                cfg.tab_max_width = n;
            }
        }

        if let Some(v) = map.get("tab_min_shrink_width") {
            if let Ok(n) = v.parse::<usize>() {
                cfg.tab_min_shrink_width = n;
            }
        }

        if let Some(v) = map.get("tab_truncation_point") {
            if let Ok(f) = v.parse::<f32>() {
                cfg.tab_truncation_point = f;
            }
        }

        if let Some(v) = map.get("tab_hide_single") {
            cfg.tab_hide_single = v == "true";
        }

        if let Some(tabs) = map.get("tabs") {
            parse_tabs_block(tabs, &mut cfg);
        }

        if let Some(v) = map.get("fullscreen_min_cols") {
            if let Ok(n) = v.parse::<usize>() {
                cfg.fullscreen_min_cols = n;
            }
        }

        if let Some(v) = map.get("default_session_name") {
            cfg.default_session_name = v.clone();
        }

        if let Some(v) = map.get("search_plugin_url") {
            if !v.trim().is_empty() {
                cfg.search_plugin_url = v.clone();
            }
        }

        // ── Mode colors ────────────────────────────────────────────────────
        let mode_color_keys: &[(&str, InputMode)] = &[
            ("mode_color_locked", InputMode::Locked),
            ("mode_color_resize", InputMode::Resize),
            ("mode_color_pane", InputMode::Pane),
            ("mode_color_tab", InputMode::Tab),
            ("mode_color_scroll", InputMode::Scroll),
            ("mode_color_search", InputMode::Search),
            ("mode_color_session", InputMode::Session),
            ("mode_color_move", InputMode::Move),
            ("mode_color_tmux", InputMode::Tmux),
            ("mode_color_rename_tab", InputMode::RenameTab),
            ("mode_color_rename_pane", InputMode::RenamePane),
            ("mode_color_prompt", InputMode::Prompt),
        ];

        // Note: EnterSearch shares the same default as Search but has no
        // dedicated key — it tracks the Search color when overridden.
        for (key, mode) in mode_color_keys {
            if let Some(v) = map.get(*key) {
                if let Some(color) = Color::parse_hex(v) {
                    cfg.set_mode_color(*mode, color);
                    // Sync EnterSearch with Search when the search key is set
                    if *mode == InputMode::Search {
                        cfg.set_mode_color(InputMode::EnterSearch, color);
                    }
                }
            }
        }

        // ── Structured mode styles ─────────────────────────────────────────
        if let Some(modes) = map.get("modes") {
            parse_modes_block(modes, &mut cfg);
        }

        // ── Icon overrides ─────────────────────────────────────────────────
        if let Some(v) = map.get("icon_tab_dir") {
            cfg.icons.tab_dir = parse_icon_value(v);
        }
        if let Some(v) = map.get("icon_tab_home") {
            cfg.icons.tab_home = parse_icon_value(v);
        }
        if let Some(v) = map.get("icon_tab_process") {
            cfg.icons.tab_process = parse_icon_value(v);
        }
        if let Some(v) = map.get("icon_tab_icon") {
            cfg.icons.tab_icon = parse_icon_value(v);
        }
        if let Some(v) = map.get("icon_calendar") {
            cfg.icons.calendar = parse_icon_value(v);
        }

        // ── Status / widgets ───────────────────────────────────────────────
        // Sync the date_time default symbol with the (possibly overridden)
        // icon library *before* parsing the status block, so that an explicit
        // `status.date_time.symbol` still wins over the legacy override.
        cfg.status.date_time.symbol = cfg.icons.calendar.clone();
        if let Some(status) = map.get("status") {
            parse_status_block(status, &mut cfg);
        }

        cfg
    }

    /// Look up the configured color for the given `InputMode`. Returns `None`
    /// only if the mode is absent from `mode_styles` (i.e. `Normal`).
    pub fn mode_color(&self, mode: InputMode) -> Option<Color> {
        self.mode_style(mode).map(|style| style.color)
    }

    pub fn mode_style(&self, mode: InputMode) -> Option<&ModeStyle> {
        self.mode_styles
            .iter()
            .find(|(m, _)| *m == mode)
            .map(|(_, style)| style)
    }

    fn mode_style_mut(&mut self, mode: InputMode) -> Option<&mut ModeStyle> {
        self.mode_styles
            .iter_mut()
            .find(|(m, _)| *m == mode)
            .map(|(_, style)| style)
    }

    fn set_mode_color(&mut self, mode: InputMode, color: Color) {
        if let Some(style) = self.mode_style_mut(mode) {
            style.color = color;
        }
    }

    fn apply_mode_update(&mut self, mode: InputMode, update: &ModeStyleUpdate) {
        if let Some(style) = self.mode_style_mut(mode) {
            if let Some(color) = update.color {
                style.color = color;
            }
            if let Some(icon) = &update.icon {
                style.icon = icon.clone();
            }
            if let Some(label) = &update.label {
                style.label = label.clone();
            }
        }
    }
}

fn parse_tabs_block(value: &str, config: &mut Config) {
    let Some(doc) = parse_config_document(
        value,
        &[
            "max_width",
            "min_shrink_width",
            "truncation_point",
            "hide_single",
        ],
    ) else {
        return;
    };

    if let Some(max_width) = document_value(&doc, "max_width").and_then(|v| v.parse().ok()) {
        config.tab_max_width = max_width;
    }
    if let Some(min_shrink_width) =
        document_value(&doc, "min_shrink_width").and_then(|v| v.parse().ok())
    {
        config.tab_min_shrink_width = min_shrink_width;
    }
    if let Some(truncation_point) =
        document_value(&doc, "truncation_point").and_then(|v| v.parse().ok())
    {
        config.tab_truncation_point = truncation_point;
    }
    if let Some(hide_single) = document_value(&doc, "hide_single").and_then(|v| v.parse().ok()) {
        config.tab_hide_single = hide_single;
    }
    if let Some(markers) = document_values(&doc, "project_markers") {
        config.project_markers = dedup_strings(markers);
    }
    if let Some(extra_markers) = document_values(&doc, "extra_project_markers") {
        for marker in extra_markers {
            if !config.project_markers.contains(&marker) {
                config.project_markers.push(marker);
            }
        }
    }
}

#[derive(Default)]
struct ModeStyleUpdate {
    color: Option<Color>,
    icon: Option<String>,
    label: Option<String>,
}

fn parse_modes_block(value: &str, config: &mut Config) {
    let Some(doc) = parse_modes_document(value) else {
        return;
    };

    let enter_search_is_explicit = doc.nodes().iter().any(|node| {
        matches!(
            mode_from_name(node.name().value()),
            Some(InputMode::EnterSearch)
        )
    });

    for node in doc.nodes() {
        let Some(mode) = mode_from_name(node.name().value()) else {
            continue;
        };
        let update = parse_mode_style_update(node);

        if mode == InputMode::Search {
            config.apply_mode_update(InputMode::Search, &update);
            if !enter_search_is_explicit {
                config.apply_mode_update(InputMode::EnterSearch, &update);
            }
        } else {
            config.apply_mode_update(mode, &update);
        }
    }
}

fn parse_modes_document(value: &str) -> Option<KdlDocument> {
    parse_config_document(value, &["color", "icon", "label"])
}

fn parse_config_document(value: &str, child_assignment_keys: &[&str]) -> Option<KdlDocument> {
    value
        .parse::<KdlDocument>()
        .ok()
        .or_else(|| {
            normalize_child_assignments(value, child_assignment_keys)
                .parse::<KdlDocument>()
                .ok()
        })
        .or_else(|| normalize_spaced_equals(value).parse::<KdlDocument>().ok())
        .or_else(|| {
            normalize_spaced_equals(&normalize_child_assignments(value, child_assignment_keys))
                .parse::<KdlDocument>()
                .ok()
        })
}

fn normalize_child_assignments(value: &str, keys: &[&str]) -> String {
    value
        .lines()
        .map(|line| {
            for key in keys {
                if let Some(normalized) = normalize_child_assignment(line, key) {
                    return normalized;
                }
            }
            line.to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn normalize_child_assignment(line: &str, key: &str) -> Option<String> {
    let trimmed = line.trim_start();
    let leading = &line[..line.len() - trimmed.len()];
    let rest = trimmed.strip_prefix(key)?;

    if rest.is_empty() || !rest.starts_with(char::is_whitespace) {
        return None;
    }

    let value = rest.trim_start().strip_prefix('=')?.trim_start();
    Some(format!("{leading}{key} {value}"))
}

fn document_value(doc: &KdlDocument, key: &str) -> Option<String> {
    doc.get_arg(key).map(kdl_value_to_config_string)
}

fn document_values(doc: &KdlDocument, key: &str) -> Option<Vec<String>> {
    let values = doc
        .get_args(key)
        .into_iter()
        .map(kdl_value_to_config_string)
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    (!values.is_empty()).then_some(values)
}

fn dedup_strings(values: Vec<String>) -> Vec<String> {
    values.into_iter().fold(Vec::new(), |mut acc, value| {
        if !acc.contains(&value) {
            acc.push(value);
        }
        acc
    })
}

fn normalize_spaced_equals(value: &str) -> String {
    let mut normalized = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();
    let mut in_string = false;
    let mut escaped = false;

    while let Some(ch) = chars.next() {
        if in_string {
            normalized.push(ch);
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }

        if ch == '"' {
            in_string = true;
            normalized.push(ch);
            continue;
        }

        if ch == '=' {
            while normalized.ends_with(char::is_whitespace) {
                normalized.pop();
            }
            normalized.push('=');
            while matches!(chars.peek(), Some(next) if next.is_whitespace()) {
                chars.next();
            }
            continue;
        }

        normalized.push(ch);
    }

    normalized
}

fn parse_mode_style_update(node: &KdlNode) -> ModeStyleUpdate {
    ModeStyleUpdate {
        color: node_string_value(node, "color").and_then(Color::parse_hex),
        icon: node_string_value(node, "icon").map(parse_icon_value),
        label: node_string_value(node, "label").map(str::to_string),
    }
}

fn mode_from_name(name: &str) -> Option<InputMode> {
    match name {
        "locked" => Some(InputMode::Locked),
        "resize" => Some(InputMode::Resize),
        "pane" => Some(InputMode::Pane),
        "tab" => Some(InputMode::Tab),
        "scroll" => Some(InputMode::Scroll),
        "search" => Some(InputMode::Search),
        "enter_search" | "enter-search" => Some(InputMode::EnterSearch),
        "rename_tab" | "rename-tab" => Some(InputMode::RenameTab),
        "rename_pane" | "rename-pane" => Some(InputMode::RenamePane),
        "session" => Some(InputMode::Session),
        "move" => Some(InputMode::Move),
        "prompt" => Some(InputMode::Prompt),
        "tmux" => Some(InputMode::Tmux),
        _ => None,
    }
}

fn node_string_value<'a>(node: &'a KdlNode, key: &str) -> Option<&'a str> {
    node.get(key)
        .and_then(|entry| value_as_str(entry.value()))
        .or_else(|| {
            node.children()
                .and_then(|children| children.get_arg(key))
                .and_then(value_as_str)
        })
}

fn value_as_str(value: &KdlValue) -> Option<&str> {
    value.as_string()
}

fn kdl_value_to_config_string(value: &KdlValue) -> String {
    value
        .as_string()
        .map(str::to_string)
        .or_else(|| value.as_i64().map(|n| n.to_string()))
        .or_else(|| value.as_f64().map(|n| n.to_string()))
        .or_else(|| value.as_bool().map(|b| b.to_string()))
        .unwrap_or_else(|| value.to_string())
}

// ─── Icon value parser ────────────────────────────────────────────────────────

fn parse_icon_value(s: &str) -> String {
    if let Some(hex) = s.strip_prefix("U+") {
        if let Ok(codepoint) = u32::from_str_radix(hex, 16) {
            if let Some(c) = char::from_u32(codepoint) {
                return c.to_string();
            }
        }
    }
    s.to_string()
}

// ─── Status block parser ──────────────────────────────────────────────────────

fn parse_status_block(value: &str, cfg: &mut Config) {
    // KDL allows arbitrary nested children, so the standard parser is enough
    // here; we still try the spaced-equals fallback so users can write
    // `format = "%H:%M"` etc.
    let Some(doc) = value
        .parse::<KdlDocument>()
        .ok()
        .or_else(|| normalize_spaced_equals(value).parse::<KdlDocument>().ok())
    else {
        return;
    };

    for node in doc.nodes() {
        match node.name().value() {
            "date_time" => parse_date_time_node(node, cfg),
            "system_info" => {
                if let Some(block) = parse_system_info_node(node, &mut cfg.click_commands) {
                    cfg.system_info_assign(block);
                }
            }
            _ => {}
        }
    }
}

impl Config {
    fn system_info_assign(&mut self, block: SystemInfoBlock) {
        self.status.system_info = Some(block);
    }
}

fn parse_date_time_node(node: &KdlNode, cfg: &mut Config) {
    if let Some(s) = node_string_value(node, "visibility").and_then(Visibility::parse) {
        cfg.status.date_time.visibility = s;
    }
    if let Some(n) = node_usize_value(node, "min_cols") {
        cfg.status.date_time.min_cols = Some(n);
    }
    if let Some(s) = node_string_value(node, "symbol") {
        cfg.status.date_time.symbol = parse_icon_value(s);
    }
    if let Some(s) = node_string_value(node, "format") {
        cfg.status.date_time.format = s.to_string();
    }
    if let Some(cmd) = node_string_value(node, "on_click") {
        let idx = register_click_command(cmd.to_string(), &mut cfg.click_commands);
        cfg.status.date_time.on_click = Some(idx);
    }
}

fn parse_system_info_node(
    node: &KdlNode,
    click_commands: &mut Vec<String>,
) -> Option<SystemInfoBlock> {
    let mut block = SystemInfoBlock {
        separator: DEFAULT_WIDGET_SEPARATOR.to_string(),
        visibility: Visibility::default(),
        min_cols: None,
        widgets: Vec::new(),
    };

    if let Some(s) = node_string_value(node, "separator") {
        block.separator = s.to_string();
    }
    if let Some(s) = node_string_value(node, "visibility").and_then(Visibility::parse) {
        block.visibility = s;
    }
    if let Some(n) = node_usize_value(node, "min_cols") {
        block.min_cols = Some(n);
    }

    if let Some(children) = node.children() {
        for child in children.nodes() {
            if child.name().value() == "info" {
                if let Some(widget) = parse_info_node(child, click_commands) {
                    block.widgets.push(widget);
                }
            }
        }
    }

    block.widgets = dedupe_widgets_last_wins(block.widgets);

    Some(block)
}

fn parse_info_node(node: &KdlNode, click_commands: &mut Vec<String>) -> Option<InfoWidget> {
    // First positional argument is the widget id.
    let id = node
        .get(0_usize)
        .and_then(|entry| entry.value().as_string())
        .map(str::to_string)?;

    if !is_valid_widget_id(&id) {
        return None;
    }

    let kind = node_string_value(node, "type")
        .and_then(WidgetKind::parse)
        .unwrap_or(WidgetKind::Number);

    let command = node_string_value(node, "command")?.to_string();

    let interval = node_string_value(node, "interval")
        .and_then(parse_duration)
        .unwrap_or(Duration::ZERO);

    let default_symbol = node_string_value(node, "default")
        .map(parse_icon_value)
        .unwrap_or_default();

    let visibility = node_string_value(node, "visibility")
        .and_then(Visibility::parse)
        .unwrap_or_default();
    let min_cols = node_usize_value(node, "min_cols");

    let on_click = node_string_value(node, "on_click")
        .map(|cmd| register_click_command(cmd.to_string(), click_commands));

    let mut matches = Vec::new();
    if let Some(children) = node.children() {
        for child in children.nodes() {
            if child.name().value() == "match" {
                if let Some(rule) = parse_match_node(child, kind) {
                    matches.push(rule);
                }
            }
        }
    }

    Some(InfoWidget {
        id,
        kind,
        command,
        interval,
        default_symbol,
        matches,
        on_click,
        visibility,
        min_cols,
    })
}

fn parse_match_node(node: &KdlNode, kind: WidgetKind) -> Option<SymbolRule> {
    // The glyph is the first positional argument of the `match` node.
    let glyph = node
        .get(0_usize)
        .and_then(|entry| entry.value().as_string())
        .map(parse_icon_value)?;

    if let Some(entry) = node.get("val") {
        let value = match kind {
            WidgetKind::Number => MatchValue::Number(value_as_f64(entry.value())?),
            WidgetKind::String => MatchValue::String(entry.value().as_string()?.to_string()),
        };
        return Some(SymbolRule::Exact { value, glyph });
    }

    if kind == WidgetKind::String {
        // Range rules are meaningless for string widgets — silently ignore.
        return None;
    }

    let from = node.get("from").and_then(|e| value_as_f64(e.value()));
    let to = node.get("to").and_then(|e| value_as_f64(e.value()));
    if from.is_none() && to.is_none() {
        return None;
    }
    Some(SymbolRule::Range { from, to, glyph })
}

impl WidgetKind {
    fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "number" => Some(WidgetKind::Number),
            "string" => Some(WidgetKind::String),
            _ => None,
        }
    }
}

fn dedupe_widgets_last_wins(widgets: Vec<InfoWidget>) -> Vec<InfoWidget> {
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut reversed: Vec<InfoWidget> = Vec::with_capacity(widgets.len());
    for w in widgets.into_iter().rev() {
        if seen.insert(w.id.clone()) {
            reversed.push(w);
        }
    }
    reversed.reverse();
    reversed
}

fn is_valid_widget_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn register_click_command(cmd: String, registry: &mut Vec<String>) -> usize {
    registry.push(cmd);
    registry.len() - 1
}

/// Parse a duration string like `"5s"`, `"1m"`, `"30s"`, `"100ms"`, or `"0"`.
/// A bare integer is interpreted as seconds. Returns `None` on parse error.
pub fn parse_duration(s: &str) -> Option<Duration> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }

    let split_idx = s.find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '_'));
    let (num_part, unit) = match split_idx {
        Some(i) => (&s[..i], s[i..].trim()),
        None => (s, ""),
    };

    if num_part.is_empty() {
        return None;
    }

    let n: u64 = num_part.parse().ok()?;
    match unit {
        "" | "s" => Some(Duration::from_secs(n)),
        "ms" => Some(Duration::from_millis(n)),
        "m" => Some(Duration::from_secs(n.checked_mul(60)?)),
        "h" => Some(Duration::from_secs(n.checked_mul(3600)?)),
        _ => None,
    }
}

fn node_usize_value(node: &KdlNode, key: &str) -> Option<usize> {
    let from_value = |v: &KdlValue| -> Option<usize> {
        if let Some(s) = v.as_string() {
            return s.parse().ok();
        }
        if let Some(n) = v.as_i64() {
            if n >= 0 {
                return Some(n as usize);
            }
        }
        None
    };
    if let Some(entry) = node.get(key) {
        if let Some(n) = from_value(entry.value()) {
            return Some(n);
        }
    }
    node.children()
        .and_then(|children| children.get_arg(key))
        .and_then(from_value)
}

fn value_as_f64(value: &KdlValue) -> Option<f64> {
    if let Some(n) = value.as_f64() {
        return Some(n);
    }
    if let Some(n) = value.as_i64() {
        return Some(n as f64);
    }
    if let Some(s) = value.as_string() {
        return s.parse().ok();
    }
    None
}

// ─── Widget value resolution ──────────────────────────────────────────────────

/// Format a numeric widget value for `{value}` substitution. Rounds to 4
/// decimals so 73.000001 renders as `73`, then defers to Rust's `{}` for
/// display.
pub fn format_widget_number(v: f64) -> String {
    let rounded = (v * 10_000.0).round() / 10_000.0;
    if rounded.is_finite() && (rounded - rounded.round()).abs() < 1e-9 {
        format!("{}", rounded.round() as i64)
    } else {
        format!("{}", rounded)
    }
}

/// Resolve which symbol to render for a widget given the most recent value.
/// Returns the matching glyph, or the widget's `default_symbol` if no rule
/// matches. First match wins (across both `Exact` and `Range`).
pub fn resolve_widget_symbol<'a>(widget: &'a InfoWidget, value: &MatchValue) -> &'a str {
    for rule in &widget.matches {
        match rule {
            SymbolRule::Exact { value: rv, glyph } => {
                if match_value_equal(rv, value) {
                    return glyph;
                }
            }
            SymbolRule::Range { from, to, glyph } => {
                if let MatchValue::Number(n) = value {
                    let lo = from.unwrap_or(f64::NEG_INFINITY);
                    let hi = to.unwrap_or(f64::INFINITY);
                    if *n >= lo && *n <= hi {
                        return glyph;
                    }
                }
            }
        }
    }
    &widget.default_symbol
}

fn match_value_equal(a: &MatchValue, b: &MatchValue) -> bool {
    match (a, b) {
        (MatchValue::Number(x), MatchValue::Number(y)) => (x - y).abs() < 1e-9,
        (MatchValue::String(x), MatchValue::String(y)) => x == y,
        _ => false,
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_values() {
        let config = Config::default();
        assert_eq!(config.tab_max_width, 40);
        assert_eq!(config.tab_min_shrink_width, 20);
        assert!((config.tab_truncation_point - 0.4).abs() < f32::EPSILON);
        assert!(!config.tab_hide_single);
        assert_eq!(config.fullscreen_min_cols, 120);
        assert_eq!(config.default_session_name, "main");
        assert_eq!(
            config.project_markers,
            DEFAULT_PROJECT_MARKERS
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>()
        );
        assert_eq!(config.mode_styles.len(), 13);
        assert_eq!(
            config.mode_style(InputMode::Locked).unwrap(),
            &ModeStyle::new(Color::new(255, 102, 102), icons::MODE_LOCKED, "Locked")
        );
    }

    #[test]
    fn parse_legacy_overrides() {
        let mut map = BTreeMap::new();
        map.insert("tab_max_width".to_string(), "50".to_string());
        map.insert("tab_min_shrink_width".to_string(), "18".to_string());
        map.insert("tab_hide_single".to_string(), "true".to_string());
        map.insert("mode_color_locked".to_string(), "#00ff00".to_string());
        let config = Config::from_map(map);
        assert_eq!(config.tab_max_width, 50);
        assert_eq!(config.tab_min_shrink_width, 18);
        assert!(config.tab_hide_single);
        assert_eq!(
            config.mode_color(InputMode::Locked),
            Some(Color::new(0, 255, 0))
        );
    }

    #[test]
    fn parse_tabs_block_with_child_values() {
        let mut map = BTreeMap::new();
        map.insert(
            "tabs".to_string(),
            r#"
max_width 50
min_shrink_width 18
truncation_point 0.25
hide_single true
"#
            .to_string(),
        );

        let config = Config::from_map(map);

        assert_eq!(config.tab_max_width, 50);
        assert_eq!(config.tab_min_shrink_width, 18);
        assert!((config.tab_truncation_point - 0.25).abs() < f32::EPSILON);
        assert!(config.tab_hide_single);
    }

    #[test]
    fn tabs_project_markers_replace_defaults() {
        let mut map = BTreeMap::new();
        map.insert(
            "tabs".to_string(),
            r#"
project_markers ".git" "package.json" ".git"
"#
            .to_string(),
        );

        let config = Config::from_map(map);

        assert_eq!(config.project_markers, vec![".git", "package.json"]);
    }

    #[test]
    fn tabs_extra_project_markers_append_defaults() {
        let mut map = BTreeMap::new();
        map.insert(
            "tabs".to_string(),
            r#"
extra_project_markers ".mise.toml" ".git" ".tool-versions"
"#
            .to_string(),
        );

        let config = Config::from_map(map);

        assert!(config.project_markers.starts_with(
            &DEFAULT_PROJECT_MARKERS
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>()
        ));
        assert_eq!(config.project_markers.last().unwrap(), ".tool-versions");
        assert_eq!(
            config
                .project_markers
                .iter()
                .filter(|marker| marker.as_str() == ".git")
                .count(),
            1
        );
    }

    #[test]
    fn tabs_block_wins_over_legacy_tab_config() {
        let mut map = BTreeMap::new();
        map.insert("tab_max_width".to_string(), "50".to_string());
        map.insert("tab_min_shrink_width".to_string(), "18".to_string());
        map.insert("tab_truncation_point".to_string(), "0.25".to_string());
        map.insert("tab_hide_single".to_string(), "true".to_string());
        map.insert(
            "tabs".to_string(),
            r#"
max_width 40
min_shrink_width 20
truncation_point 0.4
hide_single false
"#
            .to_string(),
        );

        let config = Config::from_map(map);

        assert_eq!(config.tab_max_width, 40);
        assert_eq!(config.tab_min_shrink_width, 20);
        assert!((config.tab_truncation_point - 0.4).abs() < f32::EPSILON);
        assert!(!config.tab_hide_single);
    }

    #[test]
    fn parse_tabs_block_with_spaced_equals_child_values() {
        let mut map = BTreeMap::new();
        map.insert(
            "tabs".to_string(),
            r#"
max_width = 50
min_shrink_width = 18
truncation_point = 0.25
hide_single = true
"#
            .to_string(),
        );

        let config = Config::from_map(map);

        assert_eq!(config.tab_max_width, 50);
        assert_eq!(config.tab_min_shrink_width, 18);
        assert!((config.tab_truncation_point - 0.25).abs() < f32::EPSILON);
        assert!(config.tab_hide_single);
    }

    #[test]
    fn parse_modes_block_with_child_values() {
        let mut map = BTreeMap::new();
        map.insert(
            "modes".to_string(),
            r##"
locked {
  color "#fab387"
  icon "U+F0456"
  label "Passthrough"
}
"##
            .to_string(),
        );

        let config = Config::from_map(map);
        let style = config.mode_style(InputMode::Locked).unwrap();

        assert_eq!(style.color, Color::new(250, 179, 135));
        assert_eq!(style.icon, "\u{F0456}");
        assert_eq!(style.label, "Passthrough");
    }

    #[test]
    fn parse_modes_block_with_inline_properties() {
        let mut map = BTreeMap::new();
        map.insert(
            "modes".to_string(),
            r##"resize color="#f9e2af" icon="R" label="Resize Me""##.to_string(),
        );

        let config = Config::from_map(map);
        let style = config.mode_style(InputMode::Resize).unwrap();

        assert_eq!(style.color, Color::new(249, 226, 175));
        assert_eq!(style.icon, "R");
        assert_eq!(style.label, "Resize Me");
    }

    #[test]
    fn parse_modes_block_with_spaced_equals_properties() {
        let mut map = BTreeMap::new();
        map.insert(
            "modes".to_string(),
            r##"
locked {
  color = "#fab387"
  icon = "L"
  label = "Passthrough"
}
"##
            .to_string(),
        );

        let config = Config::from_map(map);
        let style = config.mode_style(InputMode::Locked).unwrap();

        assert_eq!(style.color, Color::new(250, 179, 135));
        assert_eq!(style.icon, "L");
        assert_eq!(style.label, "Passthrough");
    }

    #[test]
    fn modes_block_wins_over_legacy_mode_color() {
        let mut map = BTreeMap::new();
        map.insert("mode_color_locked".to_string(), "#00ff00".to_string());
        map.insert(
            "modes".to_string(),
            r##"locked color="#fab387" icon="L" label="Locked""##.to_string(),
        );

        let config = Config::from_map(map);

        assert_eq!(
            config.mode_color(InputMode::Locked),
            Some(Color::new(250, 179, 135))
        );
    }

    #[test]
    fn search_mode_updates_enter_search_unless_explicit() {
        let mut map = BTreeMap::new();
        map.insert(
            "modes".to_string(),
            r##"search color="#a6e3a1" icon="S" label="Find""##.to_string(),
        );

        let config = Config::from_map(map);

        assert_eq!(
            config.mode_style(InputMode::Search).unwrap(),
            config.mode_style(InputMode::EnterSearch).unwrap()
        );
    }

    #[test]
    fn enter_search_can_override_search_mode() {
        let mut map = BTreeMap::new();
        map.insert(
            "modes".to_string(),
            r##"
search color="#a6e3a1" icon="S" label="Find"
enter_search color="#f9e2af" icon="E" label="Enter"
"##
            .to_string(),
        );

        let config = Config::from_map(map);

        assert_eq!(
            config.mode_color(InputMode::Search),
            Some(Color::new(166, 227, 161))
        );
        assert_eq!(
            config.mode_color(InputMode::EnterSearch),
            Some(Color::new(249, 226, 175))
        );
        assert_eq!(config.mode_style(InputMode::EnterSearch).unwrap().icon, "E");
    }

    #[test]
    fn invalid_values_use_defaults() {
        let mut map = BTreeMap::new();
        map.insert("tab_max_width".to_string(), "not_a_number".to_string());
        map.insert(
            "tab_min_shrink_width".to_string(),
            "not_a_number".to_string(),
        );
        map.insert("mode_color_locked".to_string(), "invalid".to_string());
        let config = Config::from_map(map);
        assert_eq!(config.tab_max_width, 40);
        assert_eq!(config.tab_min_shrink_width, 20);
        assert_eq!(
            config.mode_color(InputMode::Locked),
            Some(Color::new(255, 102, 102))
        );
    }

    #[test]
    fn malformed_modes_block_uses_defaults() {
        let mut map = BTreeMap::new();
        map.insert("modes".to_string(), "locked { color".to_string());

        let config = Config::from_map(map);

        assert_eq!(
            config.mode_style(InputMode::Locked).unwrap(),
            &ModeStyle::new(Color::new(255, 102, 102), icons::MODE_LOCKED, "Locked")
        );
    }

    #[test]
    fn unknown_keys_ignored() {
        let mut map = BTreeMap::new();
        map.insert("nonexistent_key".to_string(), "value".to_string());
        let config = Config::from_map(map);
        assert_eq!(config.tab_max_width, 40);
    }

    #[test]
    fn parse_icon_codepoint() {
        assert_eq!(parse_icon_value("U+F05A9"), "\u{F05A9}");
    }

    #[test]
    fn parse_icon_literal() {
        assert_eq!(parse_icon_value("X"), "X");
    }

    // ── Status / widgets ──────────────────────────────────────────────────

    #[test]
    fn parse_duration_variants() {
        assert_eq!(parse_duration("0"), Some(Duration::ZERO));
        assert_eq!(parse_duration("5"), Some(Duration::from_secs(5)));
        assert_eq!(parse_duration("5s"), Some(Duration::from_secs(5)));
        assert_eq!(parse_duration("100ms"), Some(Duration::from_millis(100)));
        assert_eq!(parse_duration("2m"), Some(Duration::from_secs(120)));
        assert_eq!(parse_duration("1h"), Some(Duration::from_secs(3600)));
        assert_eq!(parse_duration("bogus"), None);
        assert_eq!(parse_duration(""), None);
    }

    #[test]
    fn format_widget_number_rounds_and_strips() {
        assert_eq!(format_widget_number(73.0), "73");
        assert_eq!(format_widget_number(73.00000001), "73");
        assert_eq!(format_widget_number(73.5), "73.5");
        assert_eq!(format_widget_number(73.12345), "73.1235");
        assert_eq!(format_widget_number(-1.0), "-1");
    }

    #[test]
    fn parse_status_block_date_time() {
        let mut map = BTreeMap::new();
        map.insert(
            "status".to_string(),
            r##"
date_time {
    visibility "always"
    min_cols 80
    symbol "U+F00F0"
    format "%H:%M"
    on_click "echo clicked"
}
"##
            .to_string(),
        );

        let cfg = Config::from_map(map);

        assert_eq!(cfg.status.date_time.visibility, Visibility::Always);
        assert_eq!(cfg.status.date_time.min_cols, Some(80));
        assert_eq!(cfg.status.date_time.symbol, "\u{F00F0}");
        assert_eq!(cfg.status.date_time.format, "%H:%M");
        assert_eq!(cfg.status.date_time.on_click, Some(0));
        assert_eq!(cfg.click_commands, vec!["echo clicked".to_string()]);
    }

    #[test]
    fn date_time_defaults_when_status_absent() {
        let cfg = Config::from_map(BTreeMap::new());
        assert_eq!(cfg.status.date_time.visibility, Visibility::Fullscreen);
        assert_eq!(cfg.status.date_time.format, DEFAULT_DATE_TIME_FORMAT);
        assert_eq!(cfg.status.date_time.symbol, icons::CALENDAR);
        assert!(cfg.status.system_info.is_none());
    }

    #[test]
    fn legacy_icon_calendar_propagates_to_date_time_default() {
        let mut map = BTreeMap::new();
        map.insert("icon_calendar".to_string(), "Z".to_string());
        let cfg = Config::from_map(map);
        assert_eq!(cfg.status.date_time.symbol, "Z");
    }

    #[test]
    fn explicit_status_symbol_overrides_legacy_icon_calendar() {
        let mut map = BTreeMap::new();
        map.insert("icon_calendar".to_string(), "Z".to_string());
        map.insert(
            "status".to_string(),
            r##"date_time symbol="X""##.to_string(),
        );
        let cfg = Config::from_map(map);
        assert_eq!(cfg.status.date_time.symbol, "X");
    }

    #[test]
    fn parse_system_info_with_widgets() {
        let mut map = BTreeMap::new();
        map.insert(
            "status".to_string(),
            r##"
system_info {
    separator " | "
    visibility "always"
    info "wifi" {
        type "number"
        command "echo 50"
        interval "10s"
        default ""
        match val=-1 ""
        match to=0 ""
        match from=1 to=25 ""
        match from=26 to=75 ""
        match from=76 ""
        on_click "echo wifi-clicked"
    }
    info "kbd" {
        type "string"
        command "echo us"
        match val="us" "  US"
        match val="es" "  ES"
    }
}
"##
            .to_string(),
        );

        let cfg = Config::from_map(map);
        let block = cfg.status.system_info.as_ref().expect("system_info parsed");
        assert_eq!(block.separator, " | ");
        assert_eq!(block.visibility, Visibility::Always);
        assert_eq!(block.widgets.len(), 2);

        let wifi = &block.widgets[0];
        assert_eq!(wifi.id, "wifi");
        assert_eq!(wifi.kind, WidgetKind::Number);
        assert_eq!(wifi.command, "echo 50");
        assert_eq!(wifi.interval, Duration::from_secs(10));
        assert_eq!(wifi.default_symbol, "");
        assert_eq!(wifi.matches.len(), 5);
        assert!(wifi.on_click.is_some());

        let kbd = &block.widgets[1];
        assert_eq!(kbd.id, "kbd");
        assert_eq!(kbd.kind, WidgetKind::String);
        assert_eq!(kbd.matches.len(), 2);
        assert!(matches!(
            &kbd.matches[0],
            SymbolRule::Exact {
                value: MatchValue::String(s),
                ..
            } if s == "us"
        ));
    }

    #[test]
    fn widget_resolves_first_matching_rule() {
        let widget = InfoWidget {
            id: "w".into(),
            kind: WidgetKind::Number,
            command: "true".into(),
            interval: Duration::ZERO,
            default_symbol: "D".into(),
            matches: vec![
                SymbolRule::Exact {
                    value: MatchValue::Number(-1.0),
                    glyph: "X".into(),
                },
                SymbolRule::Range {
                    from: Some(0.0),
                    to: Some(50.0),
                    glyph: "L".into(),
                },
                SymbolRule::Range {
                    from: Some(0.0),
                    to: Some(100.0),
                    glyph: "WIDE".into(),
                },
                SymbolRule::Range {
                    from: Some(51.0),
                    to: None,
                    glyph: "H".into(),
                },
            ],
            on_click: None,
            visibility: Visibility::Always,
            min_cols: None,
        };

        assert_eq!(
            resolve_widget_symbol(&widget, &MatchValue::Number(-1.0)),
            "X"
        );
        assert_eq!(
            resolve_widget_symbol(&widget, &MatchValue::Number(25.0)),
            "L"
        );
        // 75 is outside L's range; first WIDE match wins over H (declaration order)
        assert_eq!(
            resolve_widget_symbol(&widget, &MatchValue::Number(75.0)),
            "WIDE"
        );
        // 200 is outside both ranges, hits H (51..)
        assert_eq!(
            resolve_widget_symbol(&widget, &MatchValue::Number(200.0)),
            "H"
        );
    }

    #[test]
    fn widget_returns_default_when_no_rule_matches() {
        let widget = InfoWidget {
            id: "w".into(),
            kind: WidgetKind::Number,
            command: "true".into(),
            interval: Duration::ZERO,
            default_symbol: "D".into(),
            matches: vec![SymbolRule::Range {
                from: Some(10.0),
                to: Some(20.0),
                glyph: "Z".into(),
            }],
            on_click: None,
            visibility: Visibility::Always,
            min_cols: None,
        };
        assert_eq!(
            resolve_widget_symbol(&widget, &MatchValue::Number(5.0)),
            "D"
        );
    }

    #[test]
    fn duplicate_widget_ids_keep_last() {
        let mut map = BTreeMap::new();
        map.insert(
            "status".to_string(),
            r##"
system_info {
    info "x" {
        command "first"
    }
    info "x" {
        command "second"
    }
}
"##
            .to_string(),
        );
        let cfg = Config::from_map(map);
        let block = cfg.status.system_info.unwrap();
        assert_eq!(block.widgets.len(), 1);
        assert_eq!(block.widgets[0].command, "second");
    }

    #[test]
    fn invalid_widget_id_is_skipped() {
        let mut map = BTreeMap::new();
        map.insert(
            "status".to_string(),
            r##"
system_info {
    info "bad id!" {
        command "x"
    }
    info "good_id" {
        command "y"
    }
}
"##
            .to_string(),
        );
        let cfg = Config::from_map(map);
        let block = cfg.status.system_info.unwrap();
        assert_eq!(block.widgets.len(), 1);
        assert_eq!(block.widgets[0].id, "good_id");
    }

    #[test]
    fn widget_without_command_is_skipped() {
        let mut map = BTreeMap::new();
        map.insert(
            "status".to_string(),
            r##"
system_info {
    info "no_cmd" {
        type "number"
    }
}
"##
            .to_string(),
        );
        let cfg = Config::from_map(map);
        let block = cfg.status.system_info.unwrap();
        assert!(block.widgets.is_empty());
    }

    #[test]
    fn malformed_status_block_uses_defaults() {
        let mut map = BTreeMap::new();
        map.insert("status".to_string(), "date_time { format".to_string());
        let cfg = Config::from_map(map);
        assert_eq!(cfg.status.date_time.format, DEFAULT_DATE_TIME_FORMAT);
        assert!(cfg.status.system_info.is_none());
    }

    #[test]
    fn range_rules_silently_dropped_for_string_widgets() {
        let mut map = BTreeMap::new();
        map.insert(
            "status".to_string(),
            r##"
system_info {
    info "kbd" {
        type "string"
        command "echo us"
        match from=0 to=10 "ignored"
        match val="us" "  US"
    }
}
"##
            .to_string(),
        );
        let cfg = Config::from_map(map);
        let widgets = &cfg.status.system_info.unwrap().widgets;
        assert_eq!(widgets[0].matches.len(), 1);
    }
}
