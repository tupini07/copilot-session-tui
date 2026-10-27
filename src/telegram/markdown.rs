//! Markdown, as an agent writes it, in the HTML Telegram can show.
//!
//! Copilot answers in Markdown: headings, bullet lists, fenced code, tables. Sent as
//! text, every `**` and `###` arrives as a literal and the message reads as a diff of
//! itself. Telegram's HTML mode is a handful of inline tags — bold, italic,
//! strikethrough, code, pre, links, blockquotes — and nothing for a heading, a list or
//! a table, so each of those has to become something that reads right in plain text:
//! a bold line, an indented bullet, an aligned block in monospace.
//!
//! Two properties hold throughout and are tested:
//!
//! - **Text that is not markup converts to exactly [`escape_html`] of itself**, so an
//!   agent's prose is never reflowed or reworded, and the rest of the channel, which
//!   splits messages at the agent's own line breaks, keeps working unchanged.
//! - **Nothing the agent wrote is ever a tag.** Raw HTML in the Markdown — `<br>`,
//!   `<details>`, a `<placeholder>` — is shown as text. Telegram rejects a message with
//!   an unknown tag outright, and losing the whole message over a `<br>` is the one
//!   outcome worse than showing the Markdown raw.
//!
//! Telegram's own rules on nesting shape the output: `pre` and `code` may contain no
//! other entity, and a blockquote cannot sit inside a blockquote.

use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use unicode_width::UnicodeWidthStr;

use super::escape_html;
use crate::github::RepositoryRef;

/// What is parsed: the extensions Copilot's output actually uses. Not the full set the
/// GitHub inspector enables — superscript, math and smart punctuation would eat `^`,
/// `$` and straight quotes out of ordinary prose.
const OPTIONS: Options = Options::ENABLE_TABLES
    .union(Options::ENABLE_STRIKETHROUGH)
    .union(Options::ENABLE_TASKLISTS);

/// The longest language name put in a code block's class. A fence names one word;
/// anything longer is a caption, not a language.
const LANGUAGE_LIMIT: usize = 32;

/// Render `markdown` as Telegram HTML.
pub fn to_telegram_html(markdown: &str) -> String {
    to_telegram_html_in(markdown, None)
}

/// [`to_telegram_html`], with every `#1234` in the prose linked to that number in
/// `repository`, the one the session's folder belongs to.
///
/// Prose only: a `#12` inside code is whatever the code means by it, and one inside a
/// link already goes somewhere. The same scanner the pane uses for its click targets
/// decides what is a reference, so the phone and the screen agree.
pub fn to_telegram_html_in(markdown: &str, repository: Option<&RepositoryRef>) -> String {
    let mut renderer = Renderer {
        repository: repository.cloned(),
        ..Renderer::default()
    };
    for event in Parser::new_ext(markdown, OPTIONS) {
        renderer.event(event);
    }
    renderer.finish()
}

/// `text` escaped, with its references wrapped in links to `repository`.
fn linkify(text: &str, repository: &RepositoryRef) -> String {
    let mut out = String::with_capacity(text.len());
    let mut from = 0;
    for (span, number) in crate::github::reference_spans(text) {
        out.push_str(&escape_html(&text[from..span.start]));
        out.push_str(&format!(
            "<a href=\"{}\">{}</a>",
            escape_attribute(&repository.issue_url(number)),
            escape_html(&text[span.clone()])
        ));
        from = span.end;
    }
    out.push_str(&escape_html(&text[from..]));
    out
}

/// Remove the tags from HTML this module produced, keeping the text and its entities.
///
/// The fallback when Telegram rejects a message's markup: what remains is what the
/// channel sent before it knew Markdown, so nothing is lost but the formatting. Safe
/// only on HTML written here, where every `<` the agent typed is already `&lt;`.
pub fn strip_tags(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    for character in html.chars() {
        match character {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if in_tag => {}
            other => out.push(other),
        }
    }
    out
}

#[derive(Default)]
struct Renderer {
    out: String,
    /// A block has ended; the separator is written when the next content starts, so a
    /// nested structure ending does not pile up blank lines and the output does not
    /// end with any.
    separator_pending: bool,
    /// One entry per open list: the number the next ordered item takes, or `None` for
    /// bullets.
    lists: Vec<Option<u64>>,
    quote_depth: usize,
    /// Inside a table, cells are collected as plain text and laid out at the end.
    table: Option<Table>,
    /// Where the current image's alt text began, to fall back to its address.
    image_start: Option<usize>,
    /// Where a `#1234` in the prose points, when the session's repository is known.
    repository: Option<RepositoryRef>,
    /// Open links and images: text inside one is already a link's text.
    link_depth: usize,
    /// Open code blocks: text inside one is code, not prose.
    code_depth: usize,
}

#[derive(Default)]
struct Table {
    rows: Vec<Vec<String>>,
    current: Vec<String>,
    cell: String,
    in_cell: bool,
}

impl Renderer {
    fn event(&mut self, event: Event<'_>) {
        if self.table.is_some() {
            return self.table_event(event);
        }
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(text) => match &self.repository {
                Some(repository) if self.link_depth == 0 && self.code_depth == 0 => {
                    self.out.push_str(&linkify(&text, repository));
                }
                _ => self.out.push_str(&escape_html(&text)),
            },
            Event::Code(code) => {
                self.out.push_str("<code>");
                self.out.push_str(&escape_html(&code));
                self.out.push_str("</code>");
            }
            // Raw HTML is the agent's text, never markup Telegram should read.
            Event::Html(html) | Event::InlineHtml(html) => {
                self.out.push_str(&escape_html(&html));
            }
            Event::SoftBreak | Event::HardBreak => self.out.push('\n'),
            Event::Rule => {
                self.begin_block();
                self.out.push_str("———");
                self.end_block();
            }
            Event::TaskListMarker(checked) => {
                // Arrives right after the item's bullet, and stands in for it.
                if self.out.ends_with("• ") {
                    self.out.truncate(self.out.len() - "• ".len());
                }
                self.out.push_str(if checked { "☑ " } else { "☐ " });
            }
            Event::FootnoteReference(name) => {
                self.out.push_str(&escape_html(&format!("[{name}]")));
            }
            Event::InlineMath(text) | Event::DisplayMath(text) => {
                self.out.push_str(&escape_html(&text));
            }
        }
    }

    fn start(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph => self.begin_block(),
            Tag::Heading { .. } => {
                self.begin_block();
                self.out.push_str("<b>");
            }
            Tag::BlockQuote(_) => {
                self.begin_block();
                if self.quote_depth == 0 {
                    self.out.push_str("<blockquote>");
                }
                self.quote_depth += 1;
            }
            Tag::CodeBlock(kind) => {
                self.code_depth += 1;
                self.begin_block();
                self.out.push_str("<pre>");
                let language = match kind {
                    CodeBlockKind::Fenced(info) => language_of(&info),
                    CodeBlockKind::Indented => None,
                };
                match language {
                    Some(language) => self
                        .out
                        .push_str(&format!("<code class=\"language-{language}\">")),
                    None => self.out.push_str("<code>"),
                }
            }
            Tag::HtmlBlock => self.begin_block(),
            Tag::List(start) => {
                // A list nested in a tight item follows that item's text directly,
                // with no paragraph end to put it on its own line.
                if !self.lists.is_empty() {
                    self.separator_pending = true;
                }
                self.begin_block();
                self.lists.push(start);
            }
            Tag::Item => {
                self.begin_block();
                let depth = self.lists.len().saturating_sub(1);
                self.out.push_str(&"    ".repeat(depth));
                match self.lists.last_mut() {
                    Some(Some(number)) => {
                        self.out.push_str(&format!("{number}. "));
                        *number += 1;
                    }
                    _ => self.out.push_str("• "),
                }
            }
            Tag::Table(_) => {
                self.begin_block();
                self.table = Some(Table::default());
            }
            Tag::Emphasis => self.out.push_str("<i>"),
            Tag::Strong => self.out.push_str("<b>"),
            Tag::Strikethrough => self.out.push_str("<s>"),
            Tag::Link { dest_url, .. } => {
                self.link_depth += 1;
                self.out
                    .push_str(&format!("<a href=\"{}\">", escape_attribute(&dest_url)));
            }
            Tag::Image { dest_url, .. } => {
                // A phone cannot show the image here; its link, labelled by the alt
                // text, is the next best thing.
                self.link_depth += 1;
                self.out
                    .push_str(&format!("<a href=\"{}\">", escape_attribute(&dest_url)));
                self.image_start = Some(self.out.len());
            }
            Tag::TableHead
            | Tag::TableRow
            | Tag::TableCell
            | Tag::FootnoteDefinition(_)
            | Tag::DefinitionList
            | Tag::DefinitionListTitle
            | Tag::DefinitionListDefinition
            | Tag::Superscript
            | Tag::Subscript
            | Tag::MetadataBlock(_) => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph | TagEnd::HtmlBlock => {
                self.trim_trailing_newlines();
                self.end_block();
            }
            TagEnd::Heading(_) => {
                self.out.push_str("</b>");
                self.end_block();
            }
            TagEnd::BlockQuote(_) => {
                self.quote_depth = self.quote_depth.saturating_sub(1);
                if self.quote_depth == 0 {
                    self.trim_trailing_newlines();
                    self.out.push_str("</blockquote>");
                }
                self.end_block();
            }
            TagEnd::CodeBlock => {
                self.code_depth = self.code_depth.saturating_sub(1);
                self.trim_trailing_newlines();
                self.out.push_str("</code></pre>");
                self.end_block();
            }
            TagEnd::List(_) => {
                self.lists.pop();
                self.end_block();
            }
            TagEnd::Item => {
                self.trim_trailing_newlines();
                self.end_block();
            }
            TagEnd::Emphasis => self.out.push_str("</i>"),
            TagEnd::Strong => self.out.push_str("</b>"),
            TagEnd::Strikethrough => self.out.push_str("</s>"),
            TagEnd::Link => {
                self.link_depth = self.link_depth.saturating_sub(1);
                self.out.push_str("</a>");
            }
            TagEnd::Image => {
                self.link_depth = self.link_depth.saturating_sub(1);
                if let Some(start) = self.image_start.take() {
                    if self.out.len() == start {
                        let href = self.out[..start]
                            .rsplit("href=\"")
                            .next()
                            .and_then(|tail| tail.strip_suffix("\">"))
                            .unwrap_or("image")
                            .to_string();
                        self.out.push_str(&href);
                    }
                }
                self.out.push_str("</a>");
            }
            TagEnd::Table
            | TagEnd::TableHead
            | TagEnd::TableRow
            | TagEnd::TableCell
            | TagEnd::FootnoteDefinition
            | TagEnd::DefinitionList
            | TagEnd::DefinitionListTitle
            | TagEnd::DefinitionListDefinition
            | TagEnd::Superscript
            | TagEnd::Subscript
            | TagEnd::MetadataBlock(_) => {}
        }
    }

    /// Inside a table only the words matter: they are laid out in a monospace block,
    /// where Telegram allows no other markup.
    fn table_event(&mut self, event: Event<'_>) {
        let Some(table) = self.table.as_mut() else {
            return;
        };
        match event {
            Event::Start(Tag::TableCell) => {
                table.in_cell = true;
                table.cell.clear();
            }
            Event::End(TagEnd::TableCell) => {
                table.in_cell = false;
                let cell = std::mem::take(&mut table.cell);
                table.current.push(cell.trim().to_string());
            }
            Event::End(TagEnd::TableHead) | Event::End(TagEnd::TableRow) => {
                let row = std::mem::take(&mut table.current);
                table.rows.push(row);
            }
            Event::End(TagEnd::Table) => {
                let table = self.table.take().unwrap_or_default();
                self.out.push_str("<pre>");
                self.out.push_str(&escape_html(&layout(&table.rows)));
                self.out.push_str("</pre>");
                self.end_block();
            }
            Event::Text(text) | Event::Code(text) | Event::Html(text) | Event::InlineHtml(text)
                if table.in_cell =>
            {
                table.cell.push_str(&text);
            }
            Event::SoftBreak | Event::HardBreak if table.in_cell => table.cell.push(' '),
            _ => {}
        }
    }

    fn begin_block(&mut self) {
        if self.separator_pending {
            // Items of one list sit on consecutive lines; blocks outside a list are a
            // paragraph apart.
            self.out
                .push_str(if self.lists.is_empty() { "\n\n" } else { "\n" });
            self.separator_pending = false;
        }
    }

    fn end_block(&mut self) {
        self.separator_pending = true;
    }

    fn trim_trailing_newlines(&mut self) {
        let trimmed = self.out.trim_end_matches('\n').len();
        self.out.truncate(trimmed);
    }

    fn finish(mut self) -> String {
        self.trim_trailing_newlines();
        self.out.trim().to_string()
    }
}

/// The first word of a fence's info string, if it looks like a language name.
fn language_of(info: &str) -> Option<String> {
    let mut words = info.split_whitespace();
    let word = words.next()?;
    if words.next().is_some()
        || word.len() > LANGUAGE_LIMIT
        || !word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '+' | '#' | '.'))
    {
        return None;
    }
    Some(word.to_string())
}

/// An attribute value: the three HTML characters plus the quote that closes it.
fn escape_attribute(value: &str) -> String {
    escape_html(value).replace('"', "&quot;")
}

/// Lay rows out as columns of equal width, the first row as the header.
///
/// Monospace on a phone is about forty characters wide, so columns are separated by
/// two spaces rather than drawn with bars, and nothing is wrapped: a wide table scrolls
/// sideways inside Telegram's code block, which beats tangled lines.
fn layout(rows: &[Vec<String>]) -> String {
    let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
    let widths: Vec<usize> = (0..columns)
        .map(|column| {
            rows.iter()
                .filter_map(|row| row.get(column))
                .map(|cell| cell.width())
                .max()
                .unwrap_or(0)
        })
        .collect();
    let mut lines = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let mut line = String::new();
        for (column, width) in widths.iter().enumerate() {
            let cell = row.get(column).map(String::as_str).unwrap_or("");
            line.push_str(cell);
            if column + 1 < widths.len() {
                line.push_str(&" ".repeat(width.saturating_sub(cell.width()) + 2));
            }
        }
        lines.push(line.trim_end().to_string());
        if index == 0 && rows.len() > 1 {
            lines.push(
                widths
                    .iter()
                    .map(|width| "─".repeat(*width))
                    .collect::<Vec<_>>()
                    .join("  "),
            );
        }
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prose_without_markup_converts_to_exactly_its_escaped_self() {
        // The property the rest of the channel relies on: an agent's plain text is
        // neither reflowed nor reworded, and splits still fall on its own line breaks.
        let prose = "Finished the refactor & the tests pass.\nTwo files changed: a < b.\n\n\
                     Shall I open the PR? It touches src/main.rs and vendor/edtui.";
        assert_eq!(to_telegram_html(prose), escape_html(prose));
    }

    #[test]
    fn inline_markup_becomes_telegram_tags_and_raw_html_stays_text() {
        assert_eq!(
            to_telegram_html("**bold** and *italic* and ~~gone~~ and `code <x>`"),
            "<b>bold</b> and <i>italic</i> and <s>gone</s> and <code>code &lt;x&gt;</code>"
        );
        // A tag Telegram does not know would make it reject the whole message.
        assert_eq!(
            to_telegram_html("line one<br>line two and <details>more</details>"),
            "line one&lt;br&gt;line two and &lt;details&gt;more&lt;/details&gt;"
        );
        assert_eq!(
            to_telegram_html("<div>\nblock html\n</div>"),
            "&lt;div&gt;\nblock html\n&lt;/div&gt;"
        );
    }

    #[test]
    fn headings_are_bold_lines_and_blocks_sit_a_paragraph_apart() {
        assert_eq!(
            to_telegram_html("## Summary\n\nAll good.\n\n### Next\n\nShip it."),
            "<b>Summary</b>\n\nAll good.\n\n<b>Next</b>\n\nShip it."
        );
    }

    #[test]
    fn lists_become_indented_bullets_with_numbers_kept() {
        let markdown = "Steps:\n\n1. First\n2. Second\n   - nested a\n   - nested b\n3. Third\n\n\
                        - [x] done\n- [ ] todo\n\nAfter.";
        assert_eq!(
            to_telegram_html(markdown),
            "Steps:\n\n1. First\n2. Second\n    • nested a\n    • nested b\n3. Third\n\n\
             ☑ done\n☐ todo\n\nAfter."
        );
        // A list that starts at a later number keeps counting from there.
        assert_eq!(to_telegram_html("4. four\n5. five"), "4. four\n5. five");
    }

    #[test]
    fn fenced_code_keeps_its_language_and_an_indented_block_has_none() {
        assert_eq!(
            to_telegram_html("Run:\n\n```rust\nfn main() {\n    let a = 1 < 2;\n}\n```\n\nDone."),
            "Run:\n\n<pre><code class=\"language-rust\">fn main() {\n    let a = 1 &lt; 2;\n}\
             </code></pre>\n\nDone."
        );
        assert_eq!(
            to_telegram_html("```\nplain\n```"),
            "<pre><code>plain</code></pre>"
        );
        // An info string that is not a language name is not put in a class.
        assert_eq!(
            to_telegram_html("```this is not a language\nx\n```"),
            "<pre><code>x</code></pre>"
        );
    }

    #[test]
    fn a_table_is_laid_out_in_monospace_columns() {
        let markdown = "| File | Change |\n|---|---|\n| main.rs | **edited** |\n| lib.rs | new |";
        assert_eq!(
            to_telegram_html(markdown),
            "<pre>File     Change\n───────  ──────\nmain.rs  edited\nlib.rs   new</pre>"
        );
    }

    #[test]
    fn links_and_images_point_at_their_address_with_the_quote_escaped() {
        assert_eq!(
            to_telegram_html("See [the PR](https://example.com/pr?a=1&b=\"2\")."),
            "See <a href=\"https://example.com/pr?a=1&amp;b=&quot;2&quot;\">the PR</a>."
        );
        assert_eq!(
            to_telegram_html("![diagram](https://e.com/d.png) and ![](https://e.com/e.png)"),
            "<a href=\"https://e.com/d.png\">diagram</a> and \
             <a href=\"https://e.com/e.png\">https://e.com/e.png</a>"
        );
    }

    #[test]
    fn blockquotes_do_not_nest_and_a_rule_is_a_line() {
        assert_eq!(
            to_telegram_html("> outer\n>\n> > inner\n\n---\n\nend"),
            "<blockquote>outer\n\ninner</blockquote>\n\n———\n\nend"
        );
    }

    #[test]
    fn references_in_prose_link_to_the_sessions_repository_but_not_in_code_or_inside_a_link() {
        let repository = RepositoryRef {
            host: "github.com".to_string(),
            owner: "o".to_string(),
            name: "r".to_string(),
        };
        let html = to_telegram_html_in(
            "Fixed #12 (see [#13](https://e.com/x) and `#14`), colour #fff and sha#15.\n\n\
             ```\n#16 in code\n```",
            Some(&repository),
        );
        assert_eq!(
            html,
            "Fixed <a href=\"https://github.com/o/r/issues/12\">#12</a> (see \
             <a href=\"https://e.com/x\">#13</a> and <code>#14</code>), colour #fff and \
             sha#15.\n\n<pre><code>#16 in code</code></pre>"
        );
        // Without a repository nothing changes, so sessions outside GitHub read as before.
        assert_eq!(to_telegram_html("Fixed #12."), "Fixed #12.");
    }

    #[test]
    fn stripping_tags_leaves_the_text_and_its_entities() {
        let html = to_telegram_html("## Hi\n\n- a `<b>` & more\n- [x](https://e.com)");
        assert_eq!(strip_tags(&html), "Hi\n\n• a &lt;b&gt; &amp; more\n• x");
    }
}
