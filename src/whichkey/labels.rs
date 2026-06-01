//! Turning the raw keymap into display entries.
//!
//! Two jobs, both pure/host-testable:
//!   * **same-action merge** — bindings whose action sequence is identical
//!     collapse into one entry that lists all their keys (so `←` and `h` both
//!     bound to "move focus left" become a single row);
//!   * **labels** — a curated dictionary for common actions, with a humanized
//!     `Debug` fallback for everything else. (User overrides land on top later.)

use std::collections::{BTreeMap, BTreeSet};

use zellij_tile::prelude::actions::Action;
use zellij_tile::prelude::{BareKey, Direction, InputMode, KeyModifier, KeyWithModifier, Resize};

/// A user-defined label: the description shown for a binding, an optional
/// leading icon glyph (rendered between the separator and the description), and
/// an optional SGR color for that icon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabelSpec {
    pub desc: String,
    pub icon: Option<String>,
    /// SGR foreground for the icon; `None` falls back to the label color.
    pub icon_color: Option<String>,
}

/// User-defined labels, looked up by `(mode, canonical chord)`.
///
/// A label may be scoped to a specific mode or left modeless (global). Lookup
/// ([`Labels::lookup`]) prefers a mode-scoped entry and falls back to the
/// modeless one, so the same chord can carry different descriptions in
/// different modes (e.g. `r` = "rename pane" in Pane, "rename tab" in Tab)
/// while modeless labels keep applying everywhere — preserving the original
/// flat-config behaviour for entries that omit `mode`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Labels {
    /// Modeless labels — apply in any mode unless a mode-scoped label for the
    /// same chord exists.
    global: BTreeMap<String, LabelSpec>,
    /// Mode-scoped labels: `mode → chord → spec`.
    by_mode: BTreeMap<InputMode, BTreeMap<String, LabelSpec>>,
}

impl Labels {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a label for canonical `chord`, scoped to `mode` (or modeless when
    /// `None`).
    pub fn insert(&mut self, mode: Option<InputMode>, chord: String, spec: LabelSpec) {
        match mode {
            Some(m) => {
                self.by_mode.entry(m).or_default().insert(chord, spec);
            }
            None => {
                self.global.insert(chord, spec);
            }
        }
    }

    /// The label for `chord` in `mode`: a mode-scoped entry wins over the
    /// modeless fallback.
    pub fn lookup(&self, mode: InputMode, chord: &str) -> Option<&LabelSpec> {
        self.by_mode
            .get(&mode)
            .and_then(|m| m.get(chord))
            .or_else(|| self.global.get(chord))
    }
}

/// Per-mode display-name overrides (e.g. `Tmux` → `Command`).
pub type ModeLabels = BTreeMap<InputMode, String>;

/// A set of bindings that should render contiguously, anchored at the group's
/// smallest member. Optionally scoped to one or more modes; an empty `modes`
/// is *modeless* (applies in every mode). For the mode being rendered,
/// mode-scoped groups claim their member chords ahead of modeless ones.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Group {
    /// Modes this group applies in; empty = modeless.
    pub modes: Vec<InputMode>,
    /// Canonical key chords that render together.
    pub members: Vec<String>,
}

use crate::whichkey::modes::mode_display_name;

/// A mode's display name: the configured override if present, else the builtin.
fn mode_label(mode: InputMode, overrides: &ModeLabels) -> String {
    overrides
        .get(&mode)
        .cloned()
        .unwrap_or_else(|| mode_display_name(mode).to_string())
}

/// One display row: the keys that trigger it and the human label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub keys: Vec<String>,
    pub label: String,
    /// Optional icon glyph, shown between the separator and the label. Only
    /// user-defined labels carry one; auto-derived entries leave it `None`.
    pub icon: Option<String>,
    /// Optional SGR foreground for the icon; `None` uses the label color.
    pub icon_color: Option<String>,
    /// Whether this binding's primary action enters another mode. Mode-switch
    /// entries are tinted differently (blue) by the renderer.
    pub switch: bool,
}

impl Entry {
    /// The keys joined for display, e.g. `"k,↑"`.
    pub fn keys_display(&self) -> String {
        self.keys.join(",")
    }
}

/// Collapse a mode's keymap into display entries, merging bindings that share
/// an identical action sequence and dropping no-ops. First-seen order is kept.
///
/// `labels` overrides the auto-derived label for any binding; it is looked up
/// by `(mode, chord)` so a label may be scoped to `mode` (with a modeless
/// fallback — see [`Labels::lookup`]). Two kinds of entries are filtered out:
///   * footer chrome — the close key (a pure `SwitchToMode(base_mode)`) and the
///     `wk_next_page` / `wk_prev_page` paging pipes;
///   * **unlabeled pipes** — the host delivers every `MessagePlugin` binding as
///     an indistinguishable `KeybindPipe` (name/payload/plugin stripped), so a
///     pipe carries no meaning unless the user names it. Labeled pipes show
///     (each on its own row, never merged); unlabeled ones are hidden.
pub fn merge_keybinds(
    keybinds: &[(KeyWithModifier, Vec<Action>)],
    mode: InputMode,
    base_mode: InputMode,
    labels: &Labels,
    mode_labels: &ModeLabels,
    groups: &[Group],
) -> Vec<Entry> {
    struct Acc {
        actions: Vec<Action>,
        keys: Vec<KeyWithModifier>,
        label: String,
        icon: Option<String>,
        icon_color: Option<String>,
        is_pipe: bool,
    }

    let mut accs: Vec<Acc> = Vec::new();

    for (key, actions) in keybinds {
        if is_noop(actions) || is_close(actions, base_mode) || is_paging(actions) {
            continue;
        }
        let user = labels.lookup(mode, &canonical_key(key)).cloned();

        if is_pipe(actions, base_mode) {
            // Indistinguishable to us — only show when explicitly named, and
            // never merge (no usable action signature to merge on).
            if let Some(spec) = user {
                accs.push(Acc {
                    actions: actions.clone(),
                    keys: vec![key.clone()],
                    label: spec.desc,
                    icon: spec.icon,
                    icon_color: spec.icon_color,
                    is_pipe: true,
                });
            }
            continue;
        }

        if let Some(acc) = accs
            .iter_mut()
            .find(|a| !a.is_pipe && &a.actions == actions)
        {
            if !acc.keys.contains(key) {
                acc.keys.push(key.clone());
            }
            // A user label on any key in the group wins over the auto label.
            if let Some(spec) = user {
                acc.label = spec.desc;
                acc.icon = spec.icon;
                acc.icon_color = spec.icon_color;
            }
        } else {
            let (label, icon, icon_color) = match user {
                Some(spec) => (spec.desc, spec.icon, spec.icon_color),
                None => (label_for(actions, mode_labels, base_mode), None, None),
            };
            accs.push(Acc {
                actions: actions.clone(),
                keys: vec![key.clone()],
                label,
                icon,
                icon_color,
                is_pipe: false,
            });
        }
    }

    // Resolve which group (if any) each binding joins, keyed by the canonical
    // chord of its representative (smallest) key. Earlier groups win on overlap.
    // Scope to the rendered mode: skip groups bound to other modes, and let
    // mode-scoped groups claim their chords ahead of modeless ones (a chord in
    // both joins the mode-scoped group). `gi` stays the original index so the
    // grouping/anchoring below is unaffected by the two-pass insertion order.
    let mut group_of: BTreeMap<String, usize> = BTreeMap::new();
    let mut claim = |scoped: bool| {
        for (gi, g) in groups.iter().enumerate() {
            let applies = if scoped {
                g.modes.contains(&mode)
            } else {
                g.modes.is_empty()
            };
            if applies {
                for chord in &g.members {
                    group_of.entry(chord.clone()).or_insert(gi);
                }
            }
        }
    };
    claim(true); // mode-scoped first
    claim(false); // then modeless

    // Materialize each accumulator into a sortable display row. `to_mode` is 0
    // for bindings that enter another mode (they float to the top) and 1
    // otherwise; `head` is the row's representative (smallest) key.
    struct Row {
        to_mode: u8,
        head: SortKey,
        group: Option<usize>,
        entry: Entry,
    }
    let rows: Vec<Row> = accs
        .into_iter()
        .map(|mut acc| {
            acc.keys.sort_by_key(sort_key);
            let head = acc.keys.first().map(sort_key).unwrap_or_default();
            let group = acc
                .keys
                .first()
                .and_then(|k| group_of.get(&canonical_key(k)).copied());
            let switch = is_to_mode(&acc.actions, base_mode);
            let to_mode = u8::from(!switch);
            let keys = acc.keys.iter().map(format_key).collect();
            Row {
                to_mode,
                head,
                group,
                entry: Entry {
                    keys,
                    label: acc.label,
                    icon: acc.icon,
                    icon_color: acc.icon_color,
                    switch,
                },
            }
        })
        .collect();

    // A display unit is either a lone row or a user-defined group rendered
    // contiguously. Each unit sorts by its anchor — the `(to_mode, head)` of
    // its smallest member; a group lays its members out in that same order.
    let mut grouped: BTreeMap<usize, Vec<Row>> = BTreeMap::new();
    let mut units: Vec<((u8, SortKey), Vec<Entry>)> = Vec::new();
    for row in rows {
        match row.group {
            Some(gi) => grouped.entry(gi).or_default().push(row),
            None => units.push(((row.to_mode, row.head), vec![row.entry])),
        }
    }
    for (_gi, mut members) in grouped {
        members.sort_by(|a, b| (a.to_mode, &a.head).cmp(&(b.to_mode, &b.head)));
        let anchor = (members[0].to_mode, members[0].head.clone());
        let entries = members.into_iter().map(|m| m.entry).collect();
        units.push((anchor, entries));
    }
    units.sort_by(|a, b| a.0.cmp(&b.0));
    units.into_iter().flat_map(|(_, entries)| entries).collect()
}

/// Whether a binding's primary purpose is to *enter another mode* — a bare mode
/// switch, as opposed to a "reset to base then act" prefix (those are labeled by
/// their substantive action; see [`primary_action`]). Such bindings are biased
/// to the top of the panel.
fn is_to_mode(actions: &[Action], base_mode: InputMode) -> bool {
    matches!(
        primary_action(actions, base_mode),
        Some(Action::SwitchToMode { .. } | Action::SwitchModeForAllClients { .. })
    )
}

/// Ordering key for a chord, implementing the standard panel sort:
///   * unmodified chords first, lexicographically by key name;
///   * then modified chords by (modifier count, modifier order, key name),
///     where modifier order is shift < cmd(super) < alt < ctrl.
///
/// Chords are normalized first: an uppercase character implies Shift + its
/// lowercase form (`Y` → `Shift y`, `Ctrl Y` → `Ctrl Shift y`), so shifted
/// letters sort with the modified group rather than the bare letters. This
/// affects ordering only — the displayed key is unchanged.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Default)]
struct SortKey {
    has_mods: u8,
    mod_count: usize,
    mod_ranks: Vec<u8>,
    key_class: u8,
    key_rank: u16,
    key: String,
}

fn sort_key(key: &KeyWithModifier) -> SortKey {
    let (bare, mods) = normalized(key);
    let mut mod_ranks: Vec<u8> = mods.iter().map(|m| mod_rank(*m)).collect();
    mod_ranks.sort_unstable();
    let (key_class, key_rank, key) = bare_order(&bare);
    SortKey {
        has_mods: u8::from(!mod_ranks.is_empty()),
        mod_count: mod_ranks.len(),
        mod_ranks,
        key_class,
        key_rank,
        key,
    }
}

/// Ordering of a bare key for the panel sort, as `(class, rank, name)`:
///   * **class 0** — named "symbol" keys (arrows, home/end, paging, tab, space,
///     enter, …), sorted by a curated reading order (`rank`);
///   * **class 1** — character keys (letters, digits, punctuation), sorted
///     lexicographically *after* every symbol key.
///
/// So within any modifier tier, `↑` sorts before `0`, and arrows keep their
/// natural up/down/left/right reading order rather than alphabetical.
fn bare_order(bare: &BareKey) -> (u8, u16, String) {
    // Curated reading order for the symbol class. F-keys and any uncurated
    // named keys trail the list (see below) but stay ahead of character keys.
    const CURATED: &[&str] = &[
        "up",
        "down",
        "left",
        "right", // arrows, reading order
        "home",
        "end",
        "pageup",
        "pagedown", // navigation block
        "tab",
        "space",
        "enter",
        "backspace",
        "delete",
        "esc",
    ];
    let name = canonical_bare(bare);
    match bare {
        // Space is a `Char(' ')` but reads as the word "space" — keep it a symbol.
        BareKey::Char(c) if *c != ' ' => (1, 0, c.to_ascii_lowercase().to_string()),
        BareKey::F(n) => (0, 100 + (*n as u16), name),
        _ => {
            let rank = CURATED
                .iter()
                .position(|k| *k == name)
                .map(|i| i as u16)
                .unwrap_or(500);
            (0, rank, name)
        }
    }
}

/// Canonicalize a chord the way Zellij's own `KeyWithModifier` does: an
/// uppercase ASCII character implies Shift + its lowercase form, so `Y` ≡
/// `Shift y` and `Ctrl Y` ≡ `Ctrl Shift y`. Used for display, sorting, and
/// label matching alike so all three agree.
fn normalized(key: &KeyWithModifier) -> (BareKey, BTreeSet<KeyModifier>) {
    let mut mods = key.key_modifiers.clone();
    let bare = match key.bare_key {
        BareKey::Char(c) if c.is_ascii_uppercase() => {
            mods.insert(KeyModifier::Shift);
            BareKey::Char(c.to_ascii_lowercase())
        }
        other => other,
    };
    (bare, mods)
}

fn mod_rank(modifier: KeyModifier) -> u8 {
    match modifier {
        KeyModifier::Shift => 0,
        KeyModifier::Super => 1,
        KeyModifier::Alt => 2,
        KeyModifier::Ctrl => 3,
    }
}

/// A stable, lowercase chord string for matching user labels, e.g.
/// `Ctrl+Shift+h` → `ctrl+shift+h`, `Alt+y` → `alt+y`, `Up` → `up`. Modifiers
/// are emitted in a fixed order (ctrl, alt, shift, super) so the result is
/// independent of how the chord was written.
pub fn canonical_key(key: &KeyWithModifier) -> String {
    let (bare, mods) = normalized(key);
    let mut out = String::new();
    if mods.contains(&KeyModifier::Ctrl) {
        out.push_str("ctrl+");
    }
    if mods.contains(&KeyModifier::Alt) {
        out.push_str("alt+");
    }
    if mods.contains(&KeyModifier::Shift) {
        out.push_str("shift+");
    }
    if mods.contains(&KeyModifier::Super) {
        out.push_str("super+");
    }
    out.push_str(&canonical_bare(&bare));
    out
}

fn canonical_bare(bare: &BareKey) -> String {
    match bare {
        BareKey::Char(' ') => "space".into(),
        BareKey::Char(c) => c.to_ascii_lowercase().to_string(),
        BareKey::Enter => "enter".into(),
        BareKey::Esc => "esc".into(),
        BareKey::Tab => "tab".into(),
        BareKey::Backspace => "backspace".into(),
        BareKey::Delete => "delete".into(),
        BareKey::Insert => "insert".into(),
        BareKey::Up => "up".into(),
        BareKey::Down => "down".into(),
        BareKey::Left => "left".into(),
        BareKey::Right => "right".into(),
        BareKey::Home => "home".into(),
        BareKey::End => "end".into(),
        BareKey::PageUp => "pageup".into(),
        BareKey::PageDown => "pagedown".into(),
        BareKey::F(n) => format!("f{n}"),
        other => format!("{other:?}").to_ascii_lowercase(),
    }
}

/// Parse a user-written chord (e.g. `Ctrl h`, `ctrl+h`, `Alt y`, `Shift Enter`)
/// into the same canonical form as [`canonical_key`], or `None` if no base key
/// was given. Modifier tokens accept common aliases; everything else is the
/// base key.
pub fn parse_chord(spec: &str) -> Option<String> {
    let (mut ctrl, mut alt, mut shift, mut sup) = (false, false, false, false);
    let mut bare: Option<String> = None;

    for tok in spec.split(|c: char| c == '+' || c.is_whitespace()) {
        let raw = tok.trim();
        if raw.is_empty() {
            continue;
        }
        let t = raw.to_ascii_lowercase();
        match t.as_str() {
            "ctrl" | "control" => ctrl = true,
            "alt" | "opt" | "option" | "meta" => alt = true,
            "shift" => shift = true,
            "super" | "cmd" | "command" | "win" => sup = true,
            _ => {
                // A lone uppercase letter implies Shift (`Y` ≡ `Shift y`),
                // matching the normalization applied to the live keymap.
                if raw.chars().count() == 1 && raw.starts_with(|c: char| c.is_ascii_uppercase()) {
                    shift = true;
                }
                bare = Some(normalize_bare_name(&t));
            }
        }
    }

    let bare = bare?;
    let mut out = String::new();
    if ctrl {
        out.push_str("ctrl+");
    }
    if alt {
        out.push_str("alt+");
    }
    if shift {
        out.push_str("shift+");
    }
    if sup {
        out.push_str("super+");
    }
    out.push_str(&bare);
    Some(out)
}

fn normalize_bare_name(t: &str) -> String {
    match t {
        "escape" => "esc",
        "return" => "enter",
        "del" => "delete",
        "ins" => "insert",
        "pgup" => "pageup",
        "pgdn" | "pgdown" => "pagedown",
        "spc" => "space",
        other => other,
    }
    .to_string()
}

/// Parse a user-written chord into a `KeyWithModifier` so it can be rendered
/// with [`format_key`] (used for the footer's paging-key hints, which can't be
/// auto-discovered — the host strips pipe names from the keymap). Returns
/// `None` if no base key is recognized.
pub fn parse_chord_to_key(spec: &str) -> Option<KeyWithModifier> {
    let mut mods = BTreeSet::new();
    let mut bare: Option<BareKey> = None;

    for tok in spec.split(|c: char| c == '+' || c.is_whitespace()) {
        let raw = tok.trim();
        if raw.is_empty() {
            continue;
        }
        let t = raw.to_ascii_lowercase();
        match t.as_str() {
            "ctrl" | "control" => {
                mods.insert(KeyModifier::Ctrl);
            }
            "alt" | "opt" | "option" | "meta" => {
                mods.insert(KeyModifier::Alt);
            }
            "shift" => {
                mods.insert(KeyModifier::Shift);
            }
            "super" | "cmd" | "command" | "win" => {
                mods.insert(KeyModifier::Super);
            }
            _ => {
                if raw.chars().count() == 1 && raw.starts_with(|c: char| c.is_ascii_uppercase()) {
                    mods.insert(KeyModifier::Shift);
                }
                bare = bare_from_name(&normalize_bare_name(&t));
            }
        }
    }

    Some(KeyWithModifier {
        bare_key: bare?,
        key_modifiers: mods,
    })
}

/// Inverse of [`canonical_bare`] for the common keys: a canonical key name to
/// its `BareKey`.
fn bare_from_name(name: &str) -> Option<BareKey> {
    Some(match name {
        "enter" => BareKey::Enter,
        "esc" => BareKey::Esc,
        "tab" => BareKey::Tab,
        "space" => BareKey::Char(' '),
        "backspace" => BareKey::Backspace,
        "delete" => BareKey::Delete,
        "insert" => BareKey::Insert,
        "up" => BareKey::Up,
        "down" => BareKey::Down,
        "left" => BareKey::Left,
        "right" => BareKey::Right,
        "home" => BareKey::Home,
        "end" => BareKey::End,
        "pageup" => BareKey::PageUp,
        "pagedown" => BareKey::PageDown,
        f if f.starts_with('f') && f.len() > 1 && f[1..].chars().all(|c| c.is_ascii_digit()) => {
            BareKey::F(f[1..].parse().ok()?)
        }
        s if s.chars().count() == 1 => BareKey::Char(s.chars().next().unwrap()),
        _ => return None,
    })
}

fn is_noop(actions: &[Action]) -> bool {
    actions.is_empty() || actions.iter().all(|a| matches!(a, Action::NoOp))
}

/// A pure "return to base mode" binding — the close key, shown in the footer.
fn is_close(actions: &[Action], base_mode: InputMode) -> bool {
    actions.len() == 1
        && matches!(
            &actions[0],
            Action::SwitchToMode { input_mode } | Action::SwitchModeForAllClients { input_mode }
                if *input_mode == base_mode
        )
}

/// Whether a binding's primary action is a plugin/CLI pipe.
fn is_pipe(actions: &[Action], base_mode: InputMode) -> bool {
    matches!(
        primary_action(actions, base_mode),
        Some(Action::KeybindPipe { .. } | Action::CliPipe { .. })
    )
}

/// A binding that pages this panel — shown in the footer, not the body.
fn is_paging(actions: &[Action]) -> bool {
    actions.iter().any(|a| {
        matches!(
            a,
            Action::KeybindPipe { name: Some(n), .. } | Action::CliPipe { name: Some(n), .. }
                if n == "wk_next_page" || n == "wk_prev_page"
        )
    })
}

/// Render a key chord with NerdFont glyphs for modifiers and special keys,
/// e.g. `Ctrl+Shift+h` → `󰘴 󰘶 H`, `Up` → `↑`, `Shift+Tab` → `󰌥`. The key is
/// uppercased and every glyph (each modifier and the key) is space-separated.
pub fn format_key(key: &KeyWithModifier) -> String {
    render_chord(key, " ")
}

/// Like [`format_key`] but with no separators (`󰘴󰘶H`), for compact contexts
/// such as the footer where chords sit inline with labels.
pub fn format_key_compact(key: &KeyWithModifier) -> String {
    render_chord(key, "")
}

fn render_chord(key: &KeyWithModifier, sep: &str) -> String {
    let (bare, mods) = normalized(key);
    // Shift+Tab has its own dedicated combined glyph.
    if bare == BareKey::Tab && mods.len() == 1 && mods.contains(&KeyModifier::Shift) {
        return "\u{F0325}".to_string();
    }
    let mut parts: Vec<String> = mods
        .iter()
        .map(|m| modifier_glyph(*m).to_string())
        .collect();
    parts.push(bare_key_glyph(&bare));
    parts.join(sep)
}

fn modifier_glyph(modifier: KeyModifier) -> &'static str {
    match modifier {
        KeyModifier::Ctrl => "\u{F0634}",  // 󰘴
        KeyModifier::Alt => "\u{F0635}",   // 󰘵
        KeyModifier::Shift => "\u{F0636}", // 󰘶
        KeyModifier::Super => "\u{F0633}", // 󰘳 (Cmd)
    }
}

fn bare_key_glyph(bare: &BareKey) -> String {
    match bare {
        BareKey::Enter => "\u{F0311}".into(),     // 󰌑
        BareKey::Esc => "\u{F12B7}".into(),       // 󱊷
        BareKey::Tab => "\u{F0312}".into(),       // 󰌒
        BareKey::Backspace => "\u{F006E}".into(), // 󰁮
        BareKey::Delete => "\u{F0E7E}".into(),    // 󰹾
        BareKey::Up => "↑".into(),
        BareKey::Down => "↓".into(),
        BareKey::Left => "←".into(),
        BareKey::Right => "→".into(),
        BareKey::Char(' ') => "\u{F1050}".into(), // 󱁐
        BareKey::Char(c) => c.to_ascii_uppercase().to_string(),
        BareKey::F(n) if (1..=12).contains(n) => {
            // F1..F12 are consecutive: F1 = U+F12AB.
            char::from_u32(0xF12AB + (*n as u32 - 1))
                .map(|c| c.to_string())
                .unwrap_or_else(|| format!("F{n}"))
        }
        other => other.to_string(),
    }
}

/// The label for an action sequence: curated for the *primary* action,
/// otherwise a humanized fallback.
pub fn label_for(actions: &[Action], mode_labels: &ModeLabels, base_mode: InputMode) -> String {
    let Some(action) = primary_action(actions, base_mode) else {
        return String::new();
    };
    curated(action, mode_labels).unwrap_or_else(|| humanize(action))
}

/// The action that gives a binding its meaning. A leading switch *to the base
/// mode* is a "reset to base then do X" prefix (e.g. `SwitchToMode normal;
/// GoToTab 1`), so we skip those and label by the first substantive action.
/// A switch to a *non-base* mode is itself the point of the binding (e.g.
/// `SwitchToMode entersearch; SearchInput 0` = "enter Search"), so it is *not*
/// skipped. Falls back to the first action when there's nothing else.
fn primary_action(actions: &[Action], base_mode: InputMode) -> Option<&Action> {
    actions
        .iter()
        .find(|a| !is_switch_to(a, base_mode))
        .or_else(|| actions.first())
}

/// Whether `action` is a mode switch whose target is `mode`.
fn is_switch_to(action: &Action, mode: InputMode) -> bool {
    matches!(
        action,
        Action::SwitchToMode { input_mode } | Action::SwitchModeForAllClients { input_mode }
            if *input_mode == mode
    )
}

/// Curated, hand-tuned labels for the common, high-traffic actions.
fn curated(action: &Action, mode_labels: &ModeLabels) -> Option<String> {
    Some(match action {
        Action::SwitchToMode { input_mode } | Action::SwitchModeForAllClients { input_mode } => {
            format!("{} \u{2026}", mode_label(*input_mode, mode_labels))
        }
        Action::MoveFocus { direction } => format!("focus {}", dir(*direction)),
        Action::MoveFocusOrTab { direction } => format!("focus/tab {}", dir(*direction)),
        Action::MovePane {
            direction: Some(d), ..
        } => format!("move pane {}", dir(*d)),
        Action::MovePane { direction: None } => "move pane".into(),
        Action::Resize { resize, direction } => match direction {
            Some(d) => format!("{} {}", resize_word(*resize), dir(*d)),
            None => resize_word(*resize).into(),
        },
        Action::NewPane { .. } => "new pane".into(),
        Action::NewTab { .. } => "new tab".into(),
        Action::CloseFocus => "close pane".into(),
        Action::CloseTab => "close tab".into(),
        Action::GoToTab { index } => format!("go to tab {index}"),
        Action::GoToTabName { name, .. } => format!("go to tab {name}"),
        Action::MoveTab { direction } => format!("move tab {}", dir(*direction)),
        Action::GoToNextTab => "next tab".into(),
        Action::GoToPreviousTab => "prev tab".into(),
        Action::KeybindPipe {
            name,
            payload,
            plugin,
            ..
        }
        | Action::CliPipe {
            name,
            payload,
            plugin,
            ..
        } => pipe_label(name.as_deref(), payload.as_deref(), plugin.as_deref()),
        Action::ToggleFocusFullscreen => "fullscreen".into(),
        Action::ToggleFloatingPanes => "toggle floating".into(),
        Action::TogglePaneEmbedOrFloating => "float/embed".into(),
        Action::TogglePaneFrames => "toggle frames".into(),
        Action::Detach => "detach".into(),
        Action::Quit => "quit".into(),
        _ => return None,
    })
}

fn dir(d: Direction) -> &'static str {
    match d {
        Direction::Left => "left",
        Direction::Right => "right",
        Direction::Up => "up",
        Direction::Down => "down",
    }
}

fn resize_word(r: Resize) -> &'static str {
    match r {
        Resize::Increase => "grow",
        Resize::Decrease => "shrink",
    }
}

/// Label for a `MessagePlugin`/CLI pipe from its `name` (and `payload` when the
/// payload is a single simple token, e.g. vim-navigator's `move_focus`+`left` →
/// `move focus left`). Complex payloads (the context-keys `default: …` routing
/// strings) are ignored. These are the best automatic guess; user-supplied
/// labels will override them.
fn pipe_label(name: Option<&str>, payload: Option<&str>, plugin: Option<&str>) -> String {
    // Prefer the explicit message name, then the plugin alias, then a generic
    // marker. (The host currently strips all of these, so most pipes fall
    // through to "pipe" — user-defined labels are the real fix.)
    let base = name
        .or(plugin)
        .map(humanize_token)
        .unwrap_or_else(|| "pipe".to_string());
    match payload {
        Some(p) if is_simple_token(p) => format!("{base} {}", humanize_token(p)),
        _ => base,
    }
}

fn is_simple_token(s: &str) -> bool {
    !s.is_empty()
        && s.chars().count() <= 16
        && s.chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
}

/// Lowercase a config token and turn `_`/`-` separators into spaces.
fn humanize_token(s: &str) -> String {
    s.to_lowercase().replace(['_', '-'], " ")
}

/// Fallback: take the variant name from the `Debug` form and turn `CamelCase`
/// into lowercase words, e.g. `TogglePaneFrames` → `toggle pane frames`.
fn humanize(action: &Action) -> String {
    let debug = format!("{action:?}");
    let name: String = debug.chars().take_while(|c| c.is_alphanumeric()).collect();
    split_camel(&name)
}

fn split_camel(name: &str) -> String {
    let mut out = String::new();
    for (i, ch) in name.chars().enumerate() {
        if ch.is_uppercase() && i != 0 {
            out.push(' ');
        }
        out.extend(ch.to_lowercase());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use zellij_tile::prelude::BareKey;

    fn key(c: char) -> KeyWithModifier {
        KeyWithModifier::new(BareKey::Char(c))
    }

    fn no_labels() -> Labels {
        Labels::new()
    }

    fn spec(desc: &str, icon: Option<&str>) -> LabelSpec {
        LabelSpec {
            desc: desc.into(),
            icon: icon.map(Into::into),
            icon_color: None,
        }
    }

    fn no_mode_labels() -> ModeLabels {
        ModeLabels::new()
    }

    fn pipe(name: &str, payload: Option<&str>) -> Action {
        Action::KeybindPipe {
            name: Some(name.into()),
            payload: payload.map(Into::into),
            args: None,
            plugin: None,
            plugin_id: None,
            configuration: None,
            launch_new: false,
            skip_cache: false,
            floating: None,
            in_place: None,
            cwd: None,
            pane_title: None,
        }
    }

    #[test]
    fn identical_actions_merge_and_keep_all_keys() {
        let kb = vec![
            (
                key('h'),
                vec![Action::MoveFocus {
                    direction: Direction::Left,
                }],
            ),
            (
                key('a'),
                vec![Action::MoveFocus {
                    direction: Direction::Left,
                }],
            ),
            (
                key('j'),
                vec![Action::MoveFocus {
                    direction: Direction::Down,
                }],
            ),
        ];
        let entries = merge_keybinds(
            &kb,
            InputMode::Normal,
            InputMode::Normal,
            &no_labels(),
            &no_mode_labels(),
            &[],
        );
        assert_eq!(entries.len(), 2);
        // Keys within an entry are sorted (a < h) and rendered uppercase.
        assert_eq!(entries[0].keys, vec!["A", "H"]);
        assert_eq!(entries[0].label, "focus left");
        assert_eq!(entries[1].label, "focus down");
    }

    #[test]
    fn symbol_keys_sort_before_characters_in_curated_order() {
        let named = |b: BareKey| KeyWithModifier::new(b);
        // Distinct actions so nothing merges; none enter a mode.
        let kb = vec![
            (key('a'), vec![Action::GoToTab { index: 1 }]),
            (key('0'), vec![Action::GoToTab { index: 2 }]),
            (named(BareKey::Right), vec![Action::GoToTab { index: 3 }]),
            (named(BareKey::Up), vec![Action::GoToTab { index: 4 }]),
            (named(BareKey::Left), vec![Action::GoToTab { index: 5 }]),
            (named(BareKey::Down), vec![Action::GoToTab { index: 6 }]),
        ];
        let order: Vec<String> = merge_keybinds(
            &kb,
            InputMode::Normal,
            InputMode::Normal,
            &no_labels(),
            &no_mode_labels(),
            &[],
        )
        .iter()
        .map(|e| e.keys[0].clone())
        .collect();
        // Arrows (reading order) precede character keys; among chars, `0` < `a`.
        assert_eq!(order, vec!["↑", "↓", "←", "→", "0", "A"]);
    }

    #[test]
    fn mode_switches_float_to_top() {
        // `a` sorts first lexically but is a normal action; `p`/`z` enter modes
        // and must lead, ordered among themselves by the standard sort (p < z).
        let kb = vec![
            (key('a'), vec![Action::GoToTab { index: 1 }]),
            (
                key('z'),
                vec![Action::SwitchToMode {
                    input_mode: InputMode::Pane,
                }],
            ),
            (
                key('p'),
                vec![Action::SwitchToMode {
                    input_mode: InputMode::Tab,
                }],
            ),
        ];
        let order: Vec<String> = merge_keybinds(
            &kb,
            InputMode::Normal,
            InputMode::Normal,
            &no_labels(),
            &no_mode_labels(),
            &[],
        )
        .iter()
        .map(|e| e.keys[0].clone())
        .collect();
        assert_eq!(order, vec!["P", "Z", "A"]);
    }

    #[test]
    fn groups_render_contiguously_anchored_at_smallest() {
        // Natural order would be a, b, c; grouping {a, c} pulls c up next to a.
        let kb = vec![
            (key('a'), vec![Action::GoToTab { index: 1 }]),
            (key('b'), vec![Action::GoToTab { index: 2 }]),
            (key('c'), vec![Action::GoToTab { index: 3 }]),
        ];
        let groups = vec![Group {
            modes: vec![],
            members: vec!["a".to_string(), "c".to_string()],
        }];
        let order: Vec<String> = merge_keybinds(
            &kb,
            InputMode::Normal,
            InputMode::Normal,
            &no_labels(),
            &no_mode_labels(),
            &groups,
        )
        .iter()
        .map(|e| e.keys[0].clone())
        .collect();
        assert_eq!(order, vec!["A", "C", "B"]);
    }

    #[test]
    fn mode_scoped_group_applies_only_in_its_mode() {
        // Natural order is a, b, c. A group {a, c} scoped to Pane pulls c up next
        // to a — but only when rendering Pane; in any other mode it's inert.
        let kb = vec![
            (key('a'), vec![Action::GoToTab { index: 1 }]),
            (key('b'), vec![Action::GoToTab { index: 2 }]),
            (key('c'), vec![Action::GoToTab { index: 3 }]),
        ];
        let groups = vec![Group {
            modes: vec![InputMode::Pane],
            members: vec!["a".to_string(), "c".to_string()],
        }];
        let in_pane: Vec<String> = merge_keybinds(
            &kb,
            InputMode::Pane,
            InputMode::Normal,
            &no_labels(),
            &no_mode_labels(),
            &groups,
        )
        .iter()
        .map(|e| e.keys[0].clone())
        .collect();
        assert_eq!(in_pane, vec!["A", "C", "B"]);

        let in_tab: Vec<String> = merge_keybinds(
            &kb,
            InputMode::Tab,
            InputMode::Normal,
            &no_labels(),
            &no_mode_labels(),
            &groups,
        )
        .iter()
        .map(|e| e.keys[0].clone())
        .collect();
        assert_eq!(in_tab, vec!["A", "B", "C"]);
    }

    #[test]
    fn entries_follow_standard_chord_order() {
        // Build in deliberately scrambled order; expect normalized sort.
        let ctrl = |c: char| KeyWithModifier::new(BareKey::Char(c)).with_ctrl_modifier();
        let alt = |c: char| KeyWithModifier::new(BareKey::Char(c)).with_alt_modifier();
        // Distinct actions so nothing merges; the labels are irrelevant here.
        let kb = vec![
            (ctrl('a'), vec![Action::GoToTab { index: 1 }]),
            (alt('a'), vec![Action::GoToTab { index: 2 }]),
            (key('b'), vec![Action::GoToTab { index: 3 }]),
            (
                KeyWithModifier::new(BareKey::Char('Y')),
                vec![Action::GoToTab { index: 4 }],
            ), // Y → Shift y
            (key('a'), vec![Action::GoToTab { index: 5 }]),
        ];
        let order: Vec<String> = merge_keybinds(
            &kb,
            InputMode::Normal,
            InputMode::Normal,
            &no_labels(),
            &no_mode_labels(),
            &[],
        )
        .iter()
        .map(|e| e.keys[0].clone())
        .collect();
        // No-mods first (A, B), then 1-mod by shift<alt<ctrl: Shift Y, Alt A,
        // Ctrl A. Letters uppercase; a space follows the modifier glyphs.
        let shift = "\u{F0636}";
        let alt = "\u{F0635}";
        let ctrl = "\u{F0634}";
        assert_eq!(
            order,
            vec![
                "A".to_string(),
                "B".to_string(),
                format!("{shift} Y"),
                format!("{alt} A"),
                format!("{ctrl} A"),
            ]
        );
    }

    #[test]
    fn noops_are_dropped() {
        let kb = vec![(key('x'), vec![Action::NoOp])];
        assert!(merge_keybinds(
            &kb,
            InputMode::Normal,
            InputMode::Normal,
            &no_labels(),
            &no_mode_labels(),
            &[]
        )
        .is_empty());
    }

    #[test]
    fn switch_to_base_then_goto_tab_labels_by_tab() {
        // `bind "1" { SwitchToMode "normal"; GoToTab 1; }` etc.
        let kb = vec![
            (
                key('1'),
                vec![
                    Action::SwitchToMode {
                        input_mode: InputMode::Normal,
                    },
                    Action::GoToTab { index: 1 },
                ],
            ),
            (
                key('2'),
                vec![
                    Action::SwitchToMode {
                        input_mode: InputMode::Normal,
                    },
                    Action::GoToTab { index: 2 },
                ],
            ),
        ];
        let entries = merge_keybinds(
            &kb,
            InputMode::Normal,
            InputMode::Normal,
            &no_labels(),
            &no_mode_labels(),
            &[],
        );
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].label, "go to tab 1");
        assert_eq!(entries[1].label, "go to tab 2");
    }

    #[test]
    fn switch_to_nonbase_mode_then_act_labels_as_mode_switch() {
        // `bind "s" { SwitchToMode "entersearch"; SearchInput 0; }` — the switch
        // is to a *non-base* mode, so it's the point of the binding (not a
        // reset-to-base prefix). It must read as the mode switch and float up,
        // not be labeled by the trailing `SearchInput`.
        let kb = vec![(
            key('s'),
            vec![
                Action::SwitchToMode {
                    input_mode: InputMode::EnterSearch,
                },
                Action::SearchInput { input: vec![0] },
            ],
        )];
        let entries = merge_keybinds(
            &kb,
            InputMode::Normal,
            InputMode::Normal,
            &no_labels(),
            &no_mode_labels(),
            &[],
        );
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].label, "Search \u{2026}"); // EnterSearch displays as "Search"
        assert!(entries[0].switch);
    }

    #[test]
    fn close_and_paging_bindings_are_filtered() {
        let kb = vec![
            (
                KeyWithModifier::new(BareKey::Esc),
                vec![Action::SwitchToMode {
                    input_mode: InputMode::Normal,
                }],
            ),
            (
                KeyWithModifier::new(BareKey::Char('d')).with_ctrl_modifier(),
                vec![pipe("wk_next_page", None)],
            ),
            (
                key('p'),
                vec![Action::SwitchToMode {
                    input_mode: InputMode::Pane,
                }],
            ),
        ];
        let entries = merge_keybinds(
            &kb,
            InputMode::Normal,
            InputMode::Normal,
            &no_labels(),
            &no_mode_labels(),
            &[],
        );
        // Close (Esc→Normal) and wk_next_page are dropped; entering Pane stays.
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].label, "Pane \u{2026}");
        assert!(entries[0].switch);
    }

    #[test]
    fn pipe_label_uses_name_and_simple_payload() {
        assert_eq!(
            label_for(
                &[pipe("move_focus", Some("left"))],
                &no_mode_labels(),
                InputMode::Normal
            ),
            "move focus left"
        );
        // Complex payloads are ignored (just the name).
        assert_eq!(
            label_for(
                &[pipe("ctrl+l", Some("default: pipe x"))],
                &no_mode_labels(),
                InputMode::Normal
            ),
            "ctrl+l"
        );
    }

    #[test]
    fn unlabeled_pipes_are_hidden() {
        let kb = vec![
            (key('a'), vec![pipe("x", None)]),
            (key('b'), vec![pipe("x", None)]),
        ];
        assert!(merge_keybinds(
            &kb,
            InputMode::Normal,
            InputMode::Normal,
            &no_labels(),
            &no_mode_labels(),
            &[]
        )
        .is_empty());
    }

    #[test]
    fn labeled_pipes_show_and_never_merge() {
        // Identical (stripped) pipe actions: labels are keyed by chord, so each
        // labeled key gets its own row even though the actions are equal.
        let kb = vec![
            (key('a'), vec![pipe("x", None)]),
            (key('b'), vec![pipe("x", None)]),
            (key('c'), vec![pipe("x", None)]), // unlabeled → hidden
        ];
        let mut labels = Labels::new();
        labels.insert(None, "a".into(), spec("focus left", None));
        labels.insert(None, "b".into(), spec("focus right", None));
        let entries = merge_keybinds(
            &kb,
            InputMode::Normal,
            InputMode::Normal,
            &labels,
            &no_mode_labels(),
            &[],
        );
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].keys, vec!["A"]);
        assert_eq!(entries[0].label, "focus left");
        assert_eq!(entries[1].label, "focus right");
    }

    #[test]
    fn user_label_icon_flows_to_entry() {
        let kb = vec![(
            KeyWithModifier::new(BareKey::Char('h')).with_ctrl_modifier(),
            vec![Action::MoveFocus {
                direction: Direction::Left,
            }],
        )];
        let mut labels = Labels::new();
        labels.insert(
            None,
            "ctrl+h".into(),
            LabelSpec {
                desc: "focus left".into(),
                icon: Some("\u{F0312}".into()),
                icon_color: Some("\u{1b}[38;5;4m".into()),
            },
        );
        let entries = merge_keybinds(
            &kb,
            InputMode::Normal,
            InputMode::Normal,
            &labels,
            &no_mode_labels(),
            &[],
        );
        assert_eq!(entries[0].label, "focus left");
        assert_eq!(entries[0].icon.as_deref(), Some("\u{F0312}"));
        assert_eq!(entries[0].icon_color.as_deref(), Some("\u{1b}[38;5;4m"));
    }

    #[test]
    fn user_label_overrides_curated_for_non_pipe() {
        let kb = vec![(
            KeyWithModifier::new(BareKey::Char('p')),
            vec![Action::SwitchToMode {
                input_mode: InputMode::Pane,
            }],
        )];
        let mut labels = Labels::new();
        labels.insert(None, "p".into(), spec("panes", None));
        let entries = merge_keybinds(
            &kb,
            InputMode::Normal,
            InputMode::Normal,
            &labels,
            &no_mode_labels(),
            &[],
        );
        assert_eq!(entries[0].label, "panes");
    }

    #[test]
    fn mode_scoped_label_beats_modeless_and_is_per_mode() {
        // The same chord `r` is bound across modes; the renderer asks per mode.
        // A modeless label is the fallback; mode-scoped labels override it only
        // in their own mode.
        let kb = vec![(key('r'), vec![Action::GoToTab { index: 1 }])];
        let mut labels = Labels::new();
        labels.insert(None, "r".into(), spec("rename", None)); // modeless fallback
        labels.insert(Some(InputMode::Pane), "r".into(), spec("rename pane", None));
        labels.insert(Some(InputMode::Tab), "r".into(), spec("rename tab", None));

        let pane = merge_keybinds(
            &kb,
            InputMode::Pane,
            InputMode::Normal,
            &labels,
            &no_mode_labels(),
            &[],
        );
        assert_eq!(pane[0].label, "rename pane");

        let tab = merge_keybinds(
            &kb,
            InputMode::Tab,
            InputMode::Normal,
            &labels,
            &no_mode_labels(),
            &[],
        );
        assert_eq!(tab[0].label, "rename tab");

        // A mode with no scoped entry falls back to the modeless label.
        let mv = merge_keybinds(
            &kb,
            InputMode::Move,
            InputMode::Normal,
            &labels,
            &no_mode_labels(),
            &[],
        );
        assert_eq!(mv[0].label, "rename");
    }

    #[test]
    fn mode_label_override_renames_switch_target() {
        let mut mode_labels = ModeLabels::new();
        mode_labels.insert(InputMode::Tmux, "Command".into());
        assert_eq!(
            label_for(
                &[Action::SwitchToMode {
                    input_mode: InputMode::Tmux
                }],
                &mode_labels,
                InputMode::Normal
            ),
            "Command \u{2026}"
        );
        // Unmapped modes keep their builtin name.
        assert_eq!(
            label_for(
                &[Action::SwitchToMode {
                    input_mode: InputMode::Pane
                }],
                &mode_labels,
                InputMode::Normal
            ),
            "Pane \u{2026}"
        );
    }

    #[test]
    fn canonical_and_parse_chord_round_trip() {
        let ctrl_shift_h = KeyWithModifier::new(BareKey::Char('h'))
            .with_ctrl_modifier()
            .with_shift_modifier();
        assert_eq!(canonical_key(&ctrl_shift_h), "ctrl+shift+h");
        // Spelled various ways, all normalize to the same chord.
        assert_eq!(parse_chord("Ctrl Shift h").as_deref(), Some("ctrl+shift+h"));
        assert_eq!(parse_chord("shift+ctrl+H").as_deref(), Some("ctrl+shift+h"));
        assert_eq!(parse_chord("Alt y").as_deref(), Some("alt+y"));
        assert_eq!(parse_chord("escape").as_deref(), Some("esc"));
        assert_eq!(parse_chord("ctrl").as_deref(), None); // no base key
    }

    #[test]
    fn curated_switch_mode_reads_as_mode_ellipsis() {
        assert_eq!(
            label_for(
                &[Action::SwitchToMode {
                    input_mode: InputMode::Pane
                }],
                &no_mode_labels(),
                InputMode::Normal
            ),
            "Pane \u{2026}"
        );
    }

    #[test]
    fn humanized_fallback_splits_camelcase() {
        assert_eq!(
            label_for(
                &[Action::TogglePaneFrames],
                &no_mode_labels(),
                InputMode::Normal
            ),
            "toggle frames"
        );
        // FocusNextPane is not curated → humanized.
        assert_eq!(
            label_for(
                &[Action::FocusNextPane],
                &no_mode_labels(),
                InputMode::Normal
            ),
            "focus next pane"
        );
    }

    #[test]
    fn key_glyphs() {
        // Letters uppercase; modified chords get a space between mods and key.
        assert_eq!(format_key(&key('k')), "K");
        assert_eq!(format_key(&KeyWithModifier::new(BareKey::Up)), "↑");
        assert_eq!(
            format_key(&KeyWithModifier::new(BareKey::Char('d')).with_ctrl_modifier()),
            "\u{F0634} D"
        );
        assert_eq!(format_key(&KeyWithModifier::new(BareKey::Esc)), "\u{F12B7}");
        assert_eq!(
            format_key(&KeyWithModifier::new(BareKey::F(5))),
            "\u{F12AF}"
        );
    }

    #[test]
    fn resize_with_direction() {
        assert_eq!(
            label_for(
                &[Action::Resize {
                    resize: Resize::Increase,
                    direction: Some(Direction::Up),
                }],
                &no_mode_labels(),
                InputMode::Normal
            ),
            "grow up"
        );
    }
}
