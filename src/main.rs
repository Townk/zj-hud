mod bar;
mod search;
mod shared;
mod whichkey;

use std::collections::BTreeMap;
use zellij_tile::prelude::*;

use bar::State;
use search::SearchPane;
use whichkey::WhichKeyPane;

register_plugin!(Plugin);

/// Host-import stub for non-wasm builds. `zellij-tile` declares
/// `host_run_plugin_command` as a wasm import resolved by the zellij runtime;
/// on the host target (i.e. `cargo test`) that symbol is undefined and linking
/// fails. The unit tests only exercise pure logic and never issue host
/// commands, so a no-op lets the test binary link. It is compiled out of the
/// real wasm plugin, which keeps the genuine import.
#[cfg(not(target_arch = "wasm32"))]
#[no_mangle]
pub extern "C" fn host_run_plugin_command() {}

/// The plugin binary serves three roles, selected at load time by the `role`
/// configuration key:
///
/// - the status bar (default),
/// - a floating "visual search" input pane (`role = "search"`), and
/// - a per-tab which-key style keybinding panel (`role = "whichkey"`).
///
/// All are the same `.wasm`; Zellij keys plugin instances by (url,
/// configuration), so the differing config makes each role a distinct instance.
/// They coordinate through a single session-scoped `SharedState` file plus the
/// `shared_state::SYNC_PIPE` broadcast.
enum Plugin {
    Bar(Box<State>),
    Search(Box<SearchPane>),
    WhichKey(Box<WhichKeyPane>),
}

impl Default for Plugin {
    fn default() -> Self {
        Plugin::Bar(Box::default())
    }
}

impl ZellijPlugin for Plugin {
    fn load(&mut self, configuration: BTreeMap<String, String>) {
        match configuration.get("role").map(String::as_str) {
            Some("search") => *self = Plugin::Search(Box::default()),
            Some("whichkey") => *self = Plugin::WhichKey(Box::default()),
            _ => {}
        }
        match self {
            Plugin::Bar(state) => state.load(configuration),
            Plugin::Search(search) => search.load(configuration),
            Plugin::WhichKey(which_key) => which_key.load(configuration),
        }
    }

    fn update(&mut self, event: Event) -> bool {
        match self {
            Plugin::Bar(state) => state.update(event),
            Plugin::Search(search) => search.update(event),
            Plugin::WhichKey(which_key) => which_key.update(event),
        }
    }

    fn pipe(&mut self, pipe_message: PipeMessage) -> bool {
        match self {
            Plugin::Bar(state) => state.pipe(pipe_message),
            Plugin::Search(search) => search.pipe(pipe_message),
            Plugin::WhichKey(which_key) => which_key.pipe(pipe_message),
        }
    }

    fn render(&mut self, rows: usize, cols: usize) {
        match self {
            Plugin::Bar(state) => state.render(rows, cols),
            Plugin::Search(search) => search.render(rows, cols),
            Plugin::WhichKey(which_key) => which_key.render(rows, cols),
        }
    }
}

/// Permissions requested by all plugin roles (bar, search, and which-key).
///
/// The roles share a single wasm URL, and Zellij caches granted
/// permissions *per URL*, overwriting the cache with the exact set last
/// requested. If the roles asked for different sets they'd ping-pong that
/// cache and re-prompt on every session. Requesting one identical (union) set
/// keeps the cached grant stable: the user approves once and never again.
///
/// `ReadApplicationState`/`ReadPaneContents` are the bar's; `RunActionsAsUser`
/// (drive native search) and `InterceptInput` (grab keystrokes) are the search
/// pane's; `RunCommands` and `ChangeApplicationState` are used by multiple roles.
/// `MessageAndLaunchOtherPlugins` is required by `pipe_message_to_plugin` —
/// used for the cross-instance `SharedState` broadcast (the bar's mode sync and
/// the search pane's `search_active` flag both ride this single channel).
/// Without it those messages are silently dropped by the host.
pub const PLUGIN_PERMISSIONS: &[PermissionType] = &[
    PermissionType::ReadApplicationState,
    PermissionType::ChangeApplicationState,
    PermissionType::RunCommands,
    PermissionType::ReadPaneContents,
    PermissionType::RunActionsAsUser,
    PermissionType::InterceptInput,
    PermissionType::MessageAndLaunchOtherPlugins,
];
