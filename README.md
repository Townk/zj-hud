# zj-hud

[![CI](https://github.com/Townk/zj-hud/actions/workflows/ci.yml/badge.svg)](https://github.com/Townk/zj-hud/actions/workflows/ci.yml)
[![Latest build](https://img.shields.io/github/v/release/Townk/zj-hud?include_prereleases&label=latest)](https://github.com/Townk/zj-hud/releases)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

A heads-up-display plugin for [Zellij](https://zellij.dev): a polished status
bar, a [which-key](https://github.com/folke/which-key.nvim)-style keybinding
panel, and floating **visual search** and **rename** dialogs — all from a
single WASM binary.

It's a from-scratch port of a WezTerm status-bar setup to Zellij's WASI plugin
architecture, with the rendering (powerline segments, Oklab color gradients,
tab layout) reimplemented with no external color/UI crates so it compiles
cleanly to `wasm32-wasip1`.

## Features

- **Status bar** with a tab strip (per-tab process/directory icons, active-tab
  emphasis, overflow scrolling with chevrons) and a right-hand segment chain:
  input-mode hint → session → battery + Wi-Fi → date/time.
- **Adaptive segments** — the mode segment shows only outside `Normal`, the
  session segment hides when it matches your default session name, and
  battery/Wi-Fi/clock collapse on narrow viewports.
- **Smooth color** — a 5-stop Oklab gradient from the mode color to the bar
  background, with WCAG-contrast-aware foregrounds.
- **Live system widgets** — battery and Wi-Fi are polled via Zellij's
  `run_command` API with per-widget TTL caching (no blocking, no flicker).
- **Which-key panel** — a floating, per-tab panel that shows the keybindings
  available in the current mode and follows you across tabs.
- **Visual search dialog** — a floating input that drives Zellij's native
  search while mirroring case/word/wrap toggles into the bar.
- **Rename dialog** — a floating input for renaming tabs/panes.

See [`docs/superpowers/specs/2026-05-26-zj-hud-design.md`](docs/superpowers/specs/2026-05-26-zj-hud-design.md)
and [`docs/specs/reverse-eng-spec.md`](docs/specs/reverse-eng-spec.md) for the
full behavioral design.

## Install

Every release ships a prebuilt `zj-hud.wasm` — you don't need a Rust toolchain
to use the plugin. Two stable download URLs are published:

| URL | Tracks |
|---|---|
| `https://github.com/Townk/zj-hud/releases/download/latest/zj-hud.wasm` | The **rolling build** — refreshed on every push to `master`. |
| `https://github.com/Townk/zj-hud/releases/download/v0.1.2/zj-hud.wasm` | A **pinned version** — immutable once published. |

Zellij can load a plugin straight from a URL and caches it locally, so you can
reference the release asset directly in your layout/config. For a stable local
path instead, download it once:

```sh
mkdir -p ~/.config/zellij/plugins
curl -fL -o ~/.config/zellij/plugins/zj-hud.wasm \
  https://github.com/Townk/zj-hud/releases/download/latest/zj-hud.wasm
```

`just install` does the build-and-copy in one step if you're building from
source.

> After changing the plugin URL, clear Zellij's plugin cache
> (`~/.cache/zellij`) or restart the session to force a re-download.

## Usage

zj-hud is loaded through a Zellij **layout**, like any status-bar plugin. The
single `.wasm` serves four roles, selected by the `role` configuration key
(default: the status bar). Because Zellij keys plugin instances by
`(url, configuration)`, each distinct `role` is its own instance, and they
coordinate through a session-scoped shared-state channel.

| `role` | What it is | How it's loaded |
|---|---|---|
| _(unset)_ | Status bar | A `size=1 borderless=true` pane in your layout. |
| `whichkey` | Which-key panel | A tiny floating pane in the layout's `default_tab_template` (spawned per tab). |
| `search` | Visual search dialog | A floating instance launched from a keybinding. |
| `rename` | Rename dialog | A floating instance launched from a keybinding. |

### Minimal status bar

Add the bar to the top of your layout (e.g.
`~/.config/zellij/layouts/default.kdl`). Configuration keys go as child nodes
of the `plugin` block:

```kdl
layout {
    pane size=1 borderless=true {
        plugin location="https://github.com/Townk/zj-hud/releases/download/latest/zj-hud.wasm" {
            // All keys are optional; defaults shown.
            default_session_name "main"   // hide the session segment for this name
            tab_max_width        "40"
            fullscreen_min_cols  "120"     // collapse system widgets below this width
        }
    }
    children
}
```

That alone gives you the tab strip and the right-hand segment chain. Unknown
keys are ignored and malformed values fall back to defaults, so it's safe to
start minimal.

### Companion roles (which-key, search, rename)

The which-key panel, search dialog, and rename dialog are additional instances
of the same binary distinguished by `role`. The which-key panel is declared as
a floating pane in your layout's `default_tab_template`, while the search and
rename dialogs are launched on demand from keybindings that target the plugin
with the matching `role` (e.g. a `Search`-mode binding pipes `role "search"`
with a `case`/`word`/`wrap` payload to drive the native search toggles).

Wiring all four roles together — the tab template, the mode keybindings, the
which-key labels, and the theming — is more involved than a single snippet can
fairly capture. The complete, maintained reference configuration is documented
in [`docs/specs/reverse-eng-spec.md`](docs/specs/reverse-eng-spec.md) and the
[design spec](docs/superpowers/specs/2026-05-26-zj-hud-design.md); start from
the status bar above and layer the companion roles in from there.

To have which-key start hidden for specific modes, add a mode list to the bar's
`which_key` block. Manual show/hide still works inside that mode until the next
mode transition:

```kdl
which_key {
    start_hidden "scroll"
}
```

> On first load Zellij prompts once to approve the plugin's permissions. All
> roles request the same (union) permission set so the grant is cached per URL
> and you're never re-prompted.

## Build from source

```sh
rustup target add wasm32-wasip1   # one-time
cargo build --release --target wasm32-wasip1
```

The artifact is `target/wasm32-wasip1/release/zj-hud.wasm`. Copy it to your
Zellij plugins directory, or run `just install`.

## Development

The rendering and parsing logic (color math, tab layout, truncation, config
parsing, which-key grid, search/rename state) is unit-tested on the host
toolchain — no WASM runtime required:

```sh
cargo test                              # ~280 unit tests
cargo clippy --all-targets -- -D warnings
cargo clippy --target wasm32-wasip1 -- -D warnings
cargo fmt --all -- --check
```

A [`justfile`](justfile) wraps the common tasks (`just build`, `just test`,
`just lint`, `just install`, `just reload`). CI runs the same lint/test/build
matrix on every push, and pushing a `vX.Y.Z` tag publishes a versioned
release.

## Limitations

- Battery/Wi-Fi queries are implemented for **macOS** and **Linux**; other
  platforms fall back to a static icon.
- The which-key panel is a per-tab floating pane (a single float can't span
  tabs in Zellij), so it's recreated on each tab via the tab template.
- Rendering targets a truecolor terminal; the powerline glyphs need a
  Nerd-Font-patched font.

## License

Licensed under the [MIT License](LICENSE).
