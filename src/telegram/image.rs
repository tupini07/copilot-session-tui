//! A terminal screen, or a block of monospace text, drawn as a PNG.
//!
//! A phone shows a `<pre>` block wrapped at forty-odd columns, which turns a hundred
//! columns of terminal into tangled lines. A picture keeps the columns where they are,
//! colours and all, and Telegram shows it inline; the user zooms. So `/peek` sends one,
//! and a notification's tables, which have no Telegram markup at all, go the same way.
//!
//! Drawn here rather than by a browser or the OS: CST may be running over SSH on a
//! machine with neither. The font is embedded — Hack, a DejaVu Sans Mono descendant
//! with the box-drawing range Copilot's frames use, and Noto Emoji for the emoji in
//! session names — through the `epaint_default_fonts` crate, which exists to ship exactly
//! those bytes. Bold is drawn twice a pixel apart, as terminals did before bold faces.
//!
//! Pure: cells in, bytes out. Nothing here touches a pane.

use ab_glyph::{point, Font, FontRef, GlyphId, PxScale, ScaleFont};

/// The font size in pixels. Telegram scales a photo down to fit 1280 pixels on its
/// long side, so a 110-column screen at this size arrives near one-to-one.
const FONT_PX: f32 = 20.0;
/// Tables are a few lines, and Telegram shows a small picture small: drawn larger so
/// the figures can be read without zooming.
pub const TABLE_FONT_PX: f32 = 34.0;
/// Pixels around the text, so the frame does not touch the edge.
const MARGIN: u32 = 10;
/// The most cells drawn per side, a guard against a pathological screen size.
const MAX_CELLS: usize = 400;

/// One cell of the picture: what it shows and in which colours.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cell {
    /// The character, or `None` for the second half of a wide one.
    pub text: Option<String>,
    pub fg: Rgb,
    pub bg: Rgb,
    pub bold: bool,
}

pub type Rgb = (u8, u8, u8);

/// The Windows Terminal "Campbell" scheme: the dark background Copilot is usually
/// seen on, so a picture looks like the screen it came from.
pub const DARK_BG: Rgb = (12, 12, 12);
pub const DARK_FG: Rgb = (204, 204, 204);
/// Tables in a notification: dark on light, like the text around them in the chat.
pub const LIGHT_BG: Rgb = (255, 255, 255);
pub const LIGHT_FG: Rgb = (24, 24, 24);

/// The cells of a `vt100` screen, trailing blank rows dropped.
pub fn cells(screen: &vt100::Screen) -> Vec<Vec<Cell>> {
    let (height, width) = screen.size();
    let mut rows: Vec<Vec<Cell>> = (0..height)
        .map(|row| {
            (0..width)
                .map(|column| {
                    let Some(cell) = screen.cell(row, column) else {
                        return blank();
                    };
                    let mut fg = color(cell.fgcolor(), DARK_FG);
                    let mut bg = color(cell.bgcolor(), DARK_BG);
                    if cell.inverse() {
                        std::mem::swap(&mut fg, &mut bg);
                    }
                    let text = if cell.is_wide_continuation() {
                        None
                    } else {
                        Some(match cell.contents() {
                            "" => " ".to_string(),
                            contents => contents.to_string(),
                        })
                    };
                    Cell {
                        text,
                        fg,
                        bg,
                        bold: cell.bold(),
                    }
                })
                .collect()
        })
        .collect();
    while rows.last().is_some_and(|row| {
        row.iter().all(|cell| {
            cell.text.as_deref().is_none_or(|t| t.trim().is_empty()) && cell.bg == DARK_BG
        })
    }) {
        rows.pop();
    }
    rows
}

/// Plain lines as cells, for a table or any other monospace block.
pub fn cells_of_text(lines: &[String], fg: Rgb, bg: Rgb) -> Vec<Vec<Cell>> {
    use unicode_width::UnicodeWidthStr;
    let width = lines.iter().map(|line| line.width()).max().unwrap_or(0);
    lines
        .iter()
        .map(|line| {
            let mut row: Vec<Cell> = Vec::with_capacity(width);
            for grapheme in
                unicode_segmentation::UnicodeSegmentation::graphemes(line.as_str(), true)
            {
                let cells = grapheme.width().max(1);
                row.push(Cell {
                    text: Some(grapheme.to_string()),
                    fg,
                    bg,
                    bold: false,
                });
                for _ in 1..cells {
                    row.push(Cell {
                        text: None,
                        fg,
                        bg,
                        bold: false,
                    });
                }
            }
            while row.len() < width {
                row.push(Cell {
                    text: Some(" ".to_string()),
                    fg,
                    bg,
                    bold: false,
                });
            }
            row
        })
        .collect()
}

fn blank() -> Cell {
    Cell {
        text: Some(" ".to_string()),
        fg: DARK_FG,
        bg: DARK_BG,
        bold: false,
    }
}

/// Draw the cells and encode them as a PNG. `background` fills the margin.
pub fn render(cells: &[Vec<Cell>], background: Rgb) -> Result<Vec<u8>, String> {
    render_at(cells, background, FONT_PX)
}

/// [`render`] at a font size of the caller's: a table is a few short lines, and at
/// screen size Telegram shows it as a thumbnail.
pub fn render_at(cells: &[Vec<Cell>], background: Rgb, font_px: f32) -> Result<Vec<u8>, String> {
    let fonts = Fonts::load(font_px)?;
    let columns = cells.iter().map(Vec::len).max().unwrap_or(0).min(MAX_CELLS);
    let rows = cells.len().min(MAX_CELLS);
    if columns == 0 || rows == 0 {
        return Err("nothing to draw".to_string());
    }
    let cell_width = fonts.cell_width;
    let cell_height = fonts.cell_height;
    let width = columns as u32 * cell_width + 2 * MARGIN;
    let height = rows as u32 * cell_height + 2 * MARGIN;
    let mut canvas = Canvas::new(width, height, background);

    for (row_index, row) in cells.iter().take(rows).enumerate() {
        let top = MARGIN + row_index as u32 * cell_height;
        for (column, cell) in row.iter().take(columns).enumerate() {
            let left = MARGIN + column as u32 * cell_width;
            canvas.fill(left, top, cell_width, cell_height, cell.bg);
        }
        for (column, cell) in row.iter().take(columns).enumerate() {
            let Some(text) = cell.text.as_deref() else {
                continue;
            };
            if text.trim().is_empty() {
                continue;
            }
            // A wide character owns the next cell too; draw it across both.
            let span = 1 + row
                .iter()
                .skip(column + 1)
                .take_while(|next| next.text.is_none())
                .count();
            let left = MARGIN + column as u32 * cell_width;
            fonts.draw(
                &mut canvas,
                text,
                (left, top),
                span as u32,
                cell.fg,
                cell.bold,
            );
        }
    }
    canvas.png()
}

struct Fonts {
    mono: FontRef<'static>,
    emoji: FontRef<'static>,
    px: f32,
    scale: PxScale,
    cell_width: u32,
    cell_height: u32,
    baseline: f32,
}

impl Fonts {
    fn load(px: f32) -> Result<Self, String> {
        let mono = FontRef::try_from_slice(epaint_default_fonts::HACK_REGULAR)
            .map_err(|_| "the embedded monospace font did not load".to_string())?;
        let emoji = FontRef::try_from_slice(epaint_default_fonts::NOTO_EMOJI_REGULAR)
            .map_err(|_| "the embedded emoji font did not load".to_string())?;
        let scale = PxScale::from(px);
        let scaled = mono.as_scaled(scale);
        let cell_width = scaled.h_advance(mono.glyph_id('M')).round().max(1.0) as u32;
        let line = scaled.ascent() - scaled.descent() + scaled.line_gap();
        let cell_height = line.round().max(1.0) as u32;
        let baseline = scaled.ascent();
        Ok(Self {
            mono,
            emoji,
            px,
            scale,
            cell_width,
            cell_height,
            baseline,
        })
    }

    fn draw(
        &self,
        canvas: &mut Canvas,
        text: &str,
        (left, top): (u32, u32),
        span: u32,
        fg: Rgb,
        bold: bool,
    ) {
        // One glyph per cell: the first character carries it, and a combining mark
        // or a variation selector after it is not drawn separately.
        let Some(character) = text.chars().next() else {
            return;
        };
        let (font, id, scale) = self.glyph_for(character, span);
        if id.0 == 0 {
            return;
        }
        let glyph =
            id.with_scale_and_position(scale, point(left as f32, top as f32 + self.baseline));
        let Some(outlined) = font.outline_glyph(glyph) else {
            return;
        };
        let bounds = outlined.px_bounds();
        let passes: &[f32] = if bold { &[0.0, 1.0] } else { &[0.0] };
        for offset in passes {
            outlined.draw(|x, y, coverage| {
                let px = (bounds.min.x + x as f32 + offset).round() as i64;
                let py = (bounds.min.y + y as f32).round() as i64;
                canvas.blend(px, py, fg, coverage);
            });
        }
    }

    /// The font that has a glyph for this character, the monospace one first. An
    /// emoji is drawn to fill the cells it occupies.
    fn glyph_for(&self, character: char, span: u32) -> (&FontRef<'static>, GlyphId, PxScale) {
        let id = self.mono.glyph_id(character);
        if id.0 != 0 {
            return (&self.mono, id, self.scale);
        }
        let id = self.emoji.glyph_id(character);
        let scaled = self.emoji.as_scaled(self.scale);
        let advance = scaled.h_advance(id).max(1.0);
        let wanted = (self.cell_width * span) as f32;
        let factor = (wanted / advance).min(1.0);
        (&self.emoji, id, PxScale::from(self.px * factor))
    }
}

struct Canvas {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

impl Canvas {
    fn new(width: u32, height: u32, background: Rgb) -> Self {
        let mut pixels = Vec::with_capacity((width * height * 3) as usize);
        for _ in 0..width * height {
            pixels.extend_from_slice(&[background.0, background.1, background.2]);
        }
        Self {
            width,
            height,
            pixels,
        }
    }

    fn fill(&mut self, left: u32, top: u32, width: u32, height: u32, color: Rgb) {
        for y in top..(top + height).min(self.height) {
            for x in left..(left + width).min(self.width) {
                let at = ((y * self.width + x) * 3) as usize;
                self.pixels[at] = color.0;
                self.pixels[at + 1] = color.1;
                self.pixels[at + 2] = color.2;
            }
        }
    }

    fn blend(&mut self, x: i64, y: i64, color: Rgb, coverage: f32) {
        if x < 0 || y < 0 || x >= self.width as i64 || y >= self.height as i64 || coverage <= 0.0 {
            return;
        }
        let at = ((y as u32 * self.width + x as u32) * 3) as usize;
        let mix = |under: u8, over: u8| -> u8 {
            (under as f32 + (over as f32 - under as f32) * coverage.min(1.0)).round() as u8
        };
        self.pixels[at] = mix(self.pixels[at], color.0);
        self.pixels[at + 1] = mix(self.pixels[at + 1], color.1);
        self.pixels[at + 2] = mix(self.pixels[at + 2], color.2);
    }

    fn png(&self) -> Result<Vec<u8>, String> {
        let mut out = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut out, self.width, self.height);
            encoder.set_color(png::ColorType::Rgb);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder
                .write_header()
                .map_err(|error| format!("PNG header: {error}"))?;
            writer
                .write_image_data(&self.pixels)
                .map_err(|error| format!("PNG data: {error}"))?;
        }
        Ok(out)
    }
}

/// A terminal colour as RGB: the Campbell scheme for the first sixteen, then the
/// standard cube and greys.
fn color(color: vt100::Color, default: Rgb) -> Rgb {
    const BASE: [Rgb; 16] = [
        (12, 12, 12),
        (197, 15, 31),
        (19, 161, 14),
        (193, 156, 0),
        (0, 55, 218),
        (136, 23, 152),
        (58, 150, 221),
        (204, 204, 204),
        (118, 118, 118),
        (231, 72, 86),
        (22, 198, 12),
        (249, 241, 165),
        (59, 120, 255),
        (180, 0, 158),
        (97, 214, 214),
        (242, 242, 242),
    ];
    match color {
        vt100::Color::Default => default,
        vt100::Color::Rgb(r, g, b) => (r, g, b),
        vt100::Color::Idx(index) if index < 16 => BASE[index as usize],
        vt100::Color::Idx(index) if index < 232 => {
            let index = index - 16;
            let level = |value: u8| if value == 0 { 0 } else { 55 + value * 40 };
            (level(index / 36), level((index / 6) % 6), level(index % 6))
        }
        vt100::Color::Idx(index) => {
            let value = 8 + (index - 232) * 10;
            (value, value, value)
        }
    }
}

/// The width and height a PNG declares, from its header.
#[cfg(test)]
pub fn png_size(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 24 || &bytes[1..4] != b"PNG" {
        return None;
    }
    let read =
        |at: usize| u32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
    Some((read(16), read(20)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_screen_is_drawn_at_one_cell_per_column_with_a_margin_and_decodes_as_a_png() {
        let mut parser = vt100::Parser::new(4, 12, 0);
        parser.process(b"\x1b[1mbold\x1b[0m \x1b[31mred\x1b[0m\r\n\xe7\x95\x8c ok\r\n");
        let cells = cells(parser.screen());
        assert_eq!(cells.len(), 2, "blank tail rows are dropped");
        assert_eq!(cells[0][0].text.as_deref(), Some("b"));
        assert!(cells[0][0].bold);
        assert_eq!(cells[0][5].fg, (197, 15, 31));
        assert_eq!(cells[1][1].text, None, "the wide character's second half");

        let png = render(&cells, DARK_BG).unwrap();
        let (width, height) = png_size(&png).unwrap();
        let fonts = Fonts::load(FONT_PX).unwrap();
        assert_eq!(width, 12 * fonts.cell_width + 2 * MARGIN);
        assert_eq!(height, 2 * fonts.cell_height + 2 * MARGIN);
    }

    #[test]
    fn text_lines_become_a_light_block_as_wide_as_the_widest_line() {
        let lines = vec!["Name  Size".to_string(), "a     1".to_string()];
        let cells = cells_of_text(&lines, LIGHT_FG, LIGHT_BG);
        assert_eq!(cells[0].len(), 10);
        assert_eq!(cells[1].len(), 10, "short lines are padded");
        let png = render(&cells, LIGHT_BG).unwrap();
        assert!(png_size(&png).is_some());
    }

    #[test]
    fn the_glyph_set_covers_what_copilot_draws() {
        let fonts = Fonts::load(FONT_PX).unwrap();
        for character in ['─', '│', '┌', '┐', '└', '┘', '●', '▶', '…', 'é'] {
            let (_, id, _) = fonts.glyph_for(character, 1);
            assert_ne!(id.0, 0, "no glyph for {character:?}");
        }
        let (_, id, _) = fonts.glyph_for('🚀', 2);
        assert_ne!(id.0, 0, "emoji come from the second font");
    }
}
