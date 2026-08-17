use crate::helper::{char_width, split_str_at};
use ratatui_core::{style::Style, text::Span};
use std::borrow::Cow;

#[derive(Clone, Copy)]
struct StyledChar {
    character: char,
    style: Style,
}

#[derive(Default)]
pub(crate) struct LineWrapper;

impl LineWrapper {
    pub(crate) fn wrap_line(line: &[char], max_width: usize, tab_width: usize) -> Vec<Vec<char>> {
        wrap_items(
            line.iter().copied(),
            max_width,
            |character| char_width(*character, tab_width),
            |character| character.is_whitespace(),
        )
    }

    pub(crate) fn wrap_spans<'a>(
        spans: Vec<Span<'a>>,
        max_width: usize,
        tab_width: usize,
    ) -> Vec<Vec<Span<'a>>> {
        let characters = spans.into_iter().flat_map(|span| {
            let style = span.style;
            span.content
                .chars()
                .map(move |character| StyledChar { character, style })
                .collect::<Vec<_>>()
        });
        wrap_items(
            characters,
            max_width,
            |character| char_width(character.character, tab_width),
            |character| character.character.is_whitespace(),
        )
        .into_iter()
        .map(styled_chars_to_spans)
        .collect()
    }

    fn split_str_at(s: Cow<'_, str>, split_at: usize, tab_width: usize) -> (String, String) {
        let mut current_width = 0;
        for (i, ch) in s.chars().enumerate() {
            current_width += char_width(ch, tab_width);
            if current_width > split_at {
                let (a, b) = split_str_at(s, i);
                return (a.to_string(), b.to_string());
            }
        }

        (s.to_string(), String::new())
    }

    fn split_span_at(span: Span, split_at: usize, tab_width: usize) -> (Span, Span) {
        let (a, b) = Self::split_str_at(span.content, split_at, tab_width);
        (Span::styled(a, span.style), Span::styled(b, span.style))
    }
}

fn wrap_items<T>(
    items: impl IntoIterator<Item = T>,
    max_width: usize,
    item_width: impl Fn(&T) -> usize,
    is_whitespace: impl Fn(&T) -> bool,
) -> Vec<Vec<T>> {
    let max_width = max_width.max(1);
    let mut wrapped = Vec::new();
    let mut current = Vec::new();
    let mut current_width = 0;

    for item in items {
        let width = item_width(&item);
        if !current.is_empty() && current_width + width > max_width {
            let word_break = current
                .iter()
                .rposition(&is_whitespace)
                .filter(|index| current[..*index].iter().any(|item| !is_whitespace(item)));
            if let Some(index) = word_break {
                let next = current.split_off(index + 1);
                wrapped.push(std::mem::take(&mut current));
                current = next;
                current_width = current.iter().map(&item_width).sum();
            } else {
                wrapped.push(std::mem::take(&mut current));
                current_width = 0;
            }
        }

        current.push(item);
        current_width += width;
    }

    if !current.is_empty() {
        wrapped.push(current);
    }
    wrapped
}

fn styled_chars_to_spans<'a>(characters: Vec<StyledChar>) -> Vec<Span<'a>> {
    let mut spans = Vec::new();
    let mut text = String::new();
    let mut current_style = None;

    for StyledChar { character, style } in characters {
        if current_style.is_some_and(|current| current != style) {
            spans.push(Span::styled(
                std::mem::take(&mut text),
                current_style.unwrap(),
            ));
        }
        current_style = Some(style);
        text.push(character);
    }
    if let Some(style) = current_style {
        spans.push(Span::styled(text, style));
    }
    spans
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wrap_spans() {
        let spans = vec![Span::raw("Hello"), Span::raw("World")];
        let wrapped_spans = LineWrapper::wrap_spans(spans, 3, 0);
        let text: Vec<String> = wrapped_spans
            .iter()
            .map(|line| line.iter().map(|span| span.content.as_ref()).collect())
            .collect();

        assert_eq!(text, vec!["Hel", "loW", "orl", "d"]);
    }

    #[test]
    fn test_wrap_spans_with_emoji() {
        let spans = vec![Span::raw("Hell🙂!")];
        let wrapped_spans = LineWrapper::wrap_spans(spans, 4, 0);

        assert_eq!(wrapped_spans[0], vec![Span::raw("Hell")]);
        assert_eq!(wrapped_spans[1], vec![Span::raw("🙂!")]);
    }

    #[test]
    fn test_split_span_at_with_emoji() {
        let span = Span::raw("🙂!");
        let (left, right) = LineWrapper::split_span_at(span, 2, 0);

        assert_eq!(left, Span::raw("🙂"));
        assert_eq!(right, Span::raw("!"));
    }

    #[test]
    fn wraps_at_the_last_word_boundary() {
        let line: Vec<char> = "share. Did a ton".chars().collect();

        assert_eq!(
            LineWrapper::wrap_line(&line, 10, 2),
            vec![
                "share. ".chars().collect::<Vec<_>>(),
                "Did a ton".chars().collect::<Vec<_>>(),
            ]
        );
    }

    #[test]
    fn styled_spans_use_the_same_word_boundaries() {
        let spans = vec![
            Span::styled(
                "share. Did",
                Style::new().fg(ratatui_core::style::Color::Red),
            ),
            Span::styled(" a ton", Style::new().fg(ratatui_core::style::Color::Blue)),
        ];

        let wrapped = LineWrapper::wrap_spans(spans, 10, 2);
        let text: Vec<String> = wrapped
            .iter()
            .map(|line| line.iter().map(|span| span.content.as_ref()).collect())
            .collect();

        assert_eq!(text, vec!["share. ", "Did a ton"]);
        assert_eq!(wrapped[1].len(), 2);
    }
}
