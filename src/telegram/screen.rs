//! A session's screen, as the phone can show it: the rows of the terminal, verbatim.
//!
//! The first live view was built from the event log, as prose: the session's last
//! words and the tool it was running. That reads well on a phone and any CST can
//! produce it, but it is a summary, and the person asking for a follow is usually
//! trying to see what the terminal actually shows — a menu that opened, a prompt
//! waiting, the exact line Copilot is on. So the view is now the screen itself, in a
//! monospace block, right-trimmed and cut from the top to fit one message. Only the
//! CST hosting the pane can read a screen, so this runs on its UI thread; the log view
//! stays as what the poller falls back to when no CST hosts the session.
//!
//! Everything here is pure: a `vt100::Screen` in, rows and HTML out.

use super::escape_html;

/// One row per line of the screen, each cut at its last non-blank cell, with the
/// trailing blank rows dropped. Wide characters take two cells but appear once.
pub fn rows(screen: &vt100::Screen) -> Vec<String> {
    let (height, width) = screen.size();
    let mut rows: Vec<String> = (0..height)
        .map(|row| {
            let mut text = String::new();
            for column in 0..width {
                let Some(cell) = screen.cell(row, column) else {
                    continue;
                };
                if cell.is_wide_continuation() {
                    continue;
                }
                match cell.contents() {
                    "" => text.push(' '),
                    contents => text.push_str(contents),
                }
            }
            text.trim_end().to_string()
        })
        .collect();
    while rows.last().is_some_and(|row| row.is_empty()) {
        rows.pop();
    }
    rows
}

/// The rows as one `<pre>` block of at most `budget` characters, keeping the bottom.
///
/// The bottom is where the prompt and the latest output are; what scrolls off the top
/// of the budget is what the user could least want. A cut is marked so it is not read
/// as the top of the screen.
pub fn as_pre(rows: &[String], budget: usize) -> String {
    const OPEN: &str = "<pre>";
    const CLOSE: &str = "</pre>";
    const CUT: &str = "…\n";
    let escaped: Vec<String> = rows.iter().map(|row| escape_html(row)).collect();
    let fixed = OPEN.len() + CLOSE.len();
    let mut used = 0usize;
    let mut first = escaped.len();
    for (index, row) in escaped.iter().enumerate().rev() {
        let cost = row.chars().count() + 1;
        let cut_mark = if index > 0 { CUT.chars().count() } else { 0 };
        if used + cost + cut_mark + fixed > budget {
            break;
        }
        used += cost;
        first = index;
    }
    let mut text = String::from(OPEN);
    if first > 0 {
        text.push_str(CUT);
    }
    let shown = &escaped[first..];
    text.push_str(&shown.join("\n"));
    text.push_str(CLOSE);
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(text: &str, columns: u16) -> vt100::Parser {
        let mut parser = vt100::Parser::new(6, columns, 0);
        parser.process(text.as_bytes());
        parser
    }

    #[test]
    fn rows_are_right_trimmed_wide_characters_appear_once_and_blank_tail_rows_go() {
        let parser = screen("> hello   \r\n界 ok\r\n\r\n", 20);
        assert_eq!(rows(parser.screen()), vec!["> hello", "界 ok"]);
    }

    #[test]
    fn the_block_keeps_the_bottom_rows_marks_the_cut_and_escapes_markup() {
        let rows: Vec<String> = ["<top>", "middle & more", "bottom"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            as_pre(&rows, 1000),
            "<pre>&lt;top&gt;\nmiddle &amp; more\nbottom</pre>"
        );
        let cut = as_pre(&rows, 40);
        assert!(cut.starts_with("<pre>…\n"), "{cut}");
        assert!(cut.ends_with("bottom</pre>"));
        assert!(!cut.contains("top"));
        assert!(cut.chars().count() <= 40);
    }
}
