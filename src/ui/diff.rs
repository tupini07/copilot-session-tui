use crate::theme::Theme;
use edtui::syntect::easy::HighlightLines;
use edtui::syntect::highlighting::{FontStyle, Style as SyntaxStyle};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use std::path::Path;
use unicode_segmentation::UnicodeSegmentation;

const MAX_HIGHLIGHT_LINE_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DiffKind {
    Context,
    Addition,
    Deletion,
    Hunk,
    Meta,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DiffRow {
    old: Option<u64>,
    new: Option<u64>,
    kind: DiffKind,
    content: String,
}

pub fn render_patch(path: &str, patch: &str, theme: Theme) -> Vec<Line<'static>> {
    let rows = parse_patch(patch);
    let number_width = rows
        .iter()
        .flat_map(|row| [row.old, row.new])
        .flatten()
        .max()
        .map(digits)
        .unwrap_or(1)
        .max(2);
    let path = Path::new(path);
    let syntax = path
        .file_name()
        .and_then(|value| value.to_str())
        .and_then(|name| edtui::SYNTAX_SET.find_syntax_by_extension(name))
        .or_else(|| {
            path.extension()
                .and_then(|value| value.to_str())
                .and_then(|extension| edtui::SYNTAX_SET.find_syntax_by_extension(extension))
        });
    let syntax_theme = edtui::THEME_SET
        .themes
        .get(theme.syntax_theme)
        .or_else(|| {
            edtui::THEME_SET.themes.get(if theme.is_light {
                "inspired-github"
            } else {
                "dracula"
            })
        })
        .or_else(|| edtui::THEME_SET.themes.values().next());
    let mut old_highlighter = syntax
        .zip(syntax_theme)
        .map(|(syntax, syntax_theme)| HighlightLines::new(syntax, syntax_theme));
    let mut new_highlighter = syntax
        .zip(syntax_theme)
        .map(|(syntax, syntax_theme)| HighlightLines::new(syntax, syntax_theme));

    rows.into_iter()
        .map(|row| {
            if row.kind == DiffKind::Hunk {
                old_highlighter = syntax
                    .zip(syntax_theme)
                    .map(|(syntax, syntax_theme)| HighlightLines::new(syntax, syntax_theme));
                new_highlighter = syntax
                    .zip(syntax_theme)
                    .map(|(syntax, syntax_theme)| HighlightLines::new(syntax, syntax_theme));
            }
            render_row(
                row,
                number_width,
                &mut old_highlighter,
                &mut new_highlighter,
                theme,
            )
        })
        .collect()
}

pub fn viewport_lines(
    lines: &[Line<'static>],
    vertical_offset: usize,
    height: usize,
    horizontal_offset: usize,
    width: usize,
    theme: Theme,
) -> Vec<Line<'static>> {
    lines
        .iter()
        .skip(vertical_offset)
        .take(height)
        .map(|line| crop_line(line, horizontal_offset, width, theme.diff_context_bg))
        .collect()
}

fn parse_patch(patch: &str) -> Vec<DiffRow> {
    let mut rows = Vec::new();
    let mut old_line = 0;
    let mut new_line = 0;
    let mut in_hunk = false;
    for line in patch.lines() {
        if let Some((old, new)) = hunk_starts(line) {
            in_hunk = true;
            old_line = old;
            new_line = new;
            rows.push(DiffRow {
                old: None,
                new: None,
                kind: DiffKind::Hunk,
                content: line.to_string(),
            });
        } else if in_hunk {
            if let Some(content) = line.strip_prefix('+') {
                rows.push(DiffRow {
                    old: None,
                    new: Some(new_line),
                    kind: DiffKind::Addition,
                    content: content.to_string(),
                });
                new_line = new_line.saturating_add(1);
            } else if let Some(content) = line.strip_prefix('-') {
                rows.push(DiffRow {
                    old: Some(old_line),
                    new: None,
                    kind: DiffKind::Deletion,
                    content: content.to_string(),
                });
                old_line = old_line.saturating_add(1);
            } else if let Some(content) = line.strip_prefix(' ') {
                rows.push(DiffRow {
                    old: Some(old_line),
                    new: Some(new_line),
                    kind: DiffKind::Context,
                    content: content.to_string(),
                });
                old_line = old_line.saturating_add(1);
                new_line = new_line.saturating_add(1);
            } else {
                rows.push(DiffRow {
                    old: None,
                    new: None,
                    kind: DiffKind::Meta,
                    content: line.to_string(),
                });
            }
        } else {
            rows.push(DiffRow {
                old: None,
                new: None,
                kind: DiffKind::Meta,
                content: line.to_string(),
            });
        }
    }
    rows
}

fn hunk_starts(line: &str) -> Option<(u64, u64)> {
    let body = line.strip_prefix("@@ ")?;
    let end = body.find(" @@")?;
    let mut ranges = body[..end].split_whitespace();
    let old = range_start(ranges.next()?, '-')?;
    let new = range_start(ranges.next()?, '+')?;
    Some((old, new))
}

fn range_start(range: &str, prefix: char) -> Option<u64> {
    range.strip_prefix(prefix)?.split(',').next()?.parse().ok()
}

fn render_row(
    row: DiffRow,
    number_width: usize,
    old_highlighter: &mut Option<HighlightLines<'_>>,
    new_highlighter: &mut Option<HighlightLines<'_>>,
    theme: Theme,
) -> Line<'static> {
    let (background, marker, marker_color) = match row.kind {
        DiffKind::Context => (theme.diff_context_bg, " ", theme.muted),
        DiffKind::Addition => (theme.diff_add_bg, "+", theme.success),
        DiffKind::Deletion => (theme.diff_delete_bg, "-", theme.error),
        DiffKind::Hunk => (theme.diff_hunk_bg, " ", theme.info),
        DiffKind::Meta => (theme.diff_meta_bg, " ", theme.accent),
    };
    let base = Style::default().fg(theme.diff_code_fg).bg(background);
    let gutter_style = Style::default().fg(theme.diff_gutter_fg).bg(background);
    let mut spans = vec![
        Span::styled(" ", gutter_style),
        Span::styled(line_number(row.old, number_width), gutter_style),
        Span::styled(" ", gutter_style),
        Span::styled(line_number(row.new, number_width), gutter_style),
        Span::styled(" ", gutter_style),
        Span::styled(marker, Style::default().fg(marker_color).bg(background)),
        Span::styled("│ ", gutter_style),
    ];

    let mut code = match row.kind {
        DiffKind::Addition => highlight(
            &row.content,
            new_highlighter,
            background,
            theme.diff_code_fg,
        ),
        DiffKind::Deletion => highlight(
            &row.content,
            old_highlighter,
            background,
            theme.diff_code_fg,
        ),
        DiffKind::Context => {
            let _ = highlight(
                &row.content,
                old_highlighter,
                background,
                theme.diff_code_fg,
            );
            highlight(
                &row.content,
                new_highlighter,
                background,
                theme.diff_code_fg,
            )
        }
        DiffKind::Hunk => vec![Span::styled(
            row.content,
            base.fg(theme.info).add_modifier(Modifier::BOLD),
        )],
        DiffKind::Meta => vec![Span::styled(
            row.content,
            base.fg(theme.accent).add_modifier(Modifier::BOLD),
        )],
    };
    spans.append(&mut code);
    Line::from(spans)
}

fn crop_line(
    line: &Line<'_>,
    offset: usize,
    width: usize,
    default_background: Color,
) -> Line<'static> {
    let background = line
        .spans
        .first()
        .and_then(|span| span.style.bg)
        .unwrap_or(default_background);
    let mut source_column = 0;
    let mut output_width = 0;
    let mut output = Vec::new();
    'spans: for span in &line.spans {
        let mut text = String::new();
        for grapheme in span.content.graphemes(true) {
            let grapheme_width = crate::text::display_width(grapheme);
            let grapheme_end = source_column + grapheme_width;
            if grapheme_end <= offset {
                source_column = grapheme_end;
                continue;
            }
            if source_column >= offset + width || output_width >= width {
                if !text.is_empty() {
                    output.push(Span::styled(text, span.style));
                }
                break 'spans;
            }
            let remaining = width - output_width;
            if source_column < offset || grapheme_width > remaining {
                let visible = grapheme_end
                    .saturating_sub(offset.max(source_column))
                    .min(remaining);
                text.push_str(&" ".repeat(visible));
                output_width += visible;
            } else {
                text.push_str(grapheme);
                output_width += grapheme_width;
            }
            source_column = grapheme_end;
        }
        if !text.is_empty() {
            output.push(Span::styled(text, span.style));
        }
    }
    if output_width < width {
        output.push(Span::styled(
            " ".repeat(width - output_width),
            Style::default().bg(background),
        ));
    }
    Line::from(output)
}

#[cfg(test)]
fn line_width(line: &Line<'_>) -> usize {
    line.spans
        .iter()
        .map(|span| crate::text::display_width(&span.content))
        .sum()
}

fn highlight(
    code: &str,
    highlighter: &mut Option<HighlightLines<'_>>,
    background: Color,
    fallback_foreground: Color,
) -> Vec<Span<'static>> {
    if code.len() > MAX_HIGHLIGHT_LINE_BYTES {
        *highlighter = None;
        return vec![Span::styled(
            code.to_string(),
            Style::default().fg(fallback_foreground).bg(background),
        )];
    }
    let Some(highlighter) = highlighter else {
        return vec![Span::styled(
            code.to_string(),
            Style::default().fg(fallback_foreground).bg(background),
        )];
    };
    let source = format!("{code}\n");
    let Ok(regions) = highlighter.highlight_line(&source, &edtui::SYNTAX_SET) else {
        return vec![Span::styled(
            code.to_string(),
            Style::default().fg(fallback_foreground).bg(background),
        )];
    };
    regions
        .into_iter()
        .filter_map(|(style, text)| {
            let text = text.strip_suffix('\n').unwrap_or(text);
            (!text.is_empty())
                .then(|| Span::styled(text.to_string(), syntax_style(style, background)))
        })
        .collect()
}

fn syntax_style(style: SyntaxStyle, background: Color) -> Style {
    let mut modifier = Modifier::empty();
    if style.font_style.contains(FontStyle::BOLD) {
        modifier |= Modifier::BOLD;
    }
    if style.font_style.contains(FontStyle::ITALIC) {
        modifier |= Modifier::ITALIC;
    }
    if style.font_style.contains(FontStyle::UNDERLINE) {
        modifier |= Modifier::UNDERLINED;
    }
    Style::default()
        .fg(Color::Rgb(
            style.foreground.r,
            style.foreground.g,
            style.foreground.b,
        ))
        .bg(background)
        .add_modifier(modifier)
}

fn line_number(number: Option<u64>, width: usize) -> String {
    number.map_or_else(|| " ".repeat(width), |number| format!("{number:>width$}"))
}

fn digits(number: u64) -> usize {
    number.to_string().len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::ThemeName;

    const TEST_THEMES: [ThemeName; 3] = [
        ThemeName::Nord,
        ThemeName::CatppuccinLatte,
        ThemeName::SolarizedLight,
    ];

    #[test]
    fn parser_assigns_old_and_new_line_numbers() {
        let rows = parse_patch("@@ -10,2 +20,3 @@ fn demo()\n context\n-old\n+new\n+more");

        assert_eq!((rows[1].old, rows[1].new), (Some(10), Some(20)));
        assert_eq!((rows[2].old, rows[2].new), (Some(11), None));
        assert_eq!((rows[3].old, rows[3].new), (None, Some(21)));
        assert_eq!((rows[4].old, rows[4].new), (None, Some(22)));
        assert_eq!(rows[2].content, "old");
        assert_eq!(rows[3].content, "new");
    }

    #[test]
    fn source_lines_that_resemble_file_headers_stay_in_the_hunk() {
        let rows = parse_patch("@@ -1,2 +1,2 @@\n--- SQL comment\n+++counter\n next");

        assert_eq!(rows[1].kind, DiffKind::Deletion);
        assert_eq!(rows[1].content, "-- SQL comment");
        assert_eq!((rows[1].old, rows[1].new), (Some(1), None));
        assert_eq!(rows[2].kind, DiffKind::Addition);
        assert_eq!(rows[2].content, "++counter");
        assert_eq!((rows[2].old, rows[2].new), (None, Some(1)));
        assert_eq!((rows[3].old, rows[3].new), (Some(2), Some(2)));
    }

    #[test]
    fn renderer_uses_gutters_and_full_row_diff_backgrounds() {
        let patch =
            "diff --git a/demo.py b/demo.py\n@@ -1,2 +1,2 @@\n context\n-print('old')\n+print('new')\n\\ No newline";
        for name in TEST_THEMES {
            let theme = name.theme();
            let lines = render_patch("demo.py", patch, theme);
            let expected_backgrounds = [
                theme.diff_meta_bg,
                theme.diff_hunk_bg,
                theme.diff_context_bg,
                theme.diff_delete_bg,
                theme.diff_add_bg,
                theme.diff_meta_bg,
            ];

            assert!(lines[3].to_string().contains("  2    -│ print('old')"));
            assert!(lines[4].to_string().contains("     2 +│ print('new')"));
            for (line, expected) in lines.iter().zip(expected_backgrounds) {
                assert!(
                    line.spans
                        .iter()
                        .all(|span| span.style.bg == Some(expected)),
                    "{name:?} row did not use {expected:?}: {line:?}"
                );
                for index in [0, 1, 2, 3, 4, 6] {
                    assert_eq!(
                        line.spans[index].style.fg,
                        Some(theme.diff_gutter_fg),
                        "{name:?} gutter span {index}: {line:?}"
                    );
                }
            }

            let viewport = viewport_lines(&lines, 3, 1, 0, 60, theme);
            assert_eq!(line_width(&viewport[0]), 60);
            assert!(viewport[0]
                .spans
                .iter()
                .all(|span| span.style.bg == Some(theme.diff_delete_bg)));
        }
    }

    #[test]
    fn classic_diff_keeps_the_existing_palette() {
        let theme = ThemeName::Classic.theme();
        let lines = render_patch(
            "demo.rs",
            "diff --git a/demo.rs b/demo.rs\n@@ -1 +1 @@\n-old\n+new",
            theme,
        );

        assert_eq!(lines[0].spans[0].style.bg, Some(Color::Rgb(31, 31, 42)));
        assert_eq!(lines[1].spans[0].style.bg, Some(Color::Rgb(24, 40, 67)));
        assert_eq!(lines[2].spans[0].style.bg, Some(Color::Rgb(62, 25, 31)));
        assert_eq!(lines[3].spans[0].style.bg, Some(Color::Rgb(18, 52, 31)));
        assert_eq!(lines[2].spans[0].style.fg, Some(Color::Rgb(108, 112, 134)));
    }

    #[test]
    fn supported_extensions_receive_syntax_colours() {
        let mut rendered_foregrounds = Vec::new();
        for name in TEST_THEMES {
            let theme = name.theme();
            let lines = render_patch("demo.py", "@@ -0,0 +1 @@\n+def greet(name):", theme);
            let mut foregrounds = Vec::new();
            for foreground in lines[1].spans[7..].iter().filter_map(|span| span.style.fg) {
                if !foregrounds.contains(&foreground) {
                    foregrounds.push(foreground);
                }
            }

            assert!(
                foregrounds.len() > 1,
                "{name:?} expected multiple syntax colours: {foregrounds:?}"
            );
            assert!(lines[1].spans[7..]
                .iter()
                .all(|span| span.style.bg == Some(theme.diff_add_bg)));
            rendered_foregrounds.push(foregrounds);
        }
        assert_ne!(rendered_foregrounds[0], rendered_foregrounds[1]);
        assert_eq!(rendered_foregrounds[1], rendered_foregrounds[2]);
    }

    #[test]
    fn missing_syntax_theme_falls_back_safely() {
        let mut theme = ThemeName::Nord.theme();
        theme.syntax_theme = "missing-cst-theme";

        let lines = render_patch("demo.py", "@@ -0,0 +1 @@\n+def greet(name):", theme);
        let code_foregrounds: Vec<Color> = lines[1].spans[7..]
            .iter()
            .filter_map(|span| span.style.fg)
            .collect();

        assert!(code_foregrounds.len() > 1, "{code_foregrounds:?}");
    }

    #[test]
    fn syntax_background_override_preserves_font_modifiers() {
        let syntax = SyntaxStyle {
            foreground: edtui::syntect::highlighting::Color {
                r: 10,
                g: 20,
                b: 30,
                a: 255,
            },
            background: edtui::syntect::highlighting::Color {
                r: 200,
                g: 210,
                b: 220,
                a: 255,
            },
            font_style: FontStyle::BOLD | FontStyle::ITALIC | FontStyle::UNDERLINE,
        };
        let background = ThemeName::SolarizedLight.theme().diff_add_bg;

        let style = syntax_style(syntax, background);

        assert_eq!(style.fg, Some(Color::Rgb(10, 20, 30)));
        assert_eq!(style.bg, Some(background));
        assert!(style.add_modifier.contains(Modifier::BOLD));
        assert!(style.add_modifier.contains(Modifier::ITALIC));
        assert!(style.add_modifier.contains(Modifier::UNDERLINED));
    }

    #[test]
    fn shorter_rows_keep_their_background_after_horizontal_scrolling() {
        for name in TEST_THEMES {
            let theme = name.theme();
            let lines = render_patch(
                "demo.rs",
                "@@ -1 +1,2 @@\n-short\n+short\n+this line is deliberately much much much longer",
                theme,
            );
            let viewport = viewport_lines(&lines, 1, 2, 15, 30, theme);

            assert_eq!(line_width(&viewport[0]), 30);
            assert_eq!(line_width(&viewport[1]), 30);
            assert!(viewport[0]
                .spans
                .iter()
                .all(|span| span.style.bg == Some(theme.diff_delete_bg)));
            assert!(viewport[1]
                .spans
                .iter()
                .all(|span| span.style.bg == Some(theme.diff_add_bg)));
        }
    }

    #[test]
    fn horizontal_viewport_reveals_columns_beyond_the_initial_width() {
        for name in TEST_THEMES {
            let theme = name.theme();
            let lines = render_patch(
                "demo.txt",
                "@@ -0,0 +1 @@\n+abcdefghijklmnopqrstuvwxyz",
                theme,
            );
            let left = viewport_lines(&lines, 1, 1, 0, 16, theme)[0].to_string();
            let right = viewport_lines(&lines, 1, 1, 16, 16, theme)[0].to_string();

            assert_ne!(left, right, "{name:?}");
            assert!(right.contains("ghijklmnopqrstuv"), "{name:?} got {right:?}");
        }
    }

    #[test]
    fn viewport_crops_wide_graphemes_without_splitting_them() {
        let theme = ThemeName::Nord.theme();
        let lines = vec![Line::from(Span::styled(
            "a🚀bc",
            Style::default().bg(theme.diff_add_bg),
        ))];
        let cropped = viewport_lines(&lines, 0, 1, 2, 3, theme);

        assert_eq!(line_width(&cropped[0]), 3);
        assert!(!cropped[0].to_string().contains('🚀'));
    }
}
