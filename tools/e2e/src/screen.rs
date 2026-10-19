//! Reading the emulated terminal back: finding text on it, and writing it out as
//! text, HTML and a PNG.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

/// One screen row as a person reads it, with the column each character sits in.
///
/// A wide character takes two cells, so the text alone would put everything after
/// it a column early. `vt100`'s flattened `contents()` has the same problem.
pub struct Row {
    pub text: String,
    columns: Vec<u16>,
}

pub fn rows(screen: &vt100::Screen) -> Vec<Row> {
    let (height, width) = screen.size();
    (0..height)
        .map(|row| {
            let mut text = String::new();
            let mut columns = Vec::new();
            for column in 0..width {
                let Some(cell) = screen.cell(row, column) else {
                    continue;
                };
                if cell.is_wide_continuation() {
                    continue;
                }
                let contents = match cell.contents() {
                    "" => " ",
                    contents => contents,
                };
                for character in contents.chars() {
                    text.push(character);
                    columns.push(column);
                }
            }
            Row { text, columns }
        })
        .collect()
}

/// Where `needle` first appears, as a zero-based (column, row).
pub fn find(screen: &vt100::Screen, needle: &str) -> Option<(u16, u16)> {
    rows(screen).iter().enumerate().find_map(|(index, row)| {
        let byte = row.text.find(needle)?;
        let character = row.text[..byte].chars().count();
        Some((row.columns[character], index as u16))
    })
}

pub fn text(screen: &vt100::Screen) -> String {
    let mut out = String::new();
    for row in rows(screen) {
        out.push_str(row.text.trim_end());
        out.push('\n');
    }
    out
}

/// The screen with its colours, which the text cannot show: a highlight, a dimmed
/// hint, or a popup whose background did not fill.
pub fn html(screen: &vt100::Screen) -> String {
    let mut html = String::from(
        "<!doctype html><html><head><meta charset=\"utf-8\"></head>\
         <body style=\"margin:0;background:#0c0c0c\">\
         <pre style=\"margin:8px;font:15px/18px Consolas,'Cascadia Mono',monospace;color:#cccccc\">",
    );
    let (height, width) = screen.size();
    for row in 0..height {
        for column in 0..width {
            let Some(cell) = screen.cell(row, column) else {
                continue;
            };
            if cell.is_wide_continuation() {
                continue;
            }
            let mut fg = color(cell.fgcolor(), "#cccccc");
            let mut bg = color(cell.bgcolor(), "#0c0c0c");
            if cell.inverse() {
                std::mem::swap(&mut fg, &mut bg);
            }
            let text = match cell.contents() {
                "" => " ".to_string(),
                text => text
                    .replace('&', "&amp;")
                    .replace('<', "&lt;")
                    .replace('>', "&gt;"),
            };
            let mut style = format!("color:{fg};background:{bg}");
            if cell.bold() {
                style.push_str(";font-weight:bold");
            }
            if cell.italic() {
                style.push_str(";font-style:italic");
            }
            if cell.underline() {
                style.push_str(";text-decoration:underline");
            }
            html.push_str(&format!("<span style=\"{style}\">{text}</span>"));
        }
        html.push('\n');
    }
    html.push_str("</pre></body></html>");
    html
}

fn color(color: vt100::Color, default: &str) -> String {
    match color {
        vt100::Color::Default => default.to_string(),
        vt100::Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
        vt100::Color::Idx(index) => {
            // The Windows Terminal "Campbell" scheme, then the standard 256-colour cube.
            const BASE: [(u8, u8, u8); 16] = [
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
            let (r, g, b) = if index < 16 {
                BASE[index as usize]
            } else if index < 232 {
                let index = index - 16;
                let level = |value: u8| if value == 0 { 0 } else { 55 + value * 40 };
                (level(index / 36), level((index / 6) % 6), level(index % 6))
            } else {
                let value = 8 + (index - 232) * 10;
                (value, value, value)
            };
            format!("#{r:02x}{g:02x}{b:02x}")
        }
    }
}

/// A Chromium-based browser that can screenshot the HTML, if there is one.
pub fn find_browser() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("CST_E2E_BROWSER") {
        return Some(PathBuf::from(path));
    }
    let mut candidates: Vec<PathBuf> = Vec::new();
    for base in ["ProgramFiles(x86)", "ProgramFiles", "LOCALAPPDATA"] {
        if let Some(base) = std::env::var_os(base) {
            let base = PathBuf::from(base);
            candidates.push(base.join(r"Microsoft\Edge\Application\msedge.exe"));
            candidates.push(base.join(r"Google\Chrome\Application\chrome.exe"));
        }
    }
    candidates.push(PathBuf::from(
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    ));
    candidates.push(PathBuf::from(
        "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
    ));
    if let Some(found) = candidates.into_iter().find(|path| path.is_file()) {
        return Some(found);
    }
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        for name in [
            "chromium",
            "chromium-browser",
            "google-chrome",
            "microsoft-edge",
        ] {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// Screenshots `html` into `png` with a headless browser.
///
/// The browser gets a profile of its own under the run directory: with the
/// default one, an already running browser can take the launch over, and the
/// user's profile would be touched.
pub fn png(
    browser: &Path,
    profile: &Path,
    html: &Path,
    png: &Path,
    size: (u16, u16),
) -> Result<(), String> {
    let (columns, rows) = size;
    // 15px Consolas advances 8.25px per cell; 18px lines; 8px of margin around.
    let width = (f32::from(columns) * 8.25).ceil() as u32 + 24;
    let height = u32::from(rows) * 18 + 24;
    let url = format!(
        "file:///{}",
        html.display()
            .to_string()
            .replace('\\', "/")
            .trim_start_matches('/')
    );
    let mut child = Command::new(browser)
        .arg("--headless")
        .arg("--disable-gpu")
        .arg("--hide-scrollbars")
        .arg("--no-first-run")
        .arg(format!("--user-data-dir={}", profile.display()))
        .arg(format!("--window-size={width},{height}"))
        .arg(format!("--screenshot={}", png.display()))
        .arg(url)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|error| format!("could not start {}: {error}", browser.display()))?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if started.elapsed() > Duration::from_secs(30) => {
                let _ = child.kill();
                return Err("the browser took over 30s to screenshot".to_string());
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(error) => return Err(error.to_string()),
        }
    }
    if png.is_file() {
        Ok(())
    } else {
        Err("the browser exited without writing the screenshot".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_after_a_wide_character_is_found_in_the_column_it_is_drawn_in() {
        let mut parser = vt100::Parser::new(3, 20, 0);
        // The ideograph takes columns 0 and 1, so "ok" starts in column 3.
        parser.process("界 ok".as_bytes());

        assert_eq!(find(parser.screen(), "ok"), Some((3, 0)));
        assert_eq!(text(parser.screen()).lines().next(), Some("界 ok"));
    }
}
