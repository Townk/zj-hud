//! Lenient KDL parsing helpers shared by every role.
//!
//! Zellij forwards plugin configuration blocks as raw strings; each role
//! ([`crate::bar::config`], [`crate::search`], [`crate::whichkey::config`])
//! parses its own block. These helpers tolerate the looser KDL-ish syntax users
//! write in their config (e.g. `key = value`, `child = value`) by normalizing it
//! before handing it to the strict [`kdl`] parser.

use kdl::{KdlDocument, KdlValue};

/// Parse a config block into a [`KdlDocument`], tolerating common shorthand.
///
/// Tries strict KDL first, then progressively normalizes `child = value`
/// assignments (for the given `child_assignment_keys`) and `key = value`
/// spaced-equals forms before retrying.
pub(crate) fn parse_config_document(
    value: &str,
    child_assignment_keys: &[&str],
) -> Option<KdlDocument> {
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

/// Collapse spaces around `=` (outside string literals) so `key = value`
/// shorthand parses as strict KDL `key=value`. Exposed for callers that only
/// need the spaced-equals fallback (e.g. the bar's status block).
pub(crate) fn normalize_spaced_equals(value: &str) -> String {
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

/// Stringify any [`KdlValue`] for the string-keyed config maps the roles use,
/// covering strings, integers, floats and booleans (not just `as_string`).
pub(crate) fn kdl_value_to_config_string(value: &KdlValue) -> String {
    value
        .as_string()
        .map(str::to_string)
        .or_else(|| value.as_i64().map(|n| n.to_string()))
        .or_else(|| value.as_f64().map(|n| n.to_string()))
        .or_else(|| value.as_bool().map(|b| b.to_string()))
        .unwrap_or_else(|| value.to_string())
}
