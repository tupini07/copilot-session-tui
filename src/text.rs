//! Terminal-safe string measurement and truncation.
//!
//! Session names routinely contain emoji, which breaks the two naive approaches:
//! byte slicing panics mid-codepoint, and counting `char`s undercounts the cells an
//! emoji actually occupies. Everything that trims text for a fixed-width column should
//! go through here.

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// Columns a string occupies in a terminal.
pub fn display_width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

/// Shorten `text` to at most `budget` columns, appending `…` when anything was cut.
///
/// Splitting happens on grapheme clusters, so multi-codepoint emoji (flags, skin-tone
/// and ZWJ sequences) are never sliced in half.
pub fn truncate_to_width(text: &str, budget: usize) -> String {
    if display_width(text) <= budget {
        return text.to_string();
    }
    if budget == 0 {
        return String::new();
    }

    // Reserve a column for the ellipsis itself.
    let target = budget.saturating_sub(1);
    let mut out = String::new();
    let mut width = 0;
    for grapheme in text.graphemes(true) {
        let grapheme_width = display_width(grapheme);
        if width + grapheme_width > target {
            break;
        }
        out.push_str(grapheme);
        width += grapheme_width;
    }
    out.push('…');
    out
}

/// Pad `text` with spaces to exactly `width` columns, truncating when it is too long.
///
/// `format!("{:<width$}")` pads by `char` count, which leaves emoji-bearing columns
/// misaligned by one cell per emoji.
pub fn pad_to_width(text: &str, width: usize) -> String {
    let truncated = truncate_to_width(text, width);
    let padding = width.saturating_sub(display_width(&truncated));
    format!("{truncated}{}", " ".repeat(padding))
}

/// Wrap text to terminal columns while preserving source line breaks and indentation.
pub fn wrap_text(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return text.lines().map(str::to_string).collect();
    }
    let mut output = Vec::new();
    for source in text.split('\n') {
        if source.is_empty() {
            output.push(String::new());
            continue;
        }
        if display_width(source) <= width {
            output.push(source.to_string());
            continue;
        }

        let indent: String = source
            .chars()
            .take_while(|character| matches!(character, ' ' | '\t'))
            .collect();
        let mut current = indent.clone();
        let content = &source[indent.len()..];
        for segment in content.split_word_bounds() {
            if display_width(&current) + display_width(segment) <= width {
                current.push_str(segment);
                continue;
            }
            if display_width(&current) > display_width(&indent) {
                output.push(current.trim_end_matches([' ', '\t']).to_string());
                current = indent.clone();
            }
            if display_width(&indent) + display_width(segment) <= width {
                current.push_str(segment);
                continue;
            }

            let available = width.saturating_sub(display_width(&indent)).max(1);
            let chunks = hard_wrap(segment, available);
            let last = chunks.len().saturating_sub(1);
            for (index, chunk) in chunks.into_iter().enumerate() {
                if index == last {
                    current.push_str(&chunk);
                } else {
                    output.push(format!("{indent}{chunk}"));
                }
            }
        }
        if !current.is_empty() {
            output.push(current);
        }
    }
    output
}

fn hard_wrap(text: &str, width: usize) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut chunk = String::new();
    let mut used = 0;
    for grapheme in text.graphemes(true) {
        let grapheme_width = display_width(grapheme);
        if !chunk.is_empty() && used + grapheme_width > width {
            chunks.push(chunk);
            chunk = String::new();
            used = 0;
        }
        chunk.push_str(grapheme);
        used += grapheme_width;
    }
    if !chunk.is_empty() {
        chunks.push(chunk);
    }
    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_emoji_counts_as_the_two_columns_it_occupies() {
        assert_eq!(display_width("🚀"), 2);
        assert_eq!(display_width("ab"), 2);
    }

    #[test]
    fn truncation_never_splits_an_emoji() {
        // The budget lands mid-rocket; the emoji must be dropped whole.
        let out = truncate_to_width("🚀🚀🚀", 3);
        assert!(out.chars().all(|c| c == '🚀' || c == '…'), "got {out:?}");
        assert!(display_width(&out) <= 3, "got {out:?}");
    }

    #[test]
    fn truncation_keeps_multi_codepoint_emoji_intact() {
        // A ZWJ family sequence is one grapheme made of many chars.
        let family = "👨‍👩‍👧";
        let out = truncate_to_width(&format!("{family}{family}"), 3);
        assert!(
            out == "…" || out.starts_with(family),
            "a ZWJ sequence must not be split, got {out:?}"
        );
    }

    #[test]
    fn short_text_is_returned_unchanged() {
        assert_eq!(truncate_to_width("hello", 10), "hello");
        assert_eq!(truncate_to_width("🚀 hi", 10), "🚀 hi");
    }

    #[test]
    fn padding_aligns_emoji_columns_to_the_requested_width() {
        assert_eq!(display_width(&pad_to_width("🚀", 6)), 6);
        assert_eq!(display_width(&pad_to_width("ab", 6)), 6);
        assert_eq!(display_width(&pad_to_width("🚀🚀🚀🚀", 6)), 6);
    }

    #[test]
    fn a_zero_budget_yields_nothing() {
        assert_eq!(truncate_to_width("🚀", 0), "");
    }

    #[test]
    fn wrapping_preserves_paragraphs_and_indentation() {
        assert_eq!(
            wrap_text("  one two three\n\nnext", 11),
            vec!["  one two", "  three", "", "next"]
        );
    }

    #[test]
    fn wrapping_breaks_long_unicode_words_on_graphemes() {
        let wrapped = wrap_text("🚀🚀🚀", 4);

        assert_eq!(wrapped, vec!["🚀🚀", "🚀"]);
    }

    #[test]
    fn wrapping_does_not_collapse_preformatted_whitespace() {
        assert_eq!(wrap_text("a  b\tc", 80), vec!["a  b\tc"]);
    }
}
