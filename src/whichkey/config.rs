//! Plugin configuration parsing.

use std::collections::BTreeMap;

use zellij_tile::prelude::InputMode;

use crate::shared::geometry::{Anchor, HAlign, Padding, VAlign, WidthMode};
use crate::whichkey::labels::{
    format_key_compact, parse_chord, parse_chord_to_key, Group, LabelSpec, Labels, ModeLabels,
};
use crate::whichkey::modes::{group_members, mode_color, mode_icon, str_to_mode};
use crate::whichkey::theme::parse_color;

/// Grid fill order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortBy {
    /// Fill left→right, then wrap to the next row.
    Row,
    /// Fill top→bottom, then move to the next column.
    Column,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    // Layout.
    pub sort_by: SortBy,
    /// Max entry rows per page (body only; chrome is extra). Overflow paginates.
    pub max_height: usize,

    // Geometry.
    pub width: WidthMode,
    pub anchor: Anchor,
    /// Outer margin: cells between the screen edge and the panel's frame.
    pub margin: Padding,
    /// Inner padding: cells between the frame and the content (text).
    pub padding: Padding,

    // Footer paging-hint display overrides (auto-discovered when unset).
    pub next_page_key: Option<String>,
    pub prev_page_key: Option<String>,
    /// Footer hint for the `wk_toggle_pane` key (the chord bound to the toggle
    /// pipe). The host strips pipe names from the keymap, so it can't be
    /// auto-discovered; set it to surface a `<glyph> hide` footer affordance.
    pub toggle_key: Option<String>,
    /// Footer hint for the `wk_go_back` key (the chord bound to the back pipe).
    /// Like the others its pipe name is stripped from the keymap, so set it to
    /// surface a `<glyph> back` affordance — shown only when there's somewhere
    /// to go back to (a non-empty mode trail).
    pub back_key: Option<String>,

    /// User-defined labels, keyed by canonical key chord. Overrides auto labels
    /// for any binding and is the *only* way to surface pipe bindings (which
    /// the host delivers without name/payload). Each may carry an optional icon.
    pub labels: Labels,

    /// Glyph drawn between a binding's keys and its icon/description (the
    /// `<keys> <sep> <icon?> <desc>` row layout). Defaults to `➜`.
    pub binding_separator: String,

    /// Per-mode title-glyph overrides (e.g. `Tmux` → a custom symbol). Falls
    /// back to the builtin [`mode_icon`] when a mode is absent.
    pub mode_symbols: BTreeMap<InputMode, String>,
    /// Per-mode symbol-color overrides, stored as ready-to-emit SGR foreground
    /// sequences. Falls back to the builtin [`mode_color`] palette when absent.
    pub mode_colors: BTreeMap<InputMode, String>,
    /// Per-mode display-name overrides (e.g. `Tmux` → `Command`). Used wherever
    /// a mode is named, such as `Command …` switch entries.
    pub mode_labels: ModeLabels,

    /// Binding groups that render contiguously in the configured member order
    /// (anchored at each group's first member), optionally scoped to one or more
    /// modes.
    pub groups: Vec<Group>,

    /// When set, append per-page layout diagnostics (column widths + each
    /// entry's key string and char count) to this path on every rebuild. The
    /// path is resolved inside the plugin's WASI sandbox — use a preopened root
    /// such as `/host/...` (the dir Zellij was launched from). Debug only.
    pub debug_log: Option<String>,

    /// Lightweight state-transition trace. Unlike `debug_log`, this does not
    /// emit layout diagnostics, so it is suitable for tracing cross-tab state.
    pub state_log: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            sort_by: SortBy::Row,
            max_height: 8,
            width: WidthMode::Single,
            anchor: Anchor::default(),
            margin: Padding::default(),
            // Snug horizontal breathing room inside the frame; no vertical inset.
            padding: Padding {
                top: 0,
                right: 1,
                bottom: 0,
                left: 1,
            },
            next_page_key: None,
            prev_page_key: None,
            toggle_key: None,
            back_key: None,
            labels: Labels::new(),
            binding_separator: "\u{279C}".to_string(), // ➜

            mode_symbols: BTreeMap::new(),
            mode_colors: BTreeMap::new(),
            mode_labels: ModeLabels::new(),
            groups: Vec::new(),
            debug_log: None,
            state_log: None,
        }
    }
}

impl Config {
    /// Build a `Config` from the raw KDL of a `which_key { … }` block as
    /// forwarded by the Bar through the shared state. Reconstructs the flat
    /// `BTreeMap` shape [`Config::from_map`] expects: a leaf node maps to its
    /// first argument; a container node (`labels`/`groups`) maps to its
    /// children serialized back to KDL (so the existing block parsers apply).
    pub fn from_block(block: &str) -> Self {
        let Some(doc) = crate::shared::kdl::parse_config_document(block, &[]) else {
            return Config::default();
        };
        let mut map: BTreeMap<String, String> = BTreeMap::new();
        for node in doc.nodes() {
            let name = node.name().value().to_string();
            let value = if let Some(children) = node.children() {
                children.to_string()
            } else {
                node.entries()
                    .iter()
                    .find(|entry| entry.name().is_none())
                    .map(|entry| crate::shared::kdl::kdl_value_to_config_string(entry.value()))
                    .unwrap_or_default()
            };
            map.insert(name, value);
        }
        Config::from_map(&map)
    }

    pub fn from_map(map: &BTreeMap<String, String>) -> Self {
        let mut config = Config::default();

        if let Some(v) = map.get("sort_by") {
            config.sort_by = parse_sort_by(v).unwrap_or(config.sort_by);
        }
        if let Some(v) = map.get("max_height").and_then(|s| s.parse().ok()) {
            config.max_height = v;
        }
        if let Some(v) = map.get("width") {
            config.width = parse_width(v);
        }
        if let Some(v) = map.get("anchor") {
            config.anchor = parse_anchor(v);
        }
        if let Some(v) = map.get("margin") {
            config.margin = parse_padding(v);
        }
        if let Some(v) = map.get("padding") {
            config.padding = parse_padding(v);
        }
        config.next_page_key = map.get("next_page_key").and_then(|s| page_key_glyph(s));
        config.prev_page_key = map.get("prev_page_key").and_then(|s| page_key_glyph(s));
        config.toggle_key = map.get("toggle_key").and_then(|s| page_key_glyph(s));
        config.back_key = map.get("back_key").and_then(|s| page_key_glyph(s));
        if let Some(v) = map.get("binding_separator") {
            let v = v.trim();
            if !v.is_empty() {
                config.binding_separator = v.to_string();
            }
        }
        if let Some(block) = map.get("labels") {
            config.labels = parse_labels_block(block);
        }
        if let Some(block) = map.get("modes") {
            let (symbols, colors, labels) = parse_modes_block(block);
            config.mode_symbols = symbols;
            config.mode_colors = colors;
            config.mode_labels = labels;
        }
        if let Some(block) = map.get("groups") {
            config.groups = parse_groups_block(block);
        }
        config.debug_log = map
            .get("debug_log")
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        config.state_log = map
            .get("state_log")
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        config
    }

    /// The title glyph for `mode`: the configured override if present, else the
    /// builtin [`mode_icon`].
    pub fn symbol(&self, mode: InputMode) -> String {
        self.mode_symbols
            .get(&mode)
            .cloned()
            .unwrap_or_else(|| mode_icon(mode).to_string())
    }

    /// The SGR foreground sequence tinting `mode`'s title glyph: the configured
    /// override if present, else the builtin [`mode_color`] palette default.
    pub fn symbol_color(&self, mode: InputMode) -> String {
        self.mode_colors
            .get(&mode)
            .cloned()
            .unwrap_or_else(|| parse_color(mode_color(mode)).unwrap_or_default())
    }

    /// Adopt the mode palette published by the Bar through the shared state, so
    /// the panel renders the same glyphs/colors/labels as the status bar
    /// without carrying its own `modes` config block. Empty fields are skipped
    /// so a partial entry still falls back to the builtin palette.
    pub fn apply_palette(
        &mut self,
        palette: &std::collections::BTreeMap<String, crate::shared::state::ModePalette>,
    ) {
        for (name, entry) in palette {
            let Some(mode) = crate::shared::state::str_to_mode(name) else {
                continue;
            };
            if !entry.icon.is_empty() {
                self.mode_symbols.insert(mode, entry.icon.clone());
            }
            if !entry.color.is_empty() {
                self.mode_colors.insert(mode, entry.color.clone());
            }
            if !entry.label.is_empty() {
                self.mode_labels.insert(mode, entry.label.clone());
            }
        }
    }
}

/// Render a paging-key chord (e.g. `"Ctrl d"`) as its compact footer glyphs
/// (`󰘴D`).
fn page_key_glyph(chord: &str) -> Option<String> {
    parse_chord_to_key(chord).map(|k| format_key_compact(&k))
}

// --- labels -----------------------------------------------------------------

/// Parse the `labels { ... }` block. Like the `modes` block, Zellij hands us
/// the children as a stringified KDL blob (one node per line) and each line is a
/// KDL node using **named properties**, mirroring `zj-hud`'s style:
///
/// ```kdl
/// labels {
///     wk binding="Ctrl h" desc="focus left"  icon="\u{F0312}"
///     wk binding="Alt y"  desc="copy pwd (abs)"   // icon optional
/// }
/// ```
///
/// The node name (`wk`) is a fixed marker and ignored. `binding` (the chord,
/// normalized via [`parse_chord`] so it matches the live keymap) and `desc` are
/// required; `mode`, `icon` and `icon_color` are optional. `mode` scopes the
/// label to one or more Zellij modes — a whitespace/comma-separated list with
/// the same grammar as the `groups` block (e.g. `mode="pane"`,
/// `mode="scroll search"`, or the `search` alias → Search+EnterSearch), parsed
/// by [`parse_mode_list`]. Without `mode` the label is modeless and applies in
/// every mode (a mode-scoped label for the same chord wins where both exist).
/// This lets one chord read differently per mode, e.g.
///
/// ```kdl
/// labels {
///     wk binding="Ctrl h" desc="focus left"            // modeless fallback
///     wk mode="pane" binding="r" desc="rename pane"
///     wk mode="tab"  binding="r" desc="rename tab"
///     wk mode="scroll search" binding="u" desc="half page up"  // both modes
/// }
/// ```
///
/// `icon_color` is a `#RGB`/`#RRGGBB` hex or 256-color index (stored as an SGR
/// sequence; an unparseable color is dropped, so the icon keeps the label
/// color). Properties are order-independent. A `mode` that resolves to no modes
/// drops the entry (rather than silently making it modeless).
fn parse_labels_block(block: &str) -> Labels {
    let mut out = Labels::new();
    let Some(doc) = crate::shared::kdl::parse_config_document(block, &[]) else {
        return out;
    };
    for node in doc.nodes() {
        // Node name (`wk`) is a fixed marker, ignored. Read named properties via
        // the kdl crate, which decodes `\u{…}`/`\n`/`\"` escapes for us.
        let (Some(binding), Some(desc)) = (node_prop(node, "binding"), node_prop(node, "desc"))
        else {
            continue;
        };
        let Some(canonical) = parse_chord(binding) else {
            continue;
        };
        // `mode` may list one or more modes (whitespace/comma separated, with
        // the `search` group alias), mirroring the `groups` block. Absent `mode`
        // is modeless (applies everywhere); an explicit `mode` that resolves to
        // no modes drops the entry rather than silently making it modeless.
        let modes: Option<Vec<InputMode>> = node_prop(node, "mode").map(parse_mode_list);
        if matches!(&modes, Some(m) if m.is_empty()) {
            continue;
        }
        let spec = LabelSpec {
            desc: desc.trim().to_string(),
            icon: node_prop(node, "icon").map(|s| s.trim().to_string()),
            icon_color: node_prop(node, "icon_color").and_then(parse_color),
        };
        match modes {
            // Scope the label to every listed mode (the same spec applies in each).
            Some(modes) => {
                for mode in modes {
                    out.insert(Some(mode), canonical.clone(), spec.clone());
                }
            }
            None => out.insert(None, canonical, spec),
        }
    }
    out
}

/// Read a named property off a KDL node as a trimmed-non-empty string. The kdl
/// crate decodes string escapes (`\u{…}`, `\n`, `\"`, …) for us.
fn node_prop<'a>(node: &'a kdl::KdlNode, key: &str) -> Option<&'a str> {
    node.get(key)
        .and_then(|entry| entry.value().as_string())
        .filter(|s| !s.trim().is_empty())
}

// --- modes ------------------------------------------------------------------

/// Parse the `modes { ... }` block: per-mode `icon`, `color`, and `label`
/// overrides. Like [`parse_labels_block`], Zellij hands us the children as a
/// stringified KDL blob (one node per line); each line is a KDL node using
/// **named properties**, mirroring `zj-hud`'s `ModeStyle`:
///
/// ```kdl
/// modes {
///     tmux color="#CC66FF" icon="\u{F0633}" label="Command"
///     pane color="#89B4FA" label="Pane"   // keep the builtin glyph
/// }
/// ```
///
/// Properties are order-independent and all optional — an omitted (or empty)
/// property keeps the builtin default, so you can change just one. `color` is a
/// hex string (`#RGB`/`#RRGGBB`) or 256-color index (unparseable colors are
/// ignored); `icon` (alias `symbol`) is the title glyph. The node name may be a
/// single mode (`pane`) or the `search` group alias, which applies to both
/// search phases.
fn parse_modes_block(
    block: &str,
) -> (
    BTreeMap<InputMode, String>,
    BTreeMap<InputMode, String>,
    ModeLabels,
) {
    let mut symbols = BTreeMap::new();
    let mut colors = BTreeMap::new();
    let mut labels = ModeLabels::new();
    let Some(doc) = crate::shared::kdl::parse_config_document(block, &[]) else {
        return (symbols, colors, labels);
    };
    for node in doc.nodes() {
        let name = node.name().value();
        let modes: Vec<InputMode> = group_members(name)
            .map(<[InputMode]>::to_vec)
            .or_else(|| str_to_mode(name).map(|m| vec![m]))
            .unwrap_or_default();
        if modes.is_empty() {
            continue;
        }

        let icon = node_prop(node, "icon").or_else(|| node_prop(node, "symbol"));
        let color = node_prop(node, "color").and_then(parse_color);
        let label = node_prop(node, "label");

        for mode in modes {
            if let Some(icon) = icon {
                symbols.insert(mode, icon.to_string());
            }
            if let Some(sgr) = &color {
                colors.insert(mode, sgr.clone());
            }
            if let Some(label) = label {
                labels.insert(mode, label.trim().to_string());
            }
        }
    }
    (symbols, colors, labels)
}

// --- groups -----------------------------------------------------------------

/// Parse the `groups { ... }` block. Each line is
/// `<id> [mode="<spec>"] "<chord>", "<chord>", …` where `<id>` is an arbitrary
/// leading handle (only ties the list together; it is *not* displayed — use
/// `wk`, the group name, or anything) and the remaining quoted tokens are the
/// member chords. Commas are optional separators. Chords are normalized via
/// [`parse_chord`] so they match the live keymap; unparseable members are
/// skipped.
///
/// An optional `mode="<spec>"` property scopes the group to one or more modes;
/// without it the group is modeless (applies in every mode). The spec is
/// whitespace/comma-separated mode names or group aliases (e.g.
/// `mode="pane tab"`, `mode="search"` → Search+EnterSearch), resolved by
/// [`parse_mode_list`] (additive-only). A `mode` that resolves to nothing
/// drops the group (rather than making it silently modeless).
///
/// Example block (as authored):
/// ```kdl
/// groups {
///     focus "Ctrl up" "Ctrl down" "Ctrl left" "Ctrl right"   // modeless
///     edits mode="pane" "r" "n" "x"                           // Pane mode only
/// }
/// ```
fn parse_groups_block(block: &str) -> Vec<Group> {
    let mut out = Vec::new();
    // KDL v1 (the `kdl` crate) uses whitespace, not commas, to separate node
    // arguments; the group grammar accepts commas as optional separators, so
    // fold them to spaces before handing the blob to the parser. No quoted
    // value in a group line ever contains a comma, so this is lossless.
    let normalized = block.replace(',', " ");
    let Some(doc) = crate::shared::kdl::parse_config_document(&normalized, &[]) else {
        return out;
    };
    for node in doc.nodes() {
        // The node name is the group handle — not displayed, ignored.
        //
        // `modes` is `Some` iff a `mode=` property was present (so an explicit
        // `mode` that resolves to nothing can drop the group, vs. absent `mode`
        // meaning modeless).
        let modes: Option<Vec<InputMode>> = node
            .get("mode")
            .and_then(|entry| entry.value().as_string())
            .map(parse_mode_list);

        // Positional (unnamed) string arguments are the member chords.
        let members: Vec<String> = node
            .entries()
            .iter()
            .filter(|entry| entry.name().is_none())
            .filter_map(|entry| entry.value().as_string())
            .filter_map(|chord| parse_chord(chord.trim()))
            .collect();

        // An explicit `mode` that resolved to no modes drops the group.
        if matches!(&modes, Some(m) if m.is_empty()) {
            continue;
        }
        if !members.is_empty() {
            out.push(Group {
                modes: modes.unwrap_or_default(),
                members,
            });
        }
    }
    out
}

/// Resolve a whitespace/comma-separated list of mode names or group aliases
/// into a deduplicated, additive set of modes (no negation). Unknown tokens are
/// ignored.
fn parse_mode_list(spec: &str) -> Vec<InputMode> {
    let mut out: Vec<InputMode> = Vec::new();
    for tok in spec.split(|c: char| c == ',' || c.is_whitespace()) {
        let t = tok.trim();
        if t.is_empty() {
            continue;
        }
        let resolved = group_members(t)
            .map(<[InputMode]>::to_vec)
            .or_else(|| str_to_mode(t).map(|m| vec![m]));
        if let Some(modes) = resolved {
            for m in modes {
                if !out.contains(&m) {
                    out.push(m);
                }
            }
        }
    }
    out
}

// --- layout / geometry parsing ---------------------------------------------

fn parse_sort_by(spec: &str) -> Option<SortBy> {
    match spec.trim().to_ascii_lowercase().as_str() {
        "row" | "rows" => Some(SortBy::Row),
        "column" | "columns" | "col" => Some(SortBy::Column),
        _ => None,
    }
}

fn parse_width(spec: &str) -> WidthMode {
    let spec = spec.trim().to_ascii_lowercase();
    match spec.as_str() {
        "single" | "auto" => WidthMode::Single,
        "fill" => WidthMode::Fill,
        other => {
            if let Ok(percent) = other.parse::<f32>() {
                if (0.1..1.0).contains(&percent) {
                    return WidthMode::Percent((percent * 100.0).round() as u16);
                }
            }
            other
                .parse::<usize>()
                .ok()
                .filter(|n| *n > 1)
                .map(WidthMode::Fixed)
                .unwrap_or(WidthMode::Single)
        }
    }
}

/// `top|bottom|left|right|center`, `+`/space/comma separated. Omitted axes
/// default to center; `center` and unknown tokens are no-ops.
pub(crate) fn parse_anchor(spec: &str) -> Anchor {
    let mut v = VAlign::Center;
    let mut h = HAlign::Center;
    for raw in spec.split(|c: char| c == '+' || c.is_whitespace() || c == ',') {
        match raw.trim().to_ascii_lowercase().as_str() {
            "top" => v = VAlign::Top,
            "bottom" => v = VAlign::Bottom,
            "left" => h = HAlign::Left,
            "right" => h = HAlign::Right,
            _ => {}
        }
    }
    Anchor { v, h }
}

/// CSS-style padding shorthand (1, 2, 3, or 4 comma/space separated values).
pub(crate) fn parse_padding(spec: &str) -> Padding {
    let vals: Vec<usize> = spec
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|s| !s.trim().is_empty())
        .filter_map(|s| s.trim().parse::<usize>().ok())
        .collect();
    match vals.as_slice() {
        [all] => Padding {
            top: *all,
            right: *all,
            bottom: *all,
            left: *all,
        },
        [v, h] => Padding {
            top: *v,
            right: *h,
            bottom: *v,
            left: *h,
        },
        [t, h, b] => Padding {
            top: *t,
            right: *h,
            bottom: *b,
            left: *h,
        },
        [t, r, b, l, ..] => Padding {
            top: *t,
            right: *r,
            bottom: *b,
            left: *l,
        },
        [] => Padding::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_block_parses_forwarded_which_key_kdl() {
        // Mirrors the blob the Bar forwards via shared state: scalars plus the
        // nested `labels`/`groups` containers must round-trip through from_block.
        let block = r#"
            sort_by "column"
            max_height 5
            width "fill"
            anchor "top+left"
            margin "1,2,1,2"
            padding "0,3,0,3"
            binding_separator "|"
            labels {
                wk binding="Ctrl h" desc="focus left"
            }
            groups {
                focus "Ctrl up" "Ctrl down"
            }
        "#;
        let cfg = Config::from_block(block);
        assert_eq!(cfg.sort_by, SortBy::Column);
        assert_eq!(cfg.max_height, 5);
        assert_eq!(cfg.width, WidthMode::Fill);
        assert_eq!(
            cfg.anchor,
            Anchor {
                v: VAlign::Top,
                h: HAlign::Left
            }
        );
        assert_eq!(cfg.binding_separator, "|");
        assert_ne!(
            cfg.labels,
            Config::default().labels,
            "labels block should round-trip"
        );
        assert_eq!(cfg.groups.len(), 1, "groups block should round-trip");
    }

    #[test]
    fn from_block_empty_yields_defaults() {
        assert_eq!(Config::from_block(""), Config::default());
    }

    #[test]
    fn anchor_omitted_axis_defaults_to_center() {
        assert_eq!(
            parse_anchor("bottom+right"),
            Anchor {
                v: VAlign::Bottom,
                h: HAlign::Right
            }
        );
        assert_eq!(
            parse_anchor("bottom+center"),
            Anchor {
                v: VAlign::Bottom,
                h: HAlign::Center
            }
        );
        assert_eq!(
            parse_anchor("top"),
            Anchor {
                v: VAlign::Top,
                h: HAlign::Center
            }
        );
        assert_eq!(
            parse_anchor("right"),
            Anchor {
                v: VAlign::Center,
                h: HAlign::Right
            }
        );
        assert_eq!(
            parse_anchor("center"),
            Anchor {
                v: VAlign::Center,
                h: HAlign::Center
            }
        );
    }

    #[test]
    fn padding_css_shorthand() {
        assert_eq!(
            parse_padding("0,2,1,0"),
            Padding {
                top: 0,
                right: 2,
                bottom: 1,
                left: 0
            }
        );
        assert_eq!(
            parse_padding("3"),
            Padding {
                top: 3,
                right: 3,
                bottom: 3,
                left: 3
            }
        );
        assert_eq!(
            parse_padding("1 2"),
            Padding {
                top: 1,
                right: 2,
                bottom: 1,
                left: 2
            }
        );
        assert_eq!(
            parse_padding("1,2,3"),
            Padding {
                top: 1,
                right: 2,
                bottom: 3,
                left: 2
            }
        );
    }

    #[test]
    fn width_and_sort() {
        assert_eq!(parse_width("fill"), WidthMode::Fill);
        assert_eq!(parse_width("single"), WidthMode::Single);
        assert_eq!(parse_width("auto"), WidthMode::Single);
        assert_eq!(parse_width("0.5"), WidthMode::Percent(50));
        assert_eq!(parse_width("0.99"), WidthMode::Percent(99));
        assert_eq!(parse_width("40"), WidthMode::Fixed(40));
        assert_eq!(parse_width("1"), WidthMode::Single);
        assert_eq!(parse_sort_by("column"), Some(SortBy::Column));
        assert_eq!(parse_sort_by("row"), Some(SortBy::Row));
    }

    #[test]
    fn labels_block_parses_kdl_blob() {
        // Mirrors the stringified children Zellij hands us: one `wk binding=…
        // desc=… [icon=…]` node per line, indented; icon via a \u{…} escape.
        let blob = concat!(
            "    wk binding=\"Ctrl h\" desc=\"focus left\" icon=\"\\u{F0312}\" icon_color=\"#89B4FA\"\n",
            "    wk binding=\"ctrl+shift+k\" desc=\"resize up\"\n",
            "    wk binding=\"Y\" desc=\"copy pwd\"\n",
            "    wk binding=\"alt y\" desc=\"copy pwd (abs)\"\n",
        );
        let labels = parse_labels_block(blob);
        // These entries omit `mode`, so they're modeless — any mode resolves them.
        let ctrl_h = labels.lookup(InputMode::Normal, "ctrl+h").unwrap();
        assert_eq!(ctrl_h.desc, "focus left");
        assert_eq!(ctrl_h.icon.as_deref(), Some("\u{F0312}"));
        assert_eq!(
            ctrl_h.icon_color.as_deref(),
            Some("\u{1b}[38;2;137;180;250m")
        );
        // No icon / icon_color props → None.
        assert_eq!(
            labels.lookup(InputMode::Pane, "ctrl+shift+k").unwrap().desc,
            "resize up"
        );
        assert_eq!(
            labels.lookup(InputMode::Pane, "ctrl+shift+k").unwrap().icon,
            None
        );
        assert_eq!(
            labels
                .lookup(InputMode::Pane, "ctrl+shift+k")
                .unwrap()
                .icon_color,
            None
        );
        // `Y` normalizes to Shift+y.
        assert_eq!(
            labels.lookup(InputMode::Tab, "shift+y").unwrap().desc,
            "copy pwd"
        );
        assert_eq!(
            labels.lookup(InputMode::Tab, "alt+y").unwrap().desc,
            "copy pwd (abs)"
        );
    }

    #[test]
    fn labels_block_scopes_to_mode_with_modeless_fallback() {
        let blob = concat!(
            "    wk binding=\"r\" desc=\"rename\"\n", // modeless
            "    wk mode=\"pane\" binding=\"r\" desc=\"rename pane\"\n",
            "    wk mode=\"tab\" binding=\"r\" desc=\"rename tab\"\n",
            "    wk mode=\"bogus\" binding=\"x\" desc=\"dropped\"\n", // bad mode → skipped
        );
        let labels = parse_labels_block(blob);
        assert_eq!(
            labels.lookup(InputMode::Pane, "r").unwrap().desc,
            "rename pane"
        );
        assert_eq!(
            labels.lookup(InputMode::Tab, "r").unwrap().desc,
            "rename tab"
        );
        // A mode without a scoped entry falls back to the modeless label.
        assert_eq!(labels.lookup(InputMode::Move, "r").unwrap().desc, "rename");
        // An unrecognized `mode` drops the entry entirely.
        assert!(labels.lookup(InputMode::Normal, "x").is_none());
    }

    #[test]
    fn labels_block_scopes_to_multiple_modes_and_aliases() {
        let blob = concat!(
            // Whitespace-separated mode list.
            "    wk mode=\"scroll search\" binding=\"u\" desc=\"half page up\"\n",
            // Comma-separated list.
            "    wk mode=\"pane,tab\" binding=\"r\" desc=\"rename\"\n",
            // The `search` alias expands to both phases.
            "    wk mode=\"search\" binding=\"n\" desc=\"next match\"\n",
            // Resolves to nothing → dropped (not made modeless).
            "    wk mode=\"bogus\" binding=\"z\" desc=\"dropped\"\n",
        );
        let labels = parse_labels_block(blob);
        // The same spec lands in every listed mode...
        assert_eq!(
            labels.lookup(InputMode::Scroll, "u").unwrap().desc,
            "half page up"
        );
        assert_eq!(
            labels.lookup(InputMode::Search, "u").unwrap().desc,
            "half page up"
        );
        // ...but not in unlisted ones.
        assert!(labels.lookup(InputMode::Normal, "u").is_none());
        assert_eq!(labels.lookup(InputMode::Pane, "r").unwrap().desc, "rename");
        assert_eq!(labels.lookup(InputMode::Tab, "r").unwrap().desc, "rename");
        // `search` alias covers both phases.
        assert_eq!(
            labels.lookup(InputMode::Search, "n").unwrap().desc,
            "next match"
        );
        assert_eq!(
            labels.lookup(InputMode::EnterSearch, "n").unwrap().desc,
            "next match"
        );
        // A `mode` that resolves to nothing drops the entry.
        assert!(labels.lookup(InputMode::Normal, "z").is_none());
    }

    #[test]
    fn paging_keys_render_as_glyphs() {
        let mut m = BTreeMap::new();
        m.insert("next_page_key".into(), "Ctrl d".into());
        m.insert("prev_page_key".into(), "Ctrl u".into());
        m.insert("back_key".into(), "Backspace".into());
        let c = Config::from_map(&m);
        assert_eq!(c.next_page_key.as_deref(), Some("\u{F0634}D")); // 󰘴D
        assert_eq!(c.prev_page_key.as_deref(), Some("\u{F0634}U")); // 󰘴U
        assert_eq!(c.back_key.as_deref(), Some("\u{F006E}")); // 󰁮
    }

    #[test]
    fn labels_block_from_map_via_config() {
        let mut m = BTreeMap::new();
        m.insert(
            "labels".into(),
            "wk binding=\"Ctrl h\" desc=\"focus left\"\nwk binding=\"Alt y\" desc=\"copy pwd\""
                .into(),
        );
        let c = Config::from_map(&m);
        assert_eq!(
            c.labels.lookup(InputMode::Normal, "ctrl+h").unwrap().desc,
            "focus left"
        );
        assert_eq!(
            c.labels.lookup(InputMode::Normal, "alt+y").unwrap().desc,
            "copy pwd"
        );
    }

    #[test]
    fn binding_separator_defaults_and_overrides() {
        assert_eq!(Config::default().binding_separator, "\u{279C}"); // ➜
        let mut m = BTreeMap::new();
        m.insert("binding_separator".into(), "|".into());
        assert_eq!(Config::from_map(&m).binding_separator, "|");
    }

    #[test]
    fn label_icon_color_unparseable_is_dropped() {
        let labels =
            parse_labels_block("wk binding=\"a\" desc=\"x\" icon=\"I\" icon_color=\"nope\"");
        let a = labels.lookup(InputMode::Normal, "a").unwrap();
        assert_eq!(a.icon.as_deref(), Some("I"));
        // Unparseable color → None (icon keeps the label color).
        assert_eq!(a.icon_color, None);
    }

    #[test]
    fn modes_block_overrides_icon_color_and_label() {
        let mut m = BTreeMap::new();
        m.insert(
            "modes".into(),
            concat!(
                // Named, order-independent props; icon via a \u{…} escape.
                "    tmux color=\"#CC66FF\" icon=\"\\u{F0633}\" label=\"Command\"\n",
                // Only color + (multi-word) label; keep the builtin glyph.
                "    rename_pane color=\"#E5BF7B\" label=\"Rename Pane\"\n",
            )
            .into(),
        );
        let c = Config::from_map(&m);
        // tmux: icon, color, and label all overridden; the escape decoded.
        assert_eq!(c.symbol(InputMode::Tmux), "\u{F0633}");
        assert_eq!(c.symbol_color(InputMode::Tmux), "\u{1b}[38;2;204;102;255m");
        assert_eq!(
            c.mode_labels.get(&InputMode::Tmux).map(String::as_str),
            Some("Command")
        );
        // rename_pane: no icon prop keeps the builtin glyph; spaced label kept.
        assert_eq!(c.mode_symbols.get(&InputMode::RenamePane), None);
        assert_eq!(
            c.symbol(InputMode::RenamePane),
            mode_icon(InputMode::RenamePane)
        );
        assert_eq!(
            c.symbol_color(InputMode::RenamePane),
            "\u{1b}[38;2;229;191;123m"
        );
        assert_eq!(
            c.mode_labels
                .get(&InputMode::RenamePane)
                .map(String::as_str),
            Some("Rename Pane")
        );
        // Untouched mode falls back to the builtin glyph + palette color.
        assert_eq!(c.symbol(InputMode::Scroll), mode_icon(InputMode::Scroll));
        assert_eq!(
            c.symbol_color(InputMode::Scroll),
            parse_color(mode_color(InputMode::Scroll)).unwrap()
        );
    }

    #[test]
    fn modes_block_search_alias_covers_both_phases() {
        let mut m = BTreeMap::new();
        m.insert(
            "modes".into(),
            "    search color=\"#61AFEF\" label=\"Find\"\n".into(),
        );
        let c = Config::from_map(&m);
        for mode in [InputMode::Search, InputMode::EnterSearch] {
            assert_eq!(c.symbol_color(mode), "\u{1b}[38;2;97;175;239m");
            assert_eq!(c.mode_labels.get(&mode).map(String::as_str), Some("Find"));
        }
    }

    #[test]
    fn groups_block_parses_ids_and_chords() {
        let mut m = BTreeMap::new();
        m.insert(
            "groups".into(),
            concat!(
                "    focus  \"Ctrl up\", \"Ctrl down\", \"Ctrl left\", \"Ctrl right\"\n",
                "    resize \"Alt up\"  \"Alt down\"\n", // commas optional
            )
            .into(),
        );
        let c = Config::from_map(&m);
        assert_eq!(c.groups.len(), 2);
        // The `id` token is dropped; members are canonicalized; no `mode` =
        // modeless.
        assert_eq!(
            c.groups[0].members,
            vec!["ctrl+up", "ctrl+down", "ctrl+left", "ctrl+right"]
        );
        assert!(c.groups[0].modes.is_empty());
        assert_eq!(c.groups[1].members, vec!["alt+up", "alt+down"]);
        assert!(c.groups[1].modes.is_empty());
    }

    #[test]
    fn groups_block_scopes_to_modes_with_aliases() {
        let mut m = BTreeMap::new();
        m.insert(
            "groups".into(),
            concat!(
                "    wk mode=\"pane tab\" \"r\" \"n\"\n", // multi-mode
                "    wk mode=\"search\" \"a\"\n",         // alias → Search+EnterSearch
                "    wk \"x\" \"y\"\n",                   // modeless
                "    wk mode=\"bogus\" \"z\"\n",          // resolves to nothing → dropped
            )
            .into(),
        );
        let c = Config::from_map(&m);
        assert_eq!(c.groups.len(), 3);
        assert_eq!(c.groups[0].members, vec!["r", "n"]);
        assert_eq!(c.groups[0].modes, vec![InputMode::Pane, InputMode::Tab]);
        // `search` alias expands to both search phases.
        assert_eq!(c.groups[1].members, vec!["a"]);
        assert_eq!(
            c.groups[1].modes,
            vec![InputMode::Search, InputMode::EnterSearch]
        );
        // Modeless group: empty modes.
        assert_eq!(c.groups[2].members, vec!["x", "y"]);
        assert!(c.groups[2].modes.is_empty());
    }

    #[test]
    fn traditional_preset_parses() {
        let mut m = BTreeMap::new();
        m.insert("sort_by".into(), "column".into());
        m.insert("max_height".into(), "4".into());
        m.insert("width".into(), "fill".into());
        m.insert("anchor".into(), "bottom+center".into());
        m.insert("margin".into(), "0,2,1,2".into());
        m.insert("padding".into(), "1,3,1,3".into());
        let c = Config::from_map(&m);
        assert_eq!(c.sort_by, SortBy::Column);
        assert_eq!(c.max_height, 4);
        assert_eq!(c.width, WidthMode::Fill);
        assert_eq!(
            c.anchor,
            Anchor {
                v: VAlign::Bottom,
                h: HAlign::Center
            }
        );
        assert_eq!(
            c.margin,
            Padding {
                top: 0,
                right: 2,
                bottom: 1,
                left: 2
            }
        );
        assert_eq!(
            c.padding,
            Padding {
                top: 1,
                right: 3,
                bottom: 1,
                left: 3
            }
        );
    }

    #[test]
    fn padding_defaults_to_horizontal_inset() {
        let c = Config::default();
        assert_eq!(
            c.padding,
            Padding {
                top: 0,
                right: 1,
                bottom: 0,
                left: 1
            }
        );
        assert_eq!(c.margin, Padding::default());
    }
}
