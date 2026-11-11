//! Encoding a picture as Sixel, for a terminal that draws it.
//!
//! Windows Terminal has drawn Sixel since 1.22 and nothing else that carries pixels, so
//! this is how the image viewer gets a picture onto the screen there. The format is
//! 1984 vintage: a palette of up to 256 colours, then the picture in bands six pixels
//! tall, each band one character per column per colour, the six bits of the character
//! saying which of the band's rows that colour fills.
//!
//! Written here rather than taken from a crate because the one ratatui image crate is a
//! release candidate that brings its own terminal probing, and the encoder itself is
//! the smaller part of the problem. Pure: pixels in, bytes out.

use color_quant::NeuQuant;
use image::RgbaImage;

/// Sixel's limit, and the most the palette ever holds.
const COLOURS: usize = 256;
/// A pixel this transparent is left unpainted, so the popup's background shows through.
const OPAQUE: u8 = 128;

/// The picture as a Sixel string, raster header through string terminator.
///
/// Transparent pixels are left out rather than drawn black: the `P2 = 1` parameter in
/// the introducer tells the terminal to keep what is behind them.
pub fn encode(image: &RgbaImage) -> String {
    let (width, height) = image.dimensions();
    if width == 0 || height == 0 {
        return String::new();
    }
    let pixels: Vec<u8> = image
        .pixels()
        .flat_map(|pixel| {
            let [r, g, b, a] = pixel.0;
            // What the quantiser sees of a transparent pixel does not matter, since it is
            // never drawn; keeping its colour out of the sample keeps the palette for
            // the pixels that are.
            if a < OPAQUE {
                [0, 0, 0, 0]
            } else {
                [r, g, b, 255]
            }
        })
        .collect();
    let (palette, indices) = quantize(&pixels);

    let mut out = String::with_capacity(pixels.len() / 2);
    // P1 0: pixel aspect ratio as the raster attributes say. P2 1: unpainted pixels keep
    // the background. P3 0: horizontal grid size, unused.
    out.push_str("\x1bP0;1;0q");
    out.push_str(&format!("\"1;1;{width};{height}"));
    for (index, colour) in palette.chunks_exact(3).enumerate() {
        let [r, g, b] = [colour[0], colour[1], colour[2]].map(percent);
        out.push_str(&format!("#{index};2;{r};{g};{b}"));
    }

    let width = width as usize;
    let height = height as usize;
    for band in (0..height).step_by(6) {
        let rows = (height - band).min(6);
        let mut present = [false; COLOURS];
        for row in band..band + rows {
            for index in indices[row * width..(row + 1) * width].iter().flatten() {
                present[*index as usize] = true;
            }
        }
        let mut first = true;
        for (colour, _) in present.iter().enumerate().filter(|(_, used)| **used) {
            // `$` returns to the band's left edge, so the next colour paints over the
            // same six rows.
            if !first {
                out.push('$');
            }
            first = false;
            out.push_str(&format!("#{colour}"));
            let mut sixels: Vec<u8> = (0..width)
                .map(|column| {
                    (0..rows)
                        .filter(|row| indices[(band + row) * width + column] == Some(colour as u8))
                        .fold(0u8, |bits, row| bits | (1 << row))
                })
                .collect();
            // Trailing empty columns need not be sent; the band is already that wide.
            while sixels.last() == Some(&0) {
                sixels.pop();
            }
            push_runs(&mut out, &sixels);
        }
        if band + rows < height {
            out.push('-');
        }
    }
    out.push_str("\x1b\\");
    out
}

/// The palette, as RGB triples, and each pixel's index into it, `None` for transparent.
///
/// A picture a model reads is usually a screenshot, a diagram or a render: flat regions
/// in a few dozen exact colours. Those are kept exactly when they fit the palette, which
/// also keeps a small picture from coming out in the grey ramp NeuQuant starts from
/// before it has seen enough pixels to learn anything. Only a photograph, with more
/// colours than Sixel can name, is quantised.
fn quantize(rgba: &[u8]) -> (Vec<u8>, Vec<Option<u8>>) {
    let opaque = |pixel: &[u8]| pixel[3] >= OPAQUE;
    let mut exact: Vec<[u8; 3]> = Vec::new();
    for pixel in rgba.chunks_exact(4).filter(|pixel| opaque(pixel)) {
        let colour = [pixel[0], pixel[1], pixel[2]];
        if !exact.contains(&colour) {
            exact.push(colour);
            if exact.len() > COLOURS {
                break;
            }
        }
    }
    if exact.len() <= COLOURS {
        let indices = rgba
            .chunks_exact(4)
            .map(|pixel| {
                opaque(pixel).then(|| {
                    let colour = [pixel[0], pixel[1], pixel[2]];
                    exact.iter().position(|known| *known == colour).unwrap_or(0) as u8
                })
            })
            .collect();
        return (exact.concat(), indices);
    }
    let quantizer = NeuQuant::new(10, COLOURS, rgba);
    let indices = rgba
        .chunks_exact(4)
        .map(|pixel| opaque(pixel).then(|| quantizer.index_of(pixel) as u8))
        .collect();
    (quantizer.color_map_rgb(), indices)
}

/// Run-length encode one colour's row of sixels.
fn push_runs(out: &mut String, sixels: &[u8]) {
    let mut index = 0;
    while index < sixels.len() {
        let bits = sixels[index];
        let mut run = 1;
        while index + run < sixels.len() && sixels[index + run] == bits {
            run += 1;
        }
        let glyph = (63 + bits) as char;
        // `!n` costs at least three characters, so short runs are cheaper spelled out.
        if run > 3 {
            out.push_str(&format!("!{run}{glyph}"));
        } else {
            out.extend(std::iter::repeat_n(glyph, run));
        }
        index += run;
    }
}

/// Sixel palettes are in percent, not bytes.
fn percent(value: u8) -> u8 {
    ((u16::from(value) * 100 + 127) / 255) as u8
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;

    fn solid(width: u32, height: u32, colour: [u8; 4]) -> RgbaImage {
        RgbaImage::from_pixel(width, height, Rgba(colour))
    }

    #[test]
    fn a_single_red_pixel_is_a_red_palette_entry_painted_in_the_top_row() {
        let out = encode(&solid(1, 1, [255, 0, 0, 255]));
        assert!(out.starts_with("\x1bP0;1;0q\"1;1;1;1#"), "{out:?}");
        assert!(out.ends_with("\x1b\\"), "{out:?}");
        // The top row of a band is bit 0, and 63 + 1 is `@`.
        assert!(
            out.contains(";2;100;0;0"),
            "palette lacks pure red: {out:?}"
        );
        assert!(out.contains('@'), "no top-row sixel: {out:?}");
    }

    #[test]
    fn a_long_run_of_one_colour_is_sent_as_a_repeat_count_not_spelled_out() {
        let out = encode(&solid(40, 1, [0, 0, 255, 255]));
        assert!(out.contains("!40@"), "{out:?}");
    }

    #[test]
    fn a_picture_taller_than_six_rows_is_sent_in_bands() {
        let out = encode(&solid(1, 7, [0, 255, 0, 255]));
        let body = out.split('"').nth(1).unwrap();
        // Six rows fill one band (`~` is all six bits); the seventh is a top row alone.
        assert_eq!(body.matches('-').count(), 1, "{out:?}");
        assert!(body.contains('~'), "{out:?}");
        assert!(body.ends_with("@\x1b\\"), "{out:?}");
    }

    #[test]
    fn transparent_pixels_are_left_unpainted_so_the_background_shows_through() {
        let out = encode(&solid(4, 4, [255, 255, 255, 0]));
        let body = out.split('"').nth(1).unwrap();
        // The palette is still announced, but nothing after it paints: no sixel glyph
        // (`?` through `~`) appears at all.
        assert!(!body.contains('$'), "{out:?}");
        assert!(
            !body
                .trim_end_matches("\x1b\\")
                .chars()
                .any(|c| ('?'..='~').contains(&c)),
            "{out:?}"
        );
        assert!(
            out.contains(";1;0q"),
            "transparency must be requested: {out:?}"
        );
    }

    #[test]
    fn a_picture_with_few_colours_keeps_them_exactly_instead_of_being_quantised() {
        let mut picture = solid(4, 1, [10, 20, 30, 255]);
        picture.put_pixel(2, 0, Rgba([250, 240, 230, 255]));
        let out = encode(&picture);
        // 10/255 is 4%, 20/255 is 8%, 30/255 is 12%; 250 is 98%, 240 is 94%, 230 is 90%.
        assert!(out.contains("#0;2;4;8;12"), "{out:?}");
        assert!(out.contains("#1;2;98;94;90"), "{out:?}");
        assert_eq!(
            out.matches(";2;").count(),
            2,
            "only the two colours used: {out:?}"
        );
    }

    #[test]
    fn a_photograph_with_more_colours_than_sixel_allows_still_fits_the_palette() {
        let picture = RgbaImage::from_fn(32, 32, |x, y| {
            Rgba([(x * 8) as u8, (y * 8) as u8, ((x + y) * 4) as u8, 255])
        });
        let out = encode(&picture);
        assert_eq!(out.matches(";2;").count(), COLOURS, "{}", out.len());
        assert!(out.ends_with("\x1b\\"));
    }

    #[test]
    fn an_empty_picture_encodes_to_nothing_rather_than_a_header_with_no_body() {
        assert_eq!(encode(&RgbaImage::new(0, 0)), "");
    }
}
