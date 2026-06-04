# zj-hud task runner.
#
# Run `just --list` to see all recipes.

# URL the user's layout references for the plugin. Must match the
# `location=` value in ~/.config/zellij/layouts/default.kdl exactly,
# since Zellij identifies plugin instances by their URL.
wasm_path := justfile_directory() + "/target/wasm32-wasip1/release/zj-hud.wasm"
plugin_url := "file:" + wasm_path
remote_wasm_path := "Projects/apps/zellij/zj-hud/target/wasm32-wasip1/release/zj-hud.wasm"

# Default to the dev loop: build + reload in the running Zellij session.
default: reload

# Build the plugin in release mode for Zellij's wasm target.
build:
    cargo build --release --target wasm32-wasip1

# Build, then copy the release wasm to Remote Shell.
copy-remote: build
    #!/usr/bin/env bash
    set -euo pipefail
    ssh remote 'mkdir -p "$HOME/Projects/apps/zellij/zj-hud/target/wasm32-wasip1/release"'
    scp "{{ wasm_path }}" "remote:{{ remote_wasm_path }}"

# Build, then hot-reload the running plugin (and close the spurious side pane).
reload: build
    #!/usr/bin/env bash
    # `start-or-reload-plugin` matches running instances by (URL, configuration),
    # but the layout starts our plugin with a populated config block while this
    # CLI call passes none. Zellij therefore *also* spawns a fresh instance in
    # a new pane on top of reloading the layout-loaded one. We capture the new
    # plugin id printed on stdout and close that spurious pane.
    set -euo pipefail
    pid=$(zellij action start-or-reload-plugin {{ plugin_url }})
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
