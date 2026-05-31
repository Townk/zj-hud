use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Returns the first `max_cols` display columns of `text`.
/// Handles multi-byte Unicode correctly by iterating char-by-char.
fn truncate_right(text: &str, max_cols: usize) -> &str {
    if max_cols == 0 {
        return &text[..0];
    }
    let mut acc = 0usize;
    for (byte_idx, ch) in text.char_indices() {
        let w = UnicodeWidthChar::width(ch).unwrap_or(0);
        if acc + w > max_cols {
            return &text[..byte_idx];
        }
        acc += w;
    }
    text
}

/// Returns the last `max_cols` display columns of `text`.
/// Handles multi-byte Unicode correctly by iterating char-by-char from the end.
fn truncate_left(text: &str, max_cols: usize) -> &str {
    if max_cols == 0 {
        return &text[text.len()..];
    }
    let total_width = UnicodeWidthStr::width(text);
    if total_width <= max_cols {
        return text;
    }
    // How many columns to skip from the front
    let skip_cols = total_width - max_cols;
    let mut acc = 0usize;
    for (byte_idx, ch) in text.char_indices() {
        let w = UnicodeWidthChar::width(ch).unwrap_or(0);
        if acc + w > skip_cols {
            // This char straddles the boundary — include it only if it fits exactly
            // (i.e. acc == skip_cols). Otherwise skip it too.
            if acc == skip_cols {
                return &text[byte_idx..];
            } else {
                // Wide char that partially overlaps — skip it, start after
                let next = byte_idx + ch.len_utf8();
                return &text[next..];
            }
        }
        acc += w;
        if acc == skip_cols {
            let next = byte_idx + ch.len_utf8();
            return &text[next..];
        }
    }
    // Entire text fits
    text
}

/// Truncates `text` to at most `max_length` display columns, inserting an
/// ellipsis at `truncation_point` (0.0 = prefix only kept at end, 1.0 = suffix
/// only kept at start, 0.4 = default 40% prefix / 60% suffix).
///
/// Implements spec §5.3.1.
pub fn truncated_text(text: &str, max_length: usize, truncation_point: f64) -> String {
    if max_length == 0 {
        return String::new();
    }
    if UnicodeWidthStr::width(text) <= max_length {
        return text.to_string();
    }

    // Clamp truncation_point
    let min_multiplier = 1.0 / max_length as f64;
    let tp = if truncation_point > 1.0 - min_multiplier {
        1.0f64
    } else if truncation_point < min_multiplier {
        0.0f64
    } else {
        truncation_point
    };

    // Choose ellipsis based on clamped truncation point
    let ellipsis: &str = if tp == 0.0 {
        "… "
    } else if tp == 1.0 {
        " …"
    } else {
        " … "
    };

    let ell_w = UnicodeWidthStr::width(ellipsis);
    let available = max_length.saturating_sub(ell_w);

    if available == 0 {
        // Return ellipsis truncated to max_length
        return truncate_right(ellipsis, max_length).to_string();
    }

    // floor(x + 0.5) == round-half-up
    let prefix_length = (available as f64 * tp + 0.5).floor() as usize;
    let suffix_length = available - prefix_length;

    let left = truncate_right(text, prefix_length);
    let right = truncate_left(text, suffix_length);

    format!("{}{}{}", left, ellipsis, right)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_text_unchanged() {
        assert_eq!(truncated_text("hello", 10, 0.4), "hello");
    }

    #[test]
    fn exact_fit_unchanged() {
        assert_eq!(truncated_text("hello", 5, 0.4), "hello");
    }

    #[test]
    fn truncation_with_default_point() {
        // "hello world!" is 12 cells, max 8
        // ellipsis = " … " (3 cells), available = 5
        // prefix_length = floor(5 * 0.4 + 0.5) = floor(2.5) = 2
        // suffix_length = 5 - 2 = 3
        let result = truncated_text("hello world!", 8, 0.4);
        assert_eq!(result, "he … ld!");
    }

    #[test]
    fn truncation_point_zero() {
        // Clamped to 0 → ellipsis = "… " (2 cells)
        // available = 6, prefix = 0, suffix = 6
        let result = truncated_text("hello world!", 8, 0.0);
        assert_eq!(result, "… world!");
    }

    #[test]
    fn truncation_point_one() {
        // Clamped to 1 → ellipsis = " …" (2 cells)
        // available = 6, prefix = 6, suffix = 0
        let result = truncated_text("hello world!", 8, 1.0);
        assert_eq!(result, "hello  …");
    }

    #[test]
    fn empty_text() {
        assert_eq!(truncated_text("", 10, 0.4), "");
    }

    #[test]
    fn max_length_zero() {
        assert_eq!(truncated_text("hello", 0, 0.4), "");
    }
}
