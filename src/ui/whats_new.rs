//! The "what changed since you were last here" screen.
//!
//! Shown once after CST updates itself, and on demand from the command palette. The
//! content comes from the `CHANGELOG.md` compiled into the binary, so this never waits
//! on the network and works offline.
//!
//! Lines are wrapped here rather than by `Paragraph::wrap`, because the scroll maths
//! below counts rendered lines. `draw_help` gets away with `Paragraph::scroll` only
//! because its lines never wrap; changelog bullets will.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use crate::app::App;
use crate::changelog::WhatsNew;
use crate::text;
use crate::theme::Theme;

/// Everything the screen needs, plus where the user has scrolled to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WhatsNewScreen {
    pub content: WhatsNew,
    pub scroll: usize,
    /// Rendered line count and viewport height from the last draw, so scrolling can be
    /// clamped without re-deriving the layout.
    pub max_scroll: usize,
}

impl WhatsNewScreen {
    pub fn new(content: WhatsNew) -> Self {
        Self {
            content,
            scroll: 0,
            max_scroll: 0,
        }
    }

    pub fn scroll_by(&mut self, amount: isize) {
        self.scroll = if amount < 0 {
            self.scroll.saturating_sub(amount.unsigned_abs())
        } else {
            self.scroll.saturating_add(amount as usize)
        }
        .min(self.max_scroll);
    }

    pub fn scroll_to_end(&mut self) {
        self.scroll = self.max_scroll;
    }
}

pub fn draw(f: &mut Frame, app: &mut App) {
    let theme = app.theme();
    let Some(screen) = app.whats_new.as_ref() else {
        return;
    };

    let area = super::popups::centered_rect(62, 70, f.area());
    super::popups::prepare_popup(f, area, theme);

    let block = Block::default()
        .title(" What's new ")
        .borders(Borders::ALL)
        .style(Style::default().fg(theme.text).bg(theme.surface))
        .border_style(Style::default().fg(theme.accent_alt));
    let inner = block.inner(area);
    f.render_widget(block, area);

    // The last column of the body carries the scrollbar, so the text stops short of it.
    let lines = build_lines(
        &screen.content,
        inner.width.saturating_sub(2).max(1) as usize,
        theme,
    );

    // Last row of the inner area is the footer; the rest is the scrolling body.
    let body = Rect {
        height: inner.height.saturating_sub(1),
        ..inner
    };
    let viewport = body.height as usize;
    let max_scroll = lines.len().saturating_sub(viewport);
    let scroll = screen.scroll.min(max_scroll);

    if let Some(screen) = app.whats_new.as_mut() {
        screen.max_scroll = max_scroll;
        screen.scroll = scroll;
    }

    f.render_widget(
        Paragraph::new(lines.clone())
            .style(Style::default().fg(theme.text).bg(theme.surface))
            .scroll((scroll.min(u16::MAX as usize) as u16, 0)),
        body,
    );
    super::draw_scrollbar(f, body, lines.len(), viewport, scroll, theme);

    let footer = Rect {
        y: inner.bottom().saturating_sub(1),
        height: 1,
        ..inner
    };
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" ↑/↓ PageUp/PageDown", Style::default().fg(theme.accent)),
            Span::raw(" scroll  "),
            Span::styled("Esc", Style::default().fg(theme.accent)),
            Span::raw(" close"),
        ]))
        .style(Style::default().fg(theme.muted).bg(theme.surface)),
        footer,
    );
}

fn build_lines(content: &WhatsNew, width: usize, theme: Theme) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(Span::styled(
        format!("  What's new in CST v{}", content.current_version),
        Style::default()
            .fg(theme.accent_alt)
            .add_modifier(Modifier::BOLD),
    ))];

    let subtitle = if content.from_version.is_empty() {
        format!(
            "  {} recent release{}",
            content.releases.len(),
            if content.releases.len() == 1 { "" } else { "s" }
        )
    } else {
        format!(
            "  You were on v{} · {} release{}",
            content.from_version,
            content.releases.len(),
            if content.releases.len() == 1 { "" } else { "s" }
        )
    };
    lines.push(Line::from(Span::styled(
        subtitle,
        Style::default().fg(theme.muted),
    )));

    for release in &content.releases {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!("  v{} — {}", release.version, release.date),
            Style::default()
                .fg(theme.warning)
                .add_modifier(Modifier::BOLD),
        )));
        for bullet in &release.bullets {
            // Wrapped here so the scroll arithmetic above counts real rendered lines.
            for (index, piece) in text::wrap_text(bullet, width.saturating_sub(4))
                .into_iter()
                .enumerate()
            {
                let prefix = if index == 0 { "  • " } else { "    " };
                lines.push(Line::from(Span::styled(
                    format!("{prefix}{piece}"),
                    Style::default().fg(theme.text),
                )));
            }
        }
    }

    if content.skipped > 0 {
        lines.push(Line::from(""));
        // Named, not just counted: a silent truncation would leave the user unsure
        // whether something was dropped.
        let tail = match &content.oldest_skipped {
            Some(oldest) => format!(
                "  … and {} earlier release{}, back to v{oldest}.",
                content.skipped,
                if content.skipped == 1 { "" } else { "s" }
            ),
            None => format!("  … and {} earlier releases.", content.skipped),
        };
        lines.push(Line::from(Span::styled(
            tail,
            Style::default().fg(theme.muted),
        )));
    }

    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::changelog::Release;
    use crate::theme::ThemeName;

    fn content(skipped: usize) -> WhatsNew {
        WhatsNew {
            from_version: "0.26.0".to_string(),
            current_version: "0.29.0".to_string(),
            releases: vec![Release {
                version: "0.29.0".to_string(),
                date: "2026-09-14".to_string(),
                bullets: vec!["Drag a session tab to move it.".to_string()],
            }],
            skipped,
            oldest_skipped: (skipped > 0).then(|| "0.18.0".to_string()),
        }
    }

    fn rendered(content: &WhatsNew, width: usize) -> String {
        build_lines(content, width, ThemeName::Classic.theme())
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn the_screen_says_which_version_you_came_from_so_the_span_is_not_a_mystery() {
        let text = rendered(&content(0), 60);
        assert!(text.contains("What's new in CST v0.29.0"), "got:\n{text}");
        assert!(text.contains("You were on v0.26.0"), "got:\n{text}");
        assert!(text.contains("Drag a session tab"), "got:\n{text}");
    }

    #[test]
    fn a_truncated_span_names_the_oldest_release_it_dropped() {
        let text = rendered(&content(7), 60);
        assert!(
            text.contains("… and 7 earlier releases, back to v0.18.0."),
            "a silent truncation leaves the user guessing, got:\n{text}"
        );
    }

    #[test]
    fn nothing_is_dropped_when_the_whole_span_fits() {
        let text = rendered(&content(0), 60);
        assert!(!text.contains("earlier release"), "got:\n{text}");
    }

    #[test]
    fn a_long_bullet_is_wrapped_rather_than_cut_off() {
        let mut long = content(0);
        long.releases[0].bullets = vec![
            "A bullet long enough that it cannot possibly fit inside a narrow popup \
             without being wrapped onto a second line."
                .to_string(),
        ];
        let text = rendered(&long, 30);
        let body: Vec<&str> = text
            .lines()
            .filter(|line| line.starts_with("  • ") || line.starts_with("    "))
            .collect();
        assert!(body.len() > 1, "expected a wrap, got:\n{text}");
        for line in &body {
            assert!(line.chars().count() <= 30, "line too wide: {line:?}");
        }
        // The continuation is indented under the bullet, not re-bulleted.
        assert!(body[1].starts_with("    "), "got: {:?}", body[1]);
    }

    #[test]
    fn scrolling_is_clamped_to_what_was_actually_rendered() {
        let mut screen = WhatsNewScreen::new(content(0));
        screen.max_scroll = 3;

        screen.scroll_by(100);
        assert_eq!(screen.scroll, 3, "cannot scroll past the end");
        screen.scroll_by(-100);
        assert_eq!(screen.scroll, 0, "cannot scroll above the start");
        screen.scroll_to_end();
        assert_eq!(screen.scroll, 3);
    }
}
