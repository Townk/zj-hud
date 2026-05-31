# zj-statusbar task runner.
#
# Run `just --list` to see all recipes.

# URL the user's layout references for the plugin. Must match the
# `location=` value in ~/.config/zellij/layouts/default.kdl exactly,
# since Zellij identifies plugin instances by their URL. The path is
# already a symlink to the cargo build output, so `build` updates it
# in place — no copy step is needed.
plugin_url := "file:" + env_var('HOME') + "/.config/zellij/plugins/zj-statusbar.wasm"

# Default to the dev loop: build + reload in the running Zellij session.
default: reload

# Build the plugin in release mode for Zellij's wasm target.
build:
    cargo build --release --target wasm32-wasip1

# Build, then hot-reload the running plugin (and close the spurious side pane).
reload: build
    #!/usr/bin/env bash
    # `start-or-reload-plugin` matches running instances by (URL, configuration),
    # but the layout starts our plugin with a populated config block while this
    # CLI call passes none. Zellij therefore *also* spawns a fresh instance in
    # a new pane on top of reloading the layout-loaded one. We capture the new
    # plugin id printed on stdout and close that spurious pane.
    set -euo pipefail
    pid=$(zellij action start-or-reload-plugin {{plugin_url}})
    if [[ "$pid" =~ ^plugin_[0-9]+$ ]]; then
        zellij action close-pane --pane-id "$pid" 2>/dev/null || true
    fi

# Run the full test suite.
test:
    cargo test

# Lint with clippy on both the wasm build and the test build.
lint:
    cargo clippy --target wasm32-wasip1 -- -D warnings
    cargo clippy --tests -- -D warnings

# Format check (CI-friendly).
fmt-check:
    cargo fmt --all -- --check

# Apply rustfmt.
fmt:
    cargo fmt --all

# Remove build artifacts.
clean:
    cargo clean
