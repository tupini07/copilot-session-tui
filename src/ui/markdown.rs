//! GitHub-flavoured markdown bodies as wrapped, themed lines.
//!
//! Issue and pull request text is written for github.com to render. Shown raw, emphasis,
//! code and links read as a wall of `**`, backticks and brackets, and the instructions a
//! template hid in HTML comments show up as if the author had written them. Heading `#`
//! markers are kept on purpose: colour alone does not tell one heading level from another.
//!
//! Parsing and styling come from `tui-markdown`. Wrapping is done here because the
//! inspector scrolls a `Vec<Line>` it has measured: if ratatui wrapped the paragraph
//! instead, the scrollbar and the scroll limit would be counting different lines from
//! the ones on screen.

use crate::text;
use crate::theme::Theme;
use pulldown_cmark::{Event, Parser, Tag, TagEnd};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use std::borrow::Cow;
use tui_markdown::{AlertKind, Options, StyleSheet};
use unicode_segmentation::UnicodeSegmentation;

/// Render `source` as lines no wider than `width` columns.
pub fn render(source: &str, width: usize, theme: Theme) -> Vec<Line<'static>> {
    let width = width.max(1);
    let source = github_source(source);
    let options = Options::new(ThemeSheet(theme)).table_width(width.min(u16::MAX as usize) as u16);
    let rendered = tui_markdown::from_str_with_options(&source, &options);
    rendered
        .lines
        .into_iter()
        .flat_map(|line| wrap_line(into_owned(line), width))
        .collect()
}

/// Styles drawn from the active CST theme.
///
/// The library defaults are fixed ANSI colours, including a cyan background behind every
/// top-level heading, which is unreadable on the light themes.
#[derive(Clone, Copy)]
struct ThemeSheet(Theme);

impl StyleSheet for ThemeSheet {
    fn heading(&self, level: u8) -> Style {
        let theme = self.0;
        match level {
            1 | 2 => Style::new().fg(theme.accent).add_modifier(Modifier::BOLD),
            3 => Style::new()
                .fg(theme.accent_alt)
                .add_modifier(Modifier::BOLD),
            _ => Style::new().fg(theme.accent_alt),
        }
    }

    fn code(&self) -> Style {
        Style::new().fg(self.0.accent_alt).bg(self.0.surface)
    }

    fn link(&self) -> Style {
        Style::new()
            .fg(self.0.info)
            .add_modifier(Modifier::UNDERLINED)
    }

    fn blockquote(&self) -> Style {
        Style::new().fg(self.0.muted)
    }

    fn heading_meta(&self) -> Style {
        Style::new().fg(self.0.muted)
    }

    fn metadata_block(&self) -> Style {
        Style::new().fg(self.0.muted)
    }

    fn html(&self) -> Style {
        Style::new().fg(self.0.muted)
    }

    fn math_inline(&self) -> Style {
        Style::new().fg(self.0.accent_alt)
    }

    fn math_display(&self) -> Style {
        Style::new().fg(self.0.accent_alt)
    }

    fn footnote_ref(&self) -> Style {
        Style::new().fg(self.0.muted)
    }

    fn footnote_def(&self) -> Style {
        Style::new().fg(self.0.muted)
    }

    fn alert(&self, kind: AlertKind) -> Style {
        let theme = self.0;
        Style::new().fg(match kind {
            AlertKind::Note => theme.info,
            AlertKind::Tip => theme.success,
            AlertKind::Important => theme.accent_alt,
            AlertKind::Warning => theme.warning,
            AlertKind::Caution => theme.error,
        })
    }

    // The default icons carry a variation selector that terminals disagree on the width
    // of, which shifts the rest of the line by a cell in some of them.
    fn alert_icon(&self, _kind: AlertKind) -> &str {
        ""
    }

    fn table_header(&self) -> Style {
        Style::new().fg(self.0.accent).add_modifier(Modifier::BOLD)
    }

    fn table_border(&self) -> Style {
        Style::new().fg(self.0.inactive)
    }

    fn list_marker(&self) -> Style {
        Style::new().fg(self.0.accent_alt)
    }

    fn image_alt(&self) -> Style {
        Style::new().fg(self.0.muted).add_modifier(Modifier::ITALIC)
    }
}

/// Rewrite `source` so CommonMark renders it the way github.com shows issue and PR text.
///
/// Two rules differ. A single newline is a line break on GitHub, where CommonMark joins
/// the lines, so a short review comment written line by line would run together. And
/// HTML comments are dropped: templates use them for instructions to the author, so
/// nearly every templated body carries several. Both are found by the parser rather
/// than by searching the text, so a code block keeps its newlines and any `<!--` in it.
fn github_source(source: &str) -> Cow<'_, str> {
    let mut edits = Vec::new();
    let mut block_start = None;
    let mut item_depth = 0usize;
    for (event, range) in Parser::new_ext(source, PARSE_OPTIONS).into_offset_iter() {
        match event {
            Event::Start(Tag::HtmlBlock) => block_start = Some(range.start),
            Event::End(TagEnd::HtmlBlock) => {
                if let Some(start) = block_start.take() {
                    if is_comment(&source[start..range.end]) {
                        edits.push((start..range.end, ""));
                    }
                }
            }
            Event::InlineHtml(html) if is_comment(&html) => edits.push((range, "")),
            Event::Start(Tag::Item) => item_depth += 1,
            Event::End(TagEnd::Item) => item_depth -= 1,
            // A backslash before a newline is CommonMark's hard break. Inside a list item
            // the lines stay joined: tui-markdown starts the line after a break at the
            // margin, which would read as the item having ended, while a joined item still
            // wraps under its own text.
            Event::SoftBreak if item_depth == 0 => edits.push((range.start..range.start, "\\")),
            _ => {}
        }
    }
    if edits.is_empty() {
        return Cow::Borrowed(source);
    }
    edits.sort_by_key(|(range, _)| range.start);
    let mut rewritten = String::with_capacity(source.len() + edits.len());
    let mut cursor = 0;
    for (range, replacement) in edits {
        if range.start < cursor {
            continue;
        }
        rewritten.push_str(&source[cursor..range.start]);
        rewritten.push_str(replacement);
        cursor = range.end;
    }
    rewritten.push_str(&source[cursor..]);
    Cow::Owned(rewritten)
}

/// The extensions `tui-markdown` parses with, so this pass sees the same structure.
const PARSE_OPTIONS: pulldown_cmark::Options = pulldown_cmark::Options::ENABLE_STRIKETHROUGH
    .union(pulldown_cmark::Options::ENABLE_TASKLISTS)
    .union(pulldown_cmark::Options::ENABLE_HEADING_ATTRIBUTES)
    .union(pulldown_cmark::Options::ENABLE_YAML_STYLE_METADATA_BLOCKS)
    .union(pulldown_cmark::Options::ENABLE_SUPERSCRIPT)
    .union(pulldown_cmark::Options::ENABLE_SUBSCRIPT)
    .union(pulldown_cmark::Options::ENABLE_MATH)
    .union(pulldown_cmark::Options::ENABLE_FOOTNOTES)
    .union(pulldown_cmark::Options::ENABLE_DEFINITION_LIST)
    .union(pulldown_cmark::Options::ENABLE_GFM)
    .union(pulldown_cmark::Options::ENABLE_TABLES);

fn is_comment(html: &str) -> bool {
    let html = html.trim();
    html.starts_with("<!--") && html.ends_with("-->")
}

fn into_owned(line: Line<'_>) -> Line<'static> {
    Line {
        style: line.style,
        alignment: line.alignment,
        spans: line
            .spans
            .into_iter()
            .map(|span| Span::styled(span.content.into_owned(), span.style))
            .collect(),
    }
}

/// Word-wrap a styled line, keeping each word's style across the break.
///
/// Continuation lines hang under the text of a list item and keep a blockquote's `>`
/// markers, so a wrapped bullet still reads as one bullet.
fn wrap_line(line: Line<'static>, width: usize) -> Vec<Line<'static>> {
    if line.width() <= width {
        return vec![line];
    }
    let line_style = line.style;
    let mut pieces = Vec::new();
    for span in &line.spans {
        for run in blank_runs(&span.content) {
            pieces.push((run, span.style));
        }
    }
    // A word is every adjacent non-blank piece, so the comma after a bold word or a link
    // stays with it instead of starting the next line.
    let mut words: Vec<Vec<(&str, Style)>> = Vec::new();
    for piece in &pieces {
        let blank = piece.0.trim().is_empty();
        match words.last_mut() {
            Some(word) if word[0].0.trim().is_empty() == blank => word.push(*piece),
            _ => words.push(vec![*piece]),
        }
    }

    let mut hanging = hanging_indent(&pieces);
    // A deeply nested item in a narrow pane would otherwise leave no room for its text.
    if text::display_width(&spans_text(&hanging)) * 2 > width {
        hanging.clear();
    }
    let hanging_width = text::display_width(&spans_text(&hanging));

    let mut output = Vec::new();
    let mut current: Vec<Span<'static>> = Vec::new();
    let mut used = 0;
    let mut line_start = 0;
    for word in words {
        let word_width: usize = word.iter().map(|(part, _)| text::display_width(part)).sum();
        if used + word_width <= width {
            for (part, style) in word {
                push_piece(&mut current, part, style);
            }
            used += word_width;
            continue;
        }
        if used > line_start {
            output.push(finish_line(std::mem::take(&mut current), line_style));
            current = hanging.clone();
            used = hanging_width;
            line_start = hanging_width;
        }
        if word[0].0.trim().is_empty() {
            continue;
        }
        if used + word_width <= width {
            for (part, style) in word {
                push_piece(&mut current, part, style);
            }
            used += word_width;
            continue;
        }
        for (part, style) in word {
            for grapheme in part.graphemes(true) {
                let grapheme_width = text::display_width(grapheme);
                if used + grapheme_width > width && used > line_start {
                    output.push(finish_line(std::mem::take(&mut current), line_style));
                    current = hanging.clone();
                    used = hanging_width;
                    line_start = hanging_width;
                }
                push_piece(&mut current, grapheme, style);
                used += grapheme_width;
            }
        }
    }
    if used > line_start || output.is_empty() {
        output.push(finish_line(current, line_style));
    }
    output
}

/// The leading run of a line that continuation lines should line up under: indentation,
/// blockquote markers and a list marker. Everything but the `>` markers becomes blank.
fn hanging_indent(pieces: &[(&str, Style)]) -> Vec<Span<'static>> {
    let source: String = pieces.iter().map(|(segment, _)| *segment).collect();
    let prefix_len = list_prefix_len(&source);
    let mut spans = Vec::new();
    let mut taken = 0;
    for (segment, style) in pieces {
        if taken >= prefix_len {
            break;
        }
        let part = &segment[..segment.len().min(prefix_len - taken)];
        taken += part.len();
        let blanked: String = part
            .chars()
            .map(|character| if character == '>' { '>' } else { ' ' })
            .collect();
        let style = if part.contains('>') {
            *style
        } else {
            Style::default()
        };
        spans.push(Span::styled(blanked, style));
    }
    spans
}

/// Byte length of the indentation, `>` markers and list marker at the start of `line`.
fn list_prefix_len(line: &str) -> usize {
    let bytes = line.as_bytes();
    let mut index = 0;
    loop {
        while index < bytes.len() && bytes[index] == b' ' {
            index += 1;
        }
        if bytes.get(index) == Some(&b'>') {
            index += 1;
            continue;
        }
        break;
    }
    let marker_start = index;
    if bytes.get(index) == Some(&b'-') {
        index += 1;
    } else {
        while index < bytes.len() && bytes[index].is_ascii_digit() {
            index += 1;
        }
        if index == marker_start || bytes.get(index) != Some(&b'.') {
            return marker_start;
        }
        index += 1;
    }
    if bytes.get(index) != Some(&b' ') {
        return marker_start;
    }
    index += 1;
    for task in ["[ ] ", "[x] ", "[X] "] {
        if line[index..].starts_with(task) {
            index += task.len();
            break;
        }
    }
    index
}

/// `text` split into alternating runs of whitespace and everything else.
fn blank_runs(text: &str) -> impl Iterator<Item = &str> {
    let mut rest = text;
    std::iter::from_fn(move || {
        let blank = rest.chars().next()?.is_whitespace();
        let end = rest
            .find(|character: char| character.is_whitespace() != blank)
            .unwrap_or(rest.len());
        let (run, tail) = rest.split_at(end);
        rest = tail;
        Some(run)
    })
}

fn push_piece(spans: &mut Vec<Span<'static>>, text: &str, style: Style) {
    match spans.last_mut() {
        Some(last) if last.style == style => last.content.to_mut().push_str(text),
        _ => spans.push(Span::styled(text.to_string(), style)),
    }
}

fn finish_line(mut spans: Vec<Span<'static>>, style: Style) -> Line<'static> {
    while let Some(last) = spans.last_mut() {
        let trimmed = last.content.trim_end().len();
        if trimmed > 0 {
            last.content.to_mut().truncate(trimmed);
            break;
        }
        spans.pop();
    }
    Line::from(spans).style(style)
}

fn spans_text(spans: &[Span<'_>]) -> String {
    spans.iter().map(|span| span.content.as_ref()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::ThemeName;

    fn plain(lines: &[Line<'_>]) -> Vec<String> {
        lines.iter().map(|line| line.to_string()).collect()
    }

    #[test]
    fn no_rendered_line_is_wider_than_the_width_it_was_given_so_the_scroll_limit_is_honest() {
        let body = "A paragraph long enough that it has to wrap several times at this width.\n\n\
                    - a bullet whose text also runs well past the edge of the pane\n\
                    > a quotation that keeps going for quite a while as well\n\n\
                    `averyveryveryverylongidentifierwithnobreaks`";
        for width in [12, 20, 37] {
            for line in render(body, width, ThemeName::Nord.theme()) {
                assert!(line.width() <= width, "{line:?} wider than {width}");
            }
        }
    }

    #[test]
    fn a_wrapped_bullet_continues_under_its_text_rather_than_under_the_dash() {
        let lines = plain(&render(
            "- first second third fourth",
            14,
            ThemeName::Nord.theme(),
        ));
        assert_eq!(lines, ["- first second", "  third fourth"]);
    }

    #[test]
    fn punctuation_after_markup_stays_with_its_word_instead_of_opening_the_next_line() {
        let lines = plain(&render("aaa **nests it**, so", 12, ThemeName::Nord.theme()));
        assert_eq!(lines, ["aaa nests", "it, so"]);
    }

    #[test]
    fn a_comment_written_line_by_line_keeps_its_lines_as_github_shows_them() {
        let lines = plain(&render(
            "Looks good.
One nit below.

- item
  continued

```
a
b
```",
            80,
            ThemeName::Nord.theme(),
        ));
        assert_eq!(
            lines,
            [
                "Looks good.",
                "One nit below.",
                "",
                "- item continued",
                "",
                "```",
                "a",
                "b",
                "```",
            ]
        );
    }

    #[test]
    fn a_wrapped_quotation_keeps_its_marker_on_every_line() {
        let lines = plain(&render("> one two three four", 10, ThemeName::Nord.theme()));
        assert!(lines.len() > 1, "got {lines:?}");
        assert!(
            lines.iter().all(|line| line.starts_with("> ")),
            "got {lines:?}"
        );
    }

    #[test]
    fn template_instructions_in_html_comments_are_not_shown_as_if_the_author_wrote_them() {
        let body = "## Summary\n\n<!-- Describe your change here. -->\n\nFixes the thing \
                    <!-- inline note --> today.\n\n```html\n<!-- kept in code -->\n```";
        let text = plain(&render(body, 80, ThemeName::Nord.theme())).join("\n");
        assert!(!text.contains("Describe your change"), "got:\n{text}");
        assert!(!text.contains("inline note"), "got:\n{text}");
        assert!(text.contains("Fixes the thing"), "got:\n{text}");
        assert!(text.contains("<!-- kept in code -->"), "got:\n{text}");
    }

    #[test]
    fn markup_is_styled_with_theme_colours_instead_of_shown_as_syntax() {
        let theme = ThemeName::Nord.theme();
        let lines = render("## Heading\n\nSome **bold** and `code`.", 80, theme);
        // A heading is styled on its line rather than its spans, so compare what is drawn.
        let drawn = |text: &str| {
            lines
                .iter()
                .flat_map(|line| {
                    line.spans
                        .iter()
                        .map(|span| (line.style.patch(span.style), span))
                })
                .find(|(_, span)| span.content.contains(text))
                .map(|(style, _)| style)
                .unwrap_or_else(|| panic!("{text} not rendered"))
        };
        assert_eq!(drawn("Heading").fg, Some(theme.accent));
        assert!(drawn("bold").add_modifier.contains(Modifier::BOLD));
        assert_eq!(drawn("code").bg, Some(theme.surface));
        assert!(!plain(&lines).join("\n").contains("**"));
    }
}
