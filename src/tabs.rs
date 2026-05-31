use std::path::Path;
use unicode_width::UnicodeWidthStr;

use crate::config::Config;
use crate::icons;
use crate::state::AppState;
use crate::truncation::truncated_text;

pub struct TabTitle {
    pub index_str: String,
    pub body: String,
    pub path: Option<PathTitle>,
    pub extra_icons: String,
}

pub struct PathTitle {
    pub icon: String,
    pub display_path: String,
    pub project_root_display_path: Option<String>,
}

struct TabBody {
    text: String,
    path: Option<PathTitle>,
}

pub fn compose_tab_title(
    tab_position: usize,
    tab_name: &str,
    state: &AppState,
    config: &Config,
) -> TabTitle {
    let index_str = format!("{} ", tab_position + 1);

    let body = compose_body(tab_position, tab_name, state, config);

    let extra_icons = if state.any_pane_zoomed(tab_position) {
        format!(" {}", config.icons.zoom_icon)
    } else {
        String::new()
    };

    TabTitle {
        index_str,
        body: body.text,
        path: body.path,
        extra_icons,
    }
}

pub fn render_tab_title(title: &TabTitle, max_width: usize, truncation_point: f32) -> String {
    let index_width = UnicodeWidthStr::width(title.index_str.as_str());
    let extra_icons_width = UnicodeWidthStr::width(title.extra_icons.as_str());

    let overhead = index_width + extra_icons_width;
    let max_body = max_width.saturating_sub(overhead);

    let body = if let Some(path) = &title.path {
        render_path_body(path, max_body, truncation_point as f64)
    } else {
        truncated_text(&title.body, max_body, truncation_point as f64)
    };

    format!("{}{}{}", title.index_str, body, title.extra_icons)
}

fn compose_body(tab_position: usize, tab_name: &str, state: &AppState, config: &Config) -> TabBody {
    // Priority 1: User-renamed tab
    if !tab_name.is_empty() && !is_default_tab_name(tab_name) {
        return plain_body(format!("{} {}", config.icons.tab_icon, tab_name));
    }

    // Find the focused terminal pane for this tab
    let pane = state.focused_pane_for_tab(tab_position);

    // Priority 2: Long-lived process from pane cache. Track the pane the cmd
    // came from so we can prefer its OSC-set window title over the bare process
    // name when one is available.
    let cmd_pane = pane
        .and_then(|p| state.pane_cache.cmd.get(&p.id).map(|cmd| (p, cmd)))
        .or_else(|| {
            state
                .panes_for_tab(tab_position)
                .iter()
                .filter(|p| !p.is_plugin)
                .find_map(|p| state.pane_cache.cmd.get(&p.id).map(|cmd| (p, cmd)))
        });

    if let Some((pane_with_cmd, cmd)) = cmd_pane {
        if let Some(argv0) = cmd.first() {
            let proc = basename(argv0);
            if !proc.is_empty() && !icons::is_shell(&proc) {
                let icon = icons::process_icon(&proc)
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| config.icons.tab_process.clone());
                let label =
                    pane_osc_title(&pane_with_cmd.title, &proc).unwrap_or(proc);
                return plain_body(format!("{} {}", icon, label));
            }
        }
    }

    // Priority 3: CWD from pane cache (focused pane first, then any sibling)
    let cwd = pane
        .and_then(|p| state.pane_cache.cwd.get(&p.id))
        .or_else(|| {
            state
                .panes_for_tab(tab_position)
                .iter()
                .filter(|p| !p.is_plugin)
                .find_map(|p| state.pane_cache.cwd.get(&p.id))
        });

    if let Some(cwd) = cwd {
        if cwd == &state.home {
            return path_body(config.icons.tab_home.clone(), "~".to_string(), None);
        }
        let display = pretty_cwd(cwd, &state.home);
        let project_root_display_path = state
            .project_roots
            .roots
            .get(cwd)
            .and_then(|root| root.as_ref())
            .map(|root| pretty_cwd(root, &state.home));
        return path_body(
            config.icons.tab_dir.clone(),
            display,
            project_root_display_path,
        );
    }

    // Priority 4: Try extracting CWD from pane title (fallback)
    if let Some(pane) = pane {
        let cwd_str = extract_cwd_from_title(&pane.title);
        if !cwd_str.is_empty() {
            let base = basename(&cwd_str);
            if base == "~" || cwd_str == "~" {
                return path_body(config.icons.tab_home.clone(), "~".to_string(), None);
            }
            return path_body(config.icons.tab_dir.clone(), base, None);
        }
    }

    // Priority 5: Last resort
    plain_body(format!(
        "{} Tab #{}",
        config.icons.tab_dir,
        tab_position + 1
    ))
}

fn plain_body(text: String) -> TabBody {
    TabBody { text, path: None }
}

fn path_body(
    icon: String,
    display_path: String,
    project_root_display_path: Option<String>,
) -> TabBody {
    let text = format!("{} {}", icon, display_path);
    TabBody {
        text,
        path: Some(PathTitle {
            icon,
            display_path,
            project_root_display_path,
        }),
    }
}

fn render_path_body(path: &PathTitle, max_body: usize, truncation_point: f64) -> String {
    let icon_width = UnicodeWidthStr::width(path.icon.as_str());
    let overhead = icon_width + 1;
    if overhead >= max_body {
        return truncated_text(
            &format!("{} {}", path.icon, path.display_path),
            max_body,
            truncation_point,
        );
    }

    let max_path = max_body - overhead;
    let rendered_path = path
        .project_root_display_path
        .as_deref()
        .and_then(|project_root| {
            abbreviated_project_path(&path.display_path, project_root, max_path, truncation_point)
        })
        .unwrap_or_else(|| truncated_text(&path.display_path, max_path, truncation_point));

    format!("{} {}", path.icon, rendered_path)
}

#[derive(Clone, Debug)]
struct PathSegment {
    text: String,
    abbreviated: bool,
}

impl PathSegment {
    fn render(&self) -> String {
        if self.abbreviated {
            abbreviate_segment(&self.text)
        } else {
            self.text.clone()
        }
    }
}

fn abbreviated_project_path(
    display_path: &str,
    project_root_display_path: &str,
    max_width: usize,
    truncation_point: f64,
) -> Option<String> {
    if UnicodeWidthStr::width(display_path) <= max_width {
        return Some(display_path.to_string());
    }

    let display_segments = split_path_segments(display_path);
    let root_segments = split_path_segments(project_root_display_path);
    let root_start = find_subsequence(&display_segments, &root_segments)?;
    let project_idx = root_start + root_segments.len().saturating_sub(1);
    let most_idx = display_segments.len().checked_sub(1)?;

    if project_idx > most_idx {
        return None;
    }

    let mut segments = display_segments
        .into_iter()
        .map(|text| PathSegment {
            text,
            abbreviated: false,
        })
        .collect::<Vec<_>>();

    for idx in (0..project_idx).chain((project_idx + 1)..most_idx) {
        if can_abbreviate_segment(&segments[idx].text) {
            segments[idx].abbreviated = true;
            let rendered = join_segments(&segments);
            if UnicodeWidthStr::width(rendered.as_str()) <= max_width {
                return Some(rendered);
            }
        }
    }

    if segments[..project_idx]
        .iter()
        .any(|segment| segment.abbreviated)
    {
        let mut collapsed = Vec::with_capacity(segments.len() - project_idx + 1);
        collapsed.push(PathSegment {
            text: "…".to_string(),
            abbreviated: false,
        });
        collapsed.extend_from_slice(&segments[project_idx..]);
        segments = collapsed;

        let rendered = join_segments(&segments);
        if UnicodeWidthStr::width(rendered.as_str()) <= max_width {
            return Some(rendered);
        }
    }

    let project_idx = segments
        .iter()
        .position(|segment| segment.text == root_segments[root_segments.len() - 1])
        .unwrap_or(0);
    if can_abbreviate_segment(&segments[project_idx].text) {
        segments[project_idx].abbreviated = true;
        let rendered = join_segments(&segments);
        if UnicodeWidthStr::width(rendered.as_str()) <= max_width {
            return Some(rendered);
        }
    }

    Some(truncated_text(display_path, max_width, truncation_point))
}

fn split_path_segments(path: &str) -> Vec<String> {
    path.split('/')
        .filter(|segment| !segment.is_empty())
        .map(str::to_string)
        .collect()
}

fn find_subsequence(haystack: &[String], needle: &[String]) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn join_segments(segments: &[PathSegment]) -> String {
    segments
        .iter()
        .map(PathSegment::render)
        .collect::<Vec<_>>()
        .join("/")
}

fn can_abbreviate_segment(segment: &str) -> bool {
    let width = UnicodeWidthStr::width(segment);
    if segment.starts_with('.') {
        width > 3
    } else {
        width > 2
    }
}

fn abbreviate_segment(segment: &str) -> String {
    let keep = if segment.starts_with('.') { 2 } else { 1 };
    let prefix = segment.chars().take(keep).collect::<String>();
    format!("{}…", prefix)
}

fn pretty_cwd(cwd: &Path, home: &Path) -> String {
    if cwd == home {
        return "~".to_string();
    }
    if let Ok(stripped) = cwd.strip_prefix(home) {
        let sub = stripped.to_string_lossy();
        let sub = sub.trim_start_matches('/');
        return format!("~/{}", sub);
    }
    cwd.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| cwd.display().to_string())
}

fn is_default_tab_name(name: &str) -> bool {
    name.starts_with("Tab #") && name[5..].chars().all(|c| c.is_ascii_digit())
}

/// Returns an OSC-emitted window title worth honoring over the bare process
/// name, if one is present. Empty titles, Zellij's default `Pane #N` titles,
/// and titles that simply echo the process name are treated as uninformative.
fn pane_osc_title(pane_title: &str, proc: &str) -> Option<String> {
    let trimmed = pane_title.trim();
    if trimmed.is_empty() || trimmed == proc || is_default_pane_title(trimmed) {
        return None;
    }
    Some(trimmed.to_string())
}

fn is_default_pane_title(title: &str) -> bool {
    title
        .strip_prefix("Pane #")
        .map(|rest| !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()))
        .unwrap_or(false)
}

fn basename(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(path)
        .to_string()
}

fn extract_cwd_from_title(title: &str) -> String {
    let trimmed = title.trim();
    if trimmed.starts_with('/') || trimmed.starts_with('~') {
        return trimmed.to_string();
    }
    if let Some((_target, dir)) = trimmed.split_once(": ") {
        if dir.starts_with('/') || dir.starts_with('~') {
            return dir.to_string();
        }
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basename_unix_path() {
        assert_eq!(basename("/home/user/projects"), "projects");
    }

    #[test]
    fn basename_just_name() {
        assert_eq!(basename("nvim"), "nvim");
    }

    #[test]
    fn is_default_tab_name_true() {
        assert!(is_default_tab_name("Tab #1"));
        assert!(is_default_tab_name("Tab #12"));
    }

    #[test]
    fn is_default_tab_name_false() {
        assert!(!is_default_tab_name("my-project"));
        assert!(!is_default_tab_name(""));
    }

    #[test]
    fn pretty_cwd_home() {
        let home = std::path::PathBuf::from("/home/user");
        assert_eq!(pretty_cwd(std::path::Path::new("/home/user"), &home), "~");
    }

    #[test]
    fn pretty_cwd_subdir() {
        let home = std::path::PathBuf::from("/home/user");
        assert_eq!(
            pretty_cwd(std::path::Path::new("/home/user/projects"), &home),
            "~/projects"
        );
    }

    #[test]
    fn pretty_cwd_outside_home() {
        let home = std::path::PathBuf::from("/home/user");
        assert_eq!(
            pretty_cwd(std::path::Path::new("/tmp/build"), &home),
            "build"
        );
    }

    #[test]
    fn render_tab_title_basic() {
        let title = TabTitle {
            index_str: "1 ".to_string(),
            body: "\u{F0770} code".to_string(),
            path: None,
            extra_icons: String::new(),
        };
        let rendered = render_tab_title(&title, 40, 0.4);
        assert!(rendered.starts_with("1 "));
        assert!(rendered.contains("code"));
    }

    #[test]
    fn zoom_icon_is_separated_from_body_by_a_space() {
        let mut state = AppState::default();
        state.panes.insert(
            0,
            vec![zellij_tile::prelude::PaneInfo {
                id: 1,
                is_focused: true,
                title: "agent".to_string(),
                is_fullscreen: true,
                ..Default::default()
            }],
        );
        state.pane_cache.cmd.insert(1, vec!["agent".to_string()]);

        let config = Config::default();
        let title = compose_tab_title(0, "Tab #1", &state, &config);

        assert!(
            title.extra_icons.starts_with(' '),
            "extra_icons should begin with a space separator, got {:?}",
            title.extra_icons
        );
        assert!(title.extra_icons.contains(&config.icons.zoom_icon));

        let rendered = render_tab_title(&title, 40, 0.4);
        let expected_suffix = format!(" {}", config.icons.zoom_icon);
        assert!(
            rendered.ends_with(&expected_suffix),
            "rendered title should end with ' <zoom_icon>', got {:?}",
            rendered
        );
    }

    #[test]
    fn inactive_tab_uses_cached_process_from_any_terminal_pane() {
        let mut state = AppState::default();
        state.panes.insert(
            0,
            vec![zellij_tile::prelude::PaneInfo {
                id: 1,
                is_focused: false,
                title: "agent".to_string(),
                ..Default::default()
            }],
        );
        state.pane_cache.cmd.insert(1, vec!["agent".to_string()]);

        let config = Config::default();
        let title = compose_tab_title(0, "Tab #1", &state, &config);

        assert!(title.body.contains("agent"));
        assert!(title.path.is_none());
    }

    #[test]
    fn long_lived_process_honors_osc_window_title() {
        let mut state = AppState::default();
        state.panes.insert(
            0,
            vec![zellij_tile::prelude::PaneInfo {
                id: 1,
                is_focused: true,
                title: "Cargo.toml - NVIM".to_string(),
                ..Default::default()
            }],
        );
        state.pane_cache.cmd.insert(1, vec!["nvim".to_string()]);

        let config = Config::default();
        let title = compose_tab_title(0, "Tab #1", &state, &config);

        assert!(title.body.contains("Cargo.toml - NVIM"));
        // The bare process name should be replaced, not appended.
        assert!(!title.body.contains(" nvim"));
        assert!(title.path.is_none());
    }

    #[test]
    fn long_lived_process_ignores_default_pane_title() {
        let mut state = AppState::default();
        state.panes.insert(
            0,
            vec![zellij_tile::prelude::PaneInfo {
                id: 1,
                is_focused: true,
                title: "Pane #3".to_string(),
                ..Default::default()
            }],
        );
        state.pane_cache.cmd.insert(1, vec!["nvim".to_string()]);

        let config = Config::default();
        let title = compose_tab_title(0, "Tab #1", &state, &config);

        assert!(title.body.contains("nvim"));
        assert!(!title.body.contains("Pane #"));
    }

    #[test]
    fn long_lived_process_ignores_title_equal_to_process_name() {
        let mut state = AppState::default();
        state.panes.insert(
            0,
            vec![zellij_tile::prelude::PaneInfo {
                id: 1,
                is_focused: true,
                title: "htop".to_string(),
                ..Default::default()
            }],
        );
        state.pane_cache.cmd.insert(1, vec!["htop".to_string()]);

        let config = Config::default();
        let title = compose_tab_title(0, "Tab #1", &state, &config);

        // Body should just be "{icon} htop" — title and proc both contribute "htop".
        let occurrences = title.body.matches("htop").count();
        assert_eq!(occurrences, 1, "body was {:?}", title.body);
    }

    #[test]
    fn project_path_abbreviates_segments_in_order() {
        let path = "~/.local/share/chezmoi/dot_config/nvim/lua/lualine/components";
        let root = "~/.local/share/chezmoi";
        let expected_steps = [
            "~/.l…/share/chezmoi/dot_config/nvim/lua/lualine/components",
            "~/.l…/s…/chezmoi/dot_config/nvim/lua/lualine/components",
            "~/.l…/s…/chezmoi/d…/nvim/lua/lualine/components",
            "~/.l…/s…/chezmoi/d…/n…/lua/lualine/components",
            "~/.l…/s…/chezmoi/d…/n…/l…/lualine/components",
            "~/.l…/s…/chezmoi/d…/n…/l…/l…/components",
            "…/chezmoi/d…/n…/l…/l…/components",
            "…/c…/d…/n…/l…/l…/components",
        ];

        for expected in expected_steps {
            let max_width = UnicodeWidthStr::width(expected);
            assert_eq!(
                abbreviated_project_path(path, root, max_width, 0.4).unwrap(),
                expected
            );
        }
    }

    #[test]
    fn project_path_falls_back_to_generic_truncation_when_fully_abbreviated_is_too_wide() {
        let path = "~/.local/share/chezmoi/dot_config/nvim/lua/lualine/components";
        let root = "~/.local/share/chezmoi";
        let max_width = 20;

        assert_eq!(
            abbreviated_project_path(path, root, max_width, 0.4).unwrap(),
            truncated_text(path, max_width, 0.4)
        );
    }

    #[test]
    fn render_path_body_uses_generic_truncation_without_project_root() {
        let path = PathTitle {
            icon: "\u{F0770}".to_string(),
            display_path: "~/.local/share/chezmoi".to_string(),
            project_root_display_path: None,
        };
        let rendered = render_path_body(&path, 12, 0.4);

        assert!(rendered.starts_with("\u{F0770} "));
        assert!(rendered.contains('…'));
    }
}
