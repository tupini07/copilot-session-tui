//! The image viewer popup. Why it is a popup, and how the picture reaches the terminal,
//! is in [`crate::image_viewer`]; this draws the frame around it.
//!
//! In Sixel the body is left as plain surface-coloured cells and the pixels are written
//! over them after the frame. In half-blocks the body is the picture, one `▀` per cell
//! with a pixel in each of its colours.

use crate::app::App;
use crate::image_viewer::HostGraphics;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear};
use ratatui::Frame;

pub fn draw(f: &mut Frame, app: &mut App) {
    let theme = app.theme();
    let Some(viewer) = app.image_viewer.as_mut() else {
        return;
    };
    let screen = f.area();
    let area = super::popups::centered_rect(92, 92, screen);
    if area.width < 8 || area.height < 4 {
        return;
    }
    f.render_widget(Clear, area);
    let surface_text = super::foreground_on(theme, theme.surface);
    let name = viewer
        .path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let title = format!(" {name}  {}x{} ", viewer.width, viewer.height);
    let hint = Line::from(Span::styled(
        " Esc close · o open outside CST ",
        Style::default().fg(theme.muted),
    ))
    .right_aligned();
    let block = Block::default()
        .title(title)
        .title_bottom(hint)
        .borders(Borders::ALL)
        .style(Style::default().fg(surface_text).bg(theme.surface))
        .border_style(Style::default().fg(super::semantic_foreground_on(
            theme,
            theme.accent,
            theme.surface,
        )));
    let inner = block.inner(area);
    f.render_widget(block, area);
    // A cell of margin at the sides, so the picture does not touch the border.
    let body = Rect::new(
        inner.x + 1,
        inner.y,
        inner.width.saturating_sub(2),
        inner.height,
    );
    let image_area = viewer.fit(body);
    viewer.placed(image_area, screen);
    if viewer.graphics != HostGraphics::HalfBlocks {
        return;
    }
    let rows = viewer.half_block_rows(image_area);
    for (dy, row) in rows.iter().enumerate() {
        for (dx, (upper, lower)) in row.iter().enumerate() {
            let position = (image_area.x + dx as u16, image_area.y + dy as u16);
            if let Some(cell) = f.buffer_mut().cell_mut(position) {
                cell.set_symbol("▀")
                    .set_fg(over(*upper, theme.surface))
                    .set_bg(over(*lower, theme.surface));
            }
        }
    }
}

/// The pixel composited onto the popup's surface, so a transparent picture shows the
/// popup rather than black.
fn over(pixel: [u8; 4], surface: Color) -> Color {
    let [r, g, b, a] = pixel;
    let Color::Rgb(sr, sg, sb) = surface else {
        return Color::Rgb(r, g, b);
    };
    let blend = |top: u8, under: u8| -> u8 {
        let alpha = u32::from(a);
        ((u32::from(top) * alpha + u32::from(under) * (255 - alpha)) / 255) as u8
    };
    Color::Rgb(blend(r, sr), blend(g, sg), blend(b, sb))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image_viewer::ImageViewer;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn app_showing(width: u32, height: u32) -> (App, tempfile::TempPath) {
        let file = tempfile::Builder::new().suffix(".png").tempfile().unwrap();
        image::RgbaImage::from_pixel(width, height, image::Rgba([200, 30, 30, 255]))
            .save(file.path())
            .unwrap();
        let mut app = App::new(Vec::new(), crate::config::UserConfig::default());
        app.image_viewer = Some(ImageViewer::open(file.path(), HostGraphics::HalfBlocks).unwrap());
        (app, file.into_temp_path())
    }

    fn frame_text(app: &mut App) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(110, 30)).unwrap();
        terminal.draw(|f| draw(f, app)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer.cell((x, y)).unwrap().symbol().to_string())
                    .collect()
            })
            .collect()
    }

    #[test]
    fn the_popup_names_the_file_and_its_real_size_and_says_how_to_leave() {
        let (mut app, path) = app_showing(640, 480);
        let lines = frame_text(&mut app);
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        assert!(
            lines
                .iter()
                .any(|line| line.contains(&format!("{name}  640x480"))),
            "{lines:#?}"
        );
        assert!(
            lines
                .iter()
                .any(|line| line.contains("Esc close · o open outside CST")),
            "{lines:#?}"
        );
    }

    #[test]
    fn without_sixel_the_picture_is_drawn_in_half_blocks_inside_the_frame() {
        let (mut app, _path) = app_showing(640, 480);
        let lines = frame_text(&mut app);
        let blocks: usize = lines.iter().map(|line| line.matches('▀').count()).sum();
        // 640×480 at one pixel per column and two per row wants 640×240 cells and is
        // scaled into the popup's body, where it keeps its 4:3 shape.
        let tall = lines.iter().filter(|line| line.contains('▀')).count();
        let wide = lines
            .iter()
            .map(|line| line.matches('▀').count())
            .max()
            .unwrap_or(0);
        assert!(blocks > 0, "{lines:#?}");
        assert_eq!(blocks, tall * wide, "the picture is a full rectangle");
        assert!(
            (wide as f64 / (tall as f64 * 2.0) - 4.0 / 3.0).abs() < 0.15,
            "{wide} cells wide by {tall} tall is not 4:3"
        );
        // Nothing is drawn over the border.
        assert!(
            !lines[1].contains('▀') && !lines[28].contains('▀'),
            "{lines:#?}"
        );
    }
}
