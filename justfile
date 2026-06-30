# zj-hud task runner.
#
# Run `just --list` to see all recipes.

# URL the user's layout references for the plugin. Must match the
# `location=` value in ~/.config/zellij/layouts/default.kdl exactly,
# since Zellij identifies plugin instances by their URL.
wasm_path := justfile_directory() + "/target/wasm32-wasip1/release/zj-hud.wasm"
plugin_url := "file:" + wasm_path
plugin_dir := env_var('HOME') + "/.config/zellij/plugins"
repo := "Townk/zj-hud"

# Default to the dev loop: build + reload in the running Zellij session.
default: reload

# Ensure the wasm target Zellij loads is installed in the active toolchain.
# `rust-toolchain.toml` pins `wasm32-wasip1`, but rustup doesn't always
# auto-install targets from that file, so a plain `just build` can fail with
# `can't find crate for core` until the target is added once.
target:
    rustup target list --installed | grep -q '^wasm32-wasip1$' || rustup target add wasm32-wasip1

# Build the plugin in release mode for Zellij's wasm target.
build: target
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
    pid=$(zellij action start-or-reload-plugin {{ plugin_url }})
    if [[ "$pid" =~ ^plugin_[0-9]+$ ]]; then
        zellij action close-pane --pane-id "$pid" 2>/dev/null || true
    fi

# Build, then install the wasm into your local Zellij plugins directory.
install: build
    mkdir -p "{{ plugin_dir }}"
    cp "{{ wasm_path }}" "{{ plugin_dir }}/"

# Run the full test suite.
test:
    cargo test

# Lint with clippy on both the wasm build and the test build.
lint: target
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

# Bump version, run the quality gate, commit, tag & push: `just release 0.2.0`.
release version:
    #!/usr/bin/env bash
    set -euo pipefail
    ver="{{ version }}"
    tag="v$ver"

    if [[ ! "$ver" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
        echo "error: version must be X.Y.Z (got '$ver')" >&2
        exit 1
    fi

    branch="$(git rev-parse --abbrev-ref HEAD)"
    if [[ "$branch" != "master" ]]; then
        echo "error: releases are cut from 'master' (currently on '$branch')" >&2
        exit 1
    fi

    if [[ -n "$(git status --porcelain)" ]]; then
        echo "error: working tree is dirty; commit or stash changes first" >&2
        exit 1
    fi

    # --force: CI moves a rolling "latest" prerelease tag on every push to
    # master, so a plain --tags fetch fails ("would clobber existing tag
    # latest") and, under set -e, kills this recipe before the version bump.
    git fetch --quiet --force origin master --tags
    if git rev-parse -q --verify "refs/tags/$tag" >/dev/null; then
        echo "error: tag $tag already exists" >&2
        exit 1
    fi
    if [[ "$(git rev-parse HEAD)" != "$(git rev-parse origin/master)" ]]; then
        echo "error: local master is not in sync with origin/master; pull/push first" >&2
        exit 1
    fi

    # Ensure the wasm target is installed before the quality gate needs it.
    rustup target list --installed | grep -q '^wasm32-wasip1$' || rustup target add wasm32-wasip1

    # Quality gate before mutating anything.
    cargo fmt --all -- --check
    cargo clippy --all-targets -- -D warnings
    cargo test

    # Bump the version in Cargo.toml and the pinned URL example in the README.
    sed -i.bak -E "s/^version = \".*\"/version = \"$ver\"/" Cargo.toml
    sed -i.bak -E "s#releases/download/v[0-9]+\.[0-9]+\.[0-9]+/#releases/download/$tag/#g" README.md
    rm -f Cargo.toml.bak README.md.bak

    # Refresh Cargo.lock and confirm the release artifact still builds.
    cargo build --release --target wasm32-wasip1

    git add Cargo.toml Cargo.lock README.md
    git commit -m "chore(release): $tag"
    git tag -a "$tag" -m "$tag"
    git push origin master "$tag"

    echo "Released $tag -> https://github.com/{{ repo }}/releases/tag/$tag"
