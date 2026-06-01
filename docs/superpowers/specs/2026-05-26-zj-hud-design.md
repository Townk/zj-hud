# zj-hud Design Spec

Port of the WezTerm `statusbar.wezterm` plugin to Zellij's WASI plugin
architecture. Behavioral source of truth: `docs/specs/reverse-eng-spec.md` and
`PROMPT.md`. Where this doc diverges from either, this doc wins.

---

## Architecture: Event-Driven State Machine

Each plugin instance holds an `AppState` struct updated on every Zellij event.
`render()` reads pre-computed state and assembles raw ANSI output. No trait
objects, no segment pipeline — fixed segment set, YAGNI.

### Data Model

```
State (ZellijPlugin impl)
├── config: Config          // parsed once in load() from KDL BTreeMap
└── app: AppState
    ├── mode: InputMode     // from ModeUpdate
    ├── tabs: Vec<TabInfo>  // from TabUpdate
    ├── panes: PaneManifest // from PaneUpdate
    ├── session_name: String // from SessionUpdate
    ├── battery: CachedValue<BatteryInfo>
    ├── wifi: CachedValue<bool>
    ├── cols: usize         // viewport width, set each render()
    ├── dirty: bool         // flipped by update(), cleared by render()
    └── got_permissions: bool
```

### Plugin Lifecycle

1. `load()` — parse config, subscribe to events, request permissions
2. `update(event)` — update relevant fields, set `dirty = true`, return dirty
3. `render(rows, cols)` — if dirty, recompute layout and emit ANSI; clear dirty

Events before permissions are granted get queued and replayed once granted.

---

## Tab Rendering Pipeline

Three stages:

### Stage 1: Title Composition (`tabs.rs`)

Priority chain per tab:
1. User-renamed tab -> tab icon + name
2. Foreground process is long-lived (not a shell) -> process icon + process name
3. Fallback -> directory icon + CWD basename (home icon if `$HOME`)

Shells (bash, zsh, fish, nu) are ephemeral — trigger CWD fallback. Everything
else is long-lived. Zoomed pane appends a zoom glyph suffix.

### Stage 2: Layout (`layout.rs`)

Given full tab list and available width:
1. Compute natural width per tab (title + padding + powerline edge)
2. Total fits -> done
3. Total exceeds budget -> equalize (floor all to min, redistribute surplus
   left-to-right per spec SS5.4.4)
4. Equalization pushes any tab below `MIN_TAB_WIDTH` (12) -> scrolling: visible
   window centered on active tab, chevron indicators on edges

Active tab always renders at `max_width` (default 40), never shrinks.

### Stage 3: Rendering (tab portion of `render.rs`)

Emit ANSI-styled cells: `[space][index][title][space][powerline edge]`. Active
tab gets distinct bg. Non-normal mode: hide inactive tabs, show only active with
mode-colored styling and rounded edge glyph (`ple_right_half_circle_thick`).

### Truncation (`truncation.rs`)

Split at `truncation_point` (default 0.4), insert ellipsis. 40% prefix, 60%
suffix.

---

## Right-Side Segments

Segment chain (right-to-left, loudest to quietest):

```
[ mode | session | battery wifi | date/time ]
  bg4     bg3        bg2           bg1
```

Each segment is a function returning `(styled_text, width)` or `("", 0)`.

### Elision

- Mode -> suppressed when `Normal`. All 13 non-normal modes produce a segment:
  `Locked`, `Resize`, `Pane`, `Tab`, `Scroll`, `EnterSearch`, `Search`,
  `RenameTab`, `RenamePane`, `Session`, `Move`, `Prompt`, `Tmux`
- Session -> suppressed when session name == configured default (`"main"`)
- Battery/WiFi/Time -> suppressed when `cols < fullscreen_min_cols` (default 120)
- Width-0 segments collapse their gradient slot and divider

### Color Gradient (`color.rs`)

5-stop Oklab interpolation from mode color to tab bar background. Each segment
gets its gradient stop as background.

Per-segment foreground: `bg.darken(0.8)`. If `contrast_ratio(bg, fg) < 3.8`,
switch to `bg.lighten(0.6)`.

Dividers: `ple_lower_right_triangle` with fg=right bg, bg=left bg.

---

## System Queries & Async Caching

Uses Zellij's `run_command()` API (WASI-safe). Results arrive as
`RunCommandResult` events with a context map identifying the query.

### Battery (TTL: 30s)

- macOS: `pmset -g batt`
- Linux: `/sys/class/power_supply/BAT*/capacity` and `status`
- No battery -> static host/desktop icon
- Icon thresholds: >=90 high, >=40 medium, >5 low, <=5 empty
- Fix WezTerm bug: use `charging` icon array when charging (not `discharging`)

### WiFi (TTL: 10s)

- macOS: `networksetup -getairportpower en0`
- Linux: `nmcli radio wifi`

### Timer-driven refresh

Subscribe to `Timer` events. Set initial timer in `load()`. On each tick, check
TTLs, fire `run_command()` if expired, set next timer.

### CachedValue<T>

```rust
struct CachedValue<T> {
    value: Option<T>,
    last_updated: Option<Instant>,
    ttl: Duration,
    in_flight: bool,
}
```

`in_flight` prevents duplicate concurrent requests.

---

## Color Math (`color.rs`)

All implemented from scratch — no external crate.

- **Representation:** `Color { r: u8, g: u8, b: u8 }`
- **Parsing:** `#RGB` and `#RRGGBB` hex strings
- **HSL ops:** `darken(factor)` reduces lightness, `lighten(factor)` increases it
- **Oklab ops:** RGB -> linear sRGB -> Oklab, linear interpolation across N
  stops, convert back. Used for the 5-stop gradient.
- **WCAG contrast:** `(L_lighter + 0.05) / (L_darker + 0.05)`, threshold 3.8
- **ANSI helpers:** `to_ansi_fg()`, `to_ansi_bg()` for truecolor sequences

---

## Configuration (`config.rs`)

Parsed from `BTreeMap<String, String>` in `load()`. All values are strings;
parse numbers/booleans from string representation.

```
Config {
    tab_max_width: usize,           // 40
    tab_truncation_point: f32,      // 0.4
    tab_hide_single: bool,          // false
    fullscreen_min_cols: usize,     // 120
    default_session_name: String,   // "main"
    mode_colors: HashMap<InputMode, Color>,
    icons: IconLibrary,
}
```

Unknown keys silently ignored. Malformed values fall back to defaults.

---

## Icons (`icons.rs`)

`pub const` Unicode codepoints. Process icon mapping via `match` on process
name. Shell detection via `is_shell()` function. Full table from spec SS5.6.

Icons are overridable via config — `IconLibrary` initialized from defaults, then
patched with user overrides.

---

## Final Bar Assembly (`render.rs`)

1. Store `cols`, check dirty flag
2. Compute right-side segments -> `(text, width)` pairs, total right width
3. Compute left-side tab layout -> available = `cols - right_width`
4. Concatenate: tabs + bg-colored gap fill + right segments
5. `print!()` single row + `\x1b[0m` reset
6. Clear dirty flag, cache output string

Cached output re-emitted when not dirty (handles redundant `render()` calls).

Empty right side (normal + "main" + narrow): tabs get full `cols`.

---

## File Dependencies

```
main.rs    -> config.rs, state.rs, render.rs
render.rs  -> tabs.rs, layout.rs, segments.rs, color.rs
tabs.rs    -> icons.rs, truncation.rs
segments.rs -> color.rs, icons.rs, system.rs
system.rs  -> state.rs
```

No circular dependencies.

---

## Events Subscribed

| Event | Purpose |
|---|---|
| ModeUpdate | Input mode changes, drives mode segment |
| TabUpdate | Full tab list, drives tab rendering |
| SessionUpdate | Session name, drives session segment |
| PaneUpdate | Pane focus/CWD/process, drives tab titles |
| Timer | Periodic refresh trigger for system queries |
| RunCommandResult | Async battery/WiFi results |
| PermissionRequestResult | Gate for ReadApplicationState |

---

## Key Decisions

- **Approach A (state machine)** over Approach B (trait objects): fixed segment
  set, YAGNI, dirty-flag maps to Zellij lifecycle
- **run_command()** for system IO: WASI-safe, proven pattern
- **Oklab** for gradient: visually smooth, ~50 lines to implement
- **Raw ANSI** for rendering: full control, no API dependency beyond stdout
- **No external color crate**: keep WASI compile simple
- **Independent build**: no reference to sibling projects
