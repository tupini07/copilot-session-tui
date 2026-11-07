//! A picture the model looked at, shown inside CST.
//!
//! Copilot draws an image it reads straight into the terminal, as Sixel, when the
//! terminal says it can take one. A CST pane cannot say that: the Windows pseudo-console
//! it runs on drops the pixels before CST ever sees them, so the pane shows the
//! `[Image: Image file at path …]` line Copilot prints instead. That line carries the
//! path, and the file is on this machine, so CST reads it itself and shows it in a popup.
//!
//! The popup is the whole trick. An image drawn inline would have to scroll with the
//! pane, and nothing tells CST when a terminal scrolls its Sixel rows away; a popup sits
//! still until it is closed. On Windows Terminal the picture goes out as Sixel; anywhere
//! else, including over SSH into a Windows machine whose SSH server strips Sixel the same
//! way, it is drawn in half-block characters, two pixels to a cell.
//!
//! State and geometry only. Drawing is in [`crate::ui::image_viewer`], and the Sixel
//! bytes are written after the frame by the event loop.

use anyhow::{Context, Result};
use image::imageops::FilterType;
use image::RgbaImage;
use ratatui::layout::Rect;
use std::path::{Path, PathBuf};

/// How the outer terminal can be given a picture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostGraphics {
    /// The terminal draws Sixel, and the bytes reach it.
    Sixel,
    /// Draw with `▀`, the upper pixel in the foreground and the lower in the background.
    HalfBlocks,
}

/// The pixel size Windows Terminal reports for one cell when asked with `CSI 16 t`:
/// a virtual 10×20, whatever the font, because it scales Sixel to the real cell itself.
/// That is also what makes it safe to skip the query.
pub const WINDOWS_TERMINAL_CELL: (u32, u32) = (10, 20);

/// The longest side a decoded picture is kept at. The viewer never has more than about
/// a thousand virtual pixels to fill, and a 6000-pixel photograph is slow to scale on
/// every resize.
const MAX_DECODED_SIDE: u32 = 2048;

/// What the host can draw, from the environment CST started in.
///
/// Windows Terminal sets `WT_SESSION`. Over SSH that variable, if it is set at all,
/// describes the terminal on the far machine, and the SSH server's pseudo-console
/// strips Sixel anyway, so the answer there is half-blocks.
pub fn detect_host_graphics(is_set: impl Fn(&str) -> bool) -> HostGraphics {
    if is_set("WT_SESSION") && !crate::links::running_over_ssh(&is_set) {
        HostGraphics::Sixel
    } else {
        HostGraphics::HalfBlocks
    }
}

#[derive(Debug)]
pub struct ImageViewer {
    pub path: PathBuf,
    /// The picture's real size, for the title.
    pub width: u32,
    pub height: u32,
    pub graphics: HostGraphics,
    image: RgbaImage,
    /// Where the picture was placed by the last frame, in screen cells.
    area: Option<Rect>,
    /// The whole frame's size at the last draw. ratatui clears the terminal when that
    /// changes, which takes the Sixel with it, so it has to be painted again.
    frame: Option<Rect>,
    /// The Sixel last written, and where. Writing it again while nothing moved would
    /// only flicker, so a frame that leaves it in place sends nothing.
    painted: Option<Rect>,
    sixel: Option<(Rect, String)>,
}

impl ImageViewer {
    /// Read and decode the file. Fails with a sentence fit for the status line.
    pub fn open(path: &Path, graphics: HostGraphics) -> Result<Self> {
        let decoded = image::ImageReader::open(path)
            .with_context(|| format!("cannot read {}", path.display()))?
            .with_guessed_format()
            .with_context(|| format!("cannot read {}", path.display()))?
            .decode()
            .with_context(|| format!("{} is not a picture CST can decode", path.display()))?;
        let (width, height) = (decoded.width(), decoded.height());
        let image = if width.max(height) > MAX_DECODED_SIDE {
            decoded
                .thumbnail(MAX_DECODED_SIDE, MAX_DECODED_SIDE)
                .to_rgba8()
        } else {
            decoded.to_rgba8()
        };
        Ok(Self {
            path: path.to_path_buf(),
            width,
            height,
            graphics,
            image,
            area: None,
            frame: None,
            painted: None,
            sixel: None,
        })
    }

    /// The largest cell rectangle inside `body` with the picture's proportions, centred.
    ///
    /// In Sixel the cell is Windows Terminal's virtual 10×20; in half-blocks it is one
    /// pixel wide and two tall. Both make a cell twice as tall as wide, so the picture
    /// keeps its shape either way.
    pub fn fit(&self, body: Rect) -> Rect {
        let (cell_w, cell_h) = match self.graphics {
            HostGraphics::Sixel => WINDOWS_TERMINAL_CELL,
            HostGraphics::HalfBlocks => (1, 2),
        };
        fit_cells(
            (self.image.width(), self.image.height()),
            (cell_w, cell_h),
            body,
        )
    }

    /// Record where this frame put the picture, and how big the frame was.
    pub fn placed(&mut self, area: Rect, frame: Rect) {
        if self.frame != Some(frame) {
            // A new frame size means ratatui cleared the screen under the picture.
            self.painted = None;
            self.frame = Some(frame);
        }
        self.area = Some(area);
    }

    /// The picture scaled to `area`, two pixels per cell: `(upper, lower)` colours.
    pub fn half_block_rows(&self, area: Rect) -> Vec<Vec<([u8; 4], [u8; 4])>> {
        if area.width == 0 || area.height == 0 {
            return Vec::new();
        }
        let scaled = image::imageops::resize(
            &self.image,
            u32::from(area.width),
            u32::from(area.height) * 2,
            FilterType::Triangle,
        );
        (0..area.height)
            .map(|row| {
                (0..area.width)
                    .map(|column| {
                        let y = u32::from(row) * 2;
                        let upper = scaled.get_pixel(u32::from(column), y).0;
                        let lower = scaled.get_pixel(u32::from(column), y + 1).0;
                        (upper, lower)
                    })
                    .collect()
            })
            .collect()
    }

    /// The bytes that put the picture on the terminal after a frame, or nothing when it
    /// is already there or not drawn with Sixel.
    pub fn paint(&mut self) -> Vec<u8> {
        if self.graphics != HostGraphics::Sixel {
            return Vec::new();
        }
        let Some(area) = self.area else {
            return Vec::new();
        };
        if self.painted == Some(area) || area.width == 0 || area.height == 0 {
            return Vec::new();
        }
        let sixel = match &self.sixel {
            Some((cached, sixel)) if *cached == area => sixel.clone(),
            _ => {
                let (cell_w, cell_h) = WINDOWS_TERMINAL_CELL;
                let scaled = image::imageops::resize(
                    &self.image,
                    u32::from(area.width) * cell_w,
                    u32::from(area.height) * cell_h,
                    FilterType::Triangle,
                );
                let sixel = crate::sixel::encode(&scaled);
                self.sixel = Some((area, sixel.clone()));
                sixel
            }
        };
        self.painted = Some(area);
        // Cursor saved and restored around it, and autowrap off while it is drawn, so a
        // picture ending at the right edge cannot push the cursor onto the next line
        // and scroll the screen.
        format!(
            "\x1b7\x1b[?7l\x1b[{};{}H{sixel}\x1b[?7h\x1b8",
            area.y + 1,
            area.x + 1
        )
        .into_bytes()
    }
}

/// The largest cell rectangle inside `body` that shows `pixels` at `cell` pixels per
/// cell without distortion, centred.
fn fit_cells(pixels: (u32, u32), cell: (u32, u32), body: Rect) -> Rect {
    if body.width == 0 || body.height == 0 || pixels.0 == 0 || pixels.1 == 0 {
        return Rect::new(body.x, body.y, 0, 0);
    }
    // The picture's size in cells if it were shown at full size, then scaled down to
    // the body by whichever side overflows more.
    let natural_w = f64::from(pixels.0) / f64::from(cell.0);
    let natural_h = f64::from(pixels.1) / f64::from(cell.1);
    let scale = (f64::from(body.width) / natural_w)
        .min(f64::from(body.height) / natural_h)
        .min(1.0);
    let width = ((natural_w * scale).round() as u16).clamp(1, body.width);
    let height = ((natural_h * scale).round() as u16).clamp(1, body.height);
    Rect::new(
        body.x + (body.width - width) / 2,
        body.y + (body.height - height) / 2,
        width,
        height,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn viewer(width: u32, height: u32, graphics: HostGraphics) -> ImageViewer {
        ImageViewer {
            path: PathBuf::from("picture.png"),
            width,
            height,
            graphics,
            image: RgbaImage::from_pixel(width, height, image::Rgba([10, 20, 30, 255])),
            area: None,
            frame: None,
            painted: None,
            sixel: None,
        }
    }

    #[test]
    fn windows_terminal_draws_sixel_but_not_when_reached_over_ssh() {
        let local = |name: &str| name == "WT_SESSION";
        assert_eq!(detect_host_graphics(local), HostGraphics::Sixel);
        let remote = |name: &str| name == "WT_SESSION" || name == "SSH_TTY";
        assert_eq!(detect_host_graphics(remote), HostGraphics::HalfBlocks);
        assert_eq!(detect_host_graphics(|_| false), HostGraphics::HalfBlocks);
    }

    #[test]
    fn a_wide_picture_fills_the_width_and_is_centred_vertically() {
        // 1000×200 pixels at 10×20 per cell is 100×10 cells; in a 50×20 body it halves.
        let area = fit_cells((1000, 200), (10, 20), Rect::new(2, 3, 50, 20));
        assert_eq!(area, Rect::new(2, 3 + (20 - 5) / 2, 50, 5));
    }

    #[test]
    fn a_small_picture_is_not_blown_up_past_its_own_size() {
        let area = fit_cells((100, 40), (10, 20), Rect::new(0, 0, 80, 20));
        assert_eq!((area.width, area.height), (10, 2));
    }

    #[test]
    fn half_blocks_give_a_tall_picture_the_full_height_at_one_pixel_per_column() {
        // 100×400 pixels at 1×2 per cell is 100×200 cells; a 90×20 body limits height.
        let area = fit_cells((100, 400), (1, 2), Rect::new(0, 0, 90, 20));
        assert_eq!((area.width, area.height), (10, 20));
    }

    #[test]
    fn sixel_is_written_once_and_again_only_after_the_frame_changed_size() {
        let mut viewer = viewer(20, 40, HostGraphics::Sixel);
        let frame = Rect::new(0, 0, 110, 30);
        viewer.placed(Rect::new(5, 4, 2, 2), frame);
        let first = viewer.paint();
        assert!(
            first.starts_with(b"\x1b7\x1b[?7l\x1b[5;6H\x1bP"),
            "{first:?}"
        );
        assert!(first.ends_with(b"\x1b\\\x1b[?7h\x1b8"), "{first:?}");
        viewer.placed(Rect::new(5, 4, 2, 2), frame);
        assert!(
            viewer.paint().is_empty(),
            "a still picture is not sent twice"
        );
        viewer.placed(Rect::new(5, 4, 2, 2), Rect::new(0, 0, 120, 30));
        assert_eq!(
            viewer.paint(),
            first,
            "a resize clears the screen, so it goes again"
        );
    }

    #[test]
    fn half_block_hosts_get_no_sixel_bytes_at_all() {
        let mut viewer = viewer(20, 40, HostGraphics::HalfBlocks);
        viewer.placed(Rect::new(5, 4, 2, 2), Rect::new(0, 0, 110, 30));
        assert!(viewer.paint().is_empty());
        let rows = viewer.half_block_rows(Rect::new(5, 4, 2, 2));
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].len(), 2);
        assert_eq!(rows[0][0], ([10, 20, 30, 255], [10, 20, 30, 255]));
    }

    #[test]
    fn a_file_that_is_not_a_picture_is_refused_with_its_name() {
        let file = tempfile::Builder::new().suffix(".png").tempfile().unwrap();
        std::fs::write(file.path(), b"not a png at all").unwrap();
        let error = ImageViewer::open(file.path(), HostGraphics::HalfBlocks)
            .expect_err("garbage must not decode")
            .to_string();
        assert!(error.contains("is not a picture CST can decode"), "{error}");
    }
}
