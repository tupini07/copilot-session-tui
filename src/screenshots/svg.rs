//! Turns a rendered ratatui cell buffer into a standalone SVG.
//!
//! SVG rather than PNG because it needs no rasterizer, no bundled font, and no
//! extra dependency: the generator stays pure Rust, the output stays diffable in
//! review, and GitHub renders it in the README at any zoom level.

use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier};
use std::fmt::Write as _;

/// Cell metrics. The advance width is the 0.6em that monospace faces use, so runs
/// land on the grid even before `textLength` corrects for the viewer's own font.
const FONT_SIZE: f32 = 15.0;
const CELL_W: f32 = 9.0;
/// Box-drawing glyphs span roughly 1.2em, so a taller cell would leave visible gaps
/// in every pane border.
const CELL_H: f32 = 18.0;
/// Baseline offset inside the cell, chosen so descenders clear the next row.
const BASELINE: f32 = 13.5;
const PADDING: f32 = 16.0;
const CORNER: f32 = 8.0;

const FONT_STACK: &str =
    "ui-monospace,SFMono-Regular,Menlo,Consolas,'DejaVu Sans Mono','Liberation Mono',monospace";

/// Catppuccin Mocha. Picked over a stock ANSI palette because CST leans on blue for
/// directories and dark gray for hints, both of which are hard to read in the
/// default Windows Terminal scheme.
const BACKGROUND: &str = "#11111b";
const FOREGROUND: &str = "#cdd6f4";

const PALETTE: [&str; 16] = [
    "#45475a", // black
    "#f38ba8", // red
    "#a6e3a1", // green
    "#f9e2af", // yellow
    "#89b4fa", // blue
    "#cba6f7", // magenta
    "#94e2d5", // cyan
    "#bac2de", // white
    "#585b70", // bright black
    "#f38ba8", // bright red
    "#a6e3a1", // bright green
    "#f9e2af", // bright yellow
    "#89b4fa", // bright blue
    "#cba6f7", // bright magenta
    "#94e2d5", // bright cyan
    "#cdd6f4", // bright white
];

/// A run of adjacent cells that share every visual attribute.
struct Run {
    x: u16,
    y: u16,
    width: u16,
    text: String,
    fg: String,
    bold: bool,
    dim: bool,
    italic: bool,
    underlined: bool,
}

pub fn render(buffer: &Buffer) -> String {
    let area = buffer.area;
    let width = PADDING * 2.0 + area.width as f32 * CELL_W;
    let height = PADDING * 2.0 + area.height as f32 * CELL_H;

    let mut svg = String::new();
    let _ = write!(
        svg,
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{width:.0}" height="{height:.0}" viewBox="0 0 {width:.0} {height:.0}" font-family="{FONT_STACK}" font-size="{FONT_SIZE}">
<rect width="{width:.0}" height="{height:.0}" rx="{CORNER}" fill="{BACKGROUND}"/>
"#
    );

    write_backgrounds(&mut svg, buffer);
    write_text(&mut svg, buffer);

    svg.push_str("</svg>\n");
    svg
}

/// Emit one rect per horizontal run of identical background, skipping the default
/// so the canvas fill shows through.
fn write_backgrounds(svg: &mut String, buffer: &Buffer) {
    let area = buffer.area;
    for y in 0..area.height {
        let mut x = 0;
        while x < area.width {
            let fill = resolved(buffer, x, y).1;
            if fill == BACKGROUND {
                x += 1;
                continue;
            }
            let start = x;
            while x < area.width && resolved(buffer, x, y).1 == fill {
                x += 1;
            }
            let _ = writeln!(
                svg,
                r#"<rect x="{:.1}" y="{:.1}" width="{:.1}" height="{:.1}" fill="{fill}" shape-rendering="crispEdges"/>"#,
                PADDING + start as f32 * CELL_W,
                PADDING + y as f32 * CELL_H,
                (x - start) as f32 * CELL_W,
                CELL_H,
            );
        }
    }
}

fn write_text(svg: &mut String, buffer: &Buffer) {
    for run in collect_runs(buffer) {
        if run.text.trim().is_empty() {
            continue;
        }
        let mut attrs = String::new();
        if run.bold {
            attrs.push_str(r#" font-weight="bold""#);
        }
        if run.italic {
            attrs.push_str(r#" font-style="italic""#);
        }
        if run.underlined {
            attrs.push_str(r#" text-decoration="underline""#);
        }
        if run.dim {
            attrs.push_str(r#" opacity="0.65""#);
        }
        // Runs of rule characters are stretched glyph-and-all, because padding the
        // gaps instead would break a border into dashes. Everything else only has
        // its spacing nudged, which leaves letterforms untouched.
        let adjust = if run.text.chars().all(is_rule) {
            "spacingAndGlyphs"
        } else {
            "spacing"
        };
        // `textLength` pins the run to the grid, so a viewer without the preferred
        // font cannot drift out of alignment.
        let _ = writeln!(
            svg,
            r#"<text x="{:.1}" y="{:.1}" fill="{}" textLength="{:.1}" lengthAdjust="{adjust}"{attrs} xml:space="preserve">{}</text>"#,
            PADDING + run.x as f32 * CELL_W,
            PADDING + run.y as f32 * CELL_H + BASELINE,
            run.fg,
            run.width as f32 * CELL_W,
            escape(&run.text),
        );
    }
}

fn collect_runs(buffer: &Buffer) -> Vec<Run> {
    let area = buffer.area;
    let mut runs = Vec::new();
    for y in 0..area.height {
        let mut current: Option<Run> = None;
        for x in 0..area.width {
            let cell = &buffer[(x, y)];
            let symbol = cell.symbol();
            // The trailing half of a wide glyph carries no symbol; it must not extend
            // the run, or the following text would be pushed off the grid.
            if symbol.is_empty() {
                if let Some(run) = current.take() {
                    runs.push(run);
                }
                continue;
            }
            let (fg, _) = resolved(buffer, x, y);
            let modifier = cell.modifier;
            let matches = current.as_ref().is_some_and(|run| {
                run.fg == fg
                    && run.bold == modifier.contains(Modifier::BOLD)
                    && run.dim == modifier.contains(Modifier::DIM)
                    && run.italic == modifier.contains(Modifier::ITALIC)
                    && run.underlined == modifier.contains(Modifier::UNDERLINED)
                    && run.x + run.width == x
            });

            if matches {
                let run = current.as_mut().expect("checked above");
                run.text.push_str(symbol);
                run.width += 1;
                continue;
            }
            if let Some(run) = current.take() {
                runs.push(run);
            }
            current = Some(Run {
                x,
                y,
                width: 1,
                text: symbol.to_string(),
                fg,
                bold: modifier.contains(Modifier::BOLD),
                dim: modifier.contains(Modifier::DIM),
                italic: modifier.contains(Modifier::ITALIC),
                underlined: modifier.contains(Modifier::UNDERLINED),
            });
        }
        if let Some(run) = current.take() {
            runs.push(run);
        }
    }
    runs
}

/// Foreground and background for a cell, with `REVERSED` already applied.
fn resolved(buffer: &Buffer, x: u16, y: u16) -> (String, String) {
    let cell = &buffer[(x, y)];
    let mut fg = hex(cell.fg, FOREGROUND);
    let mut bg = hex(cell.bg, BACKGROUND);
    if cell.modifier.contains(Modifier::REVERSED) {
        std::mem::swap(&mut fg, &mut bg);
    }
    (fg, bg)
}

fn hex(color: Color, default: &str) -> String {
    let index = match color {
        Color::Reset => return default.to_string(),
        Color::Rgb(r, g, b) => return format!("#{r:02x}{g:02x}{b:02x}"),
        Color::Indexed(n) => return indexed(n),
        Color::Black => 0,
        Color::Red => 1,
        Color::Green => 2,
        Color::Yellow => 3,
        Color::Blue => 4,
        Color::Magenta => 5,
        Color::Cyan => 6,
        Color::Gray => 7,
        Color::DarkGray => 8,
        Color::LightRed => 9,
        Color::LightGreen => 10,
        Color::LightYellow => 11,
        Color::LightBlue => 12,
        Color::LightMagenta => 13,
        Color::LightCyan => 14,
        Color::White => 15,
    };
    PALETTE[index].to_string()
}

/// The xterm-256 layout: 16 themed colors, a 6x6x6 cube, then a gray ramp.
fn indexed(n: u8) -> String {
    match n {
        0..=15 => PALETTE[n as usize].to_string(),
        16..=231 => {
            let n = n - 16;
            let level = |v: u8| if v == 0 { 0u8 } else { 55 + v * 40 };
            format!(
                "#{:02x}{:02x}{:02x}",
                level(n / 36),
                level((n % 36) / 6),
                level(n % 6)
            )
        }
        _ => {
            let value = 8 + (n - 232) * 10;
            format!("#{value:02x}{value:02x}{value:02x}")
        }
    }
}

/// Box drawing and block elements, which have to tile without seams.
fn is_rule(ch: char) -> bool {
    matches!(ch, '\u{2500}'..='\u{259f}')
}

fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(ch),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::layout::Rect;
    use ratatui::style::Style;

    fn buffer_with(lines: &[&str]) -> Buffer {
        let width = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0) as u16;
        let mut buffer = Buffer::empty(Rect::new(0, 0, width, lines.len() as u16));
        for (y, line) in lines.iter().enumerate() {
            buffer.set_string(0, y as u16, line, Style::default());
        }
        buffer
    }

    #[test]
    fn markup_special_characters_survive_as_entities() {
        let svg = render(&buffer_with(&["<a> & \"b\""]));

        assert!(svg.contains("&lt;a&gt; &amp; &quot;b&quot;"), "{svg}");
        assert!(!svg.contains(">a<"), "{svg}");
    }

    #[test]
    fn adjacent_cells_of_one_style_become_a_single_run() {
        let svg = render(&buffer_with(&["hello"]));

        assert_eq!(svg.matches("<text").count(), 1, "{svg}");
        assert!(svg.contains(">hello</text>"), "{svg}");
    }

    #[test]
    fn a_style_change_starts_a_new_run() {
        let mut buffer = buffer_with(&["ab"]);
        buffer[(1, 0)].fg = Color::Red;

        let svg = render(&buffer);

        assert_eq!(svg.matches("<text").count(), 2, "{svg}");
        assert!(svg.contains(PALETTE[1]), "{svg}");
    }

    #[test]
    fn reversed_cells_swap_foreground_and_background() {
        let mut buffer = buffer_with(&["x"]);
        buffer[(0, 0)].fg = Color::Green;
        buffer[(0, 0)].modifier = Modifier::REVERSED;

        let svg = render(&buffer);

        // Green becomes the fill of the rect, and the glyph takes the canvas color.
        assert!(
            svg.contains(&format!(r#"fill="{}" shape-rendering"#, PALETTE[2])),
            "{svg}"
        );
        assert!(
            svg.contains(&format!(r#"fill="{BACKGROUND}" textLength"#)),
            "{svg}"
        );
    }

    #[test]
    fn rule_runs_stretch_their_glyphs_so_borders_stay_unbroken() {
        let buffer = buffer_with(&["──", "ab"]);

        let svg = render(&buffer);

        // Padding the gaps between box-drawing glyphs would turn a border into a
        // dashed line, so those runs scale the glyphs instead.
        assert!(svg.contains(r#"lengthAdjust="spacingAndGlyphs""#), "{svg}");
        assert!(svg.contains(r#"lengthAdjust="spacing""#), "{svg}");
    }

    #[test]
    fn the_default_background_is_left_to_the_canvas() {
        let svg = render(&buffer_with(&["ab"]));

        // Only the rounded canvas rect, no per-cell fills.
        assert_eq!(svg.matches("<rect").count(), 1, "{svg}");
    }

    #[test]
    fn indexed_colors_follow_the_xterm_cube() {
        assert_eq!(indexed(16), "#000000");
        assert_eq!(indexed(231), "#ffffff");
        assert_eq!(indexed(232), "#080808");
        assert_eq!(indexed(1), PALETTE[1]);
    }

    #[test]
    fn the_canvas_is_sized_from_the_grid() {
        let svg = render(&buffer_with(&["abc", "def"]));

        let width = PADDING * 2.0 + 3.0 * CELL_W;
        let height = PADDING * 2.0 + 2.0 * CELL_H;
        assert!(svg.contains(&format!(r#"width="{width:.0}""#)), "{svg}");
        assert!(svg.contains(&format!(r#"height="{height:.0}""#)), "{svg}");
    }
}
