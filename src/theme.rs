use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;
#[cfg(test)]
use ratatui::style::Modifier;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum ThemeName {
    #[default]
    Classic,
    Gruvbox,
    Nord,
    Dracula,
    CatppuccinMocha,
    CatppuccinLatte,
    Palenight,
    SolarizedDark,
    SolarizedLight,
    TokyoNight,
}

impl ThemeName {
    pub const ALL: [Self; 10] = [
        Self::Classic,
        Self::Gruvbox,
        Self::Nord,
        Self::Dracula,
        Self::CatppuccinMocha,
        Self::Palenight,
        Self::SolarizedDark,
        Self::TokyoNight,
        Self::CatppuccinLatte,
        Self::SolarizedLight,
    ];

    pub const DARK: [Self; 7] = [
        Self::Gruvbox,
        Self::Nord,
        Self::Dracula,
        Self::CatppuccinMocha,
        Self::Palenight,
        Self::SolarizedDark,
        Self::TokyoNight,
    ];

    pub const LIGHT: [Self; 2] = [Self::CatppuccinLatte, Self::SolarizedLight];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Classic => "CST Classic",
            Self::Gruvbox => "Gruvbox",
            Self::Nord => "Nord",
            Self::Dracula => "Dracula",
            Self::CatppuccinMocha => "Catppuccin Mocha",
            Self::CatppuccinLatte => "Catppuccin Latte",
            Self::Palenight => "Palenight",
            Self::SolarizedDark => "Solarized Dark",
            Self::SolarizedLight => "Solarized Light",
            Self::TokyoNight => "Tokyo Night",
        }
    }

    pub const fn is_light(self) -> bool {
        matches!(self, Self::CatppuccinLatte | Self::SolarizedLight)
    }

    pub const fn terminal_light_mode(self) -> Option<bool> {
        if matches!(self, Self::Classic) {
            None
        } else {
            Some(self.is_light())
        }
    }

    pub fn theme(self) -> Theme {
        match self {
            Self::Classic => classic(),
            Self::Gruvbox => build(
                self,
                Palette::new([
                    rgb(0x28, 0x28, 0x28),
                    rgb(0x3c, 0x38, 0x36),
                    rgb(0x50, 0x49, 0x45),
                    rgb(0xeb, 0xdb, 0xb2),
                    rgb(0x92, 0x83, 0x74),
                    rgb(0xfe, 0x80, 0x19),
                    rgb(0x83, 0xa5, 0x98),
                    rgb(0x8e, 0xc0, 0x7c),
                    rgb(0xfa, 0xbd, 0x2f),
                    rgb(0xfb, 0x49, 0x34),
                ]),
            ),
            Self::Nord => build(
                self,
                Palette::new([
                    rgb(0x2e, 0x34, 0x40),
                    rgb(0x3b, 0x42, 0x52),
                    rgb(0x43, 0x4c, 0x5e),
                    rgb(0xec, 0xef, 0xf4),
                    rgb(0x7b, 0x88, 0xa1),
                    rgb(0x88, 0xc0, 0xd0),
                    rgb(0x8f, 0xbc, 0xbb),
                    rgb(0xa3, 0xbe, 0x8c),
                    rgb(0xeb, 0xcb, 0x8b),
                    rgb(0xbf, 0x61, 0x6a),
                ]),
            ),
            Self::Dracula => build(
                self,
                Palette::new([
                    rgb(0x28, 0x2a, 0x36),
                    rgb(0x34, 0x37, 0x46),
                    rgb(0x44, 0x47, 0x5a),
                    rgb(0xf8, 0xf8, 0xf2),
                    rgb(0x62, 0x72, 0xa4),
                    rgb(0xbd, 0x93, 0xf9),
                    rgb(0x8b, 0xe9, 0xfd),
                    rgb(0x50, 0xfa, 0x7b),
                    rgb(0xff, 0xb8, 0x6c),
                    rgb(0xff, 0x55, 0x55),
                ]),
            ),
            Self::CatppuccinMocha => build(
                self,
                Palette::new([
                    rgb(0x1e, 0x1e, 0x2e),
                    rgb(0x31, 0x32, 0x44),
                    rgb(0x45, 0x47, 0x5a),
                    rgb(0xcd, 0xd6, 0xf4),
                    rgb(0x6c, 0x70, 0x86),
                    rgb(0xcb, 0xa6, 0xf7),
                    rgb(0x94, 0xe2, 0xd5),
                    rgb(0xa6, 0xe3, 0xa1),
                    rgb(0xf9, 0xe2, 0xaf),
                    rgb(0xf3, 0x8b, 0xa8),
                ]),
            ),
            Self::CatppuccinLatte => build(
                self,
                Palette::new([
                    rgb(0xef, 0xf1, 0xf5),
                    rgb(0xcc, 0xd0, 0xda),
                    rgb(0xef, 0xf1, 0xf5),
                    rgb(0x4c, 0x4f, 0x69),
                    rgb(0x64, 0x67, 0x7c),
                    rgb(0x88, 0x39, 0xef),
                    rgb(0x17, 0x92, 0x99),
                    rgb(0x40, 0xa0, 0x2b),
                    rgb(0xdf, 0x8e, 0x1d),
                    rgb(0xd2, 0x0f, 0x39),
                ]),
            ),
            Self::Palenight => build(
                self,
                Palette::new([
                    rgb(0x29, 0x2d, 0x3e),
                    rgb(0x32, 0x37, 0x4d),
                    rgb(0x43, 0x47, 0x58),
                    rgb(0xa6, 0xac, 0xcd),
                    rgb(0x67, 0x6e, 0x95),
                    rgb(0xc7, 0x92, 0xea),
                    rgb(0x82, 0xaa, 0xff),
                    rgb(0xc3, 0xe8, 0x8d),
                    rgb(0xff, 0xcb, 0x6b),
                    rgb(0xf0, 0x71, 0x78),
                ]),
            ),
            Self::SolarizedDark => build(
                self,
                Palette::new([
                    rgb(0x00, 0x2b, 0x36),
                    rgb(0x07, 0x36, 0x42),
                    rgb(0x0a, 0x40, 0x50),
                    rgb(0x93, 0xa1, 0xa1),
                    rgb(0x58, 0x6e, 0x75),
                    rgb(0x26, 0x8b, 0xd2),
                    rgb(0x2a, 0xa1, 0x98),
                    rgb(0x85, 0x99, 0x00),
                    rgb(0xb5, 0x89, 0x00),
                    rgb(0xdc, 0x32, 0x2f),
                ]),
            ),
            Self::SolarizedLight => build(
                self,
                Palette::new([
                    rgb(0xfd, 0xf6, 0xe3),
                    rgb(0xee, 0xe8, 0xd5),
                    rgb(0xfd, 0xf6, 0xe3),
                    rgb(0x58, 0x6e, 0x75),
                    rgb(0x5f, 0x74, 0x7b),
                    rgb(0x26, 0x8b, 0xd2),
                    rgb(0x2a, 0xa1, 0x98),
                    rgb(0x85, 0x99, 0x00),
                    rgb(0xb5, 0x89, 0x00),
                    rgb(0xdc, 0x32, 0x2f),
                ]),
            ),
            Self::TokyoNight => build(
                self,
                Palette::new([
                    rgb(0x1a, 0x1b, 0x26),
                    rgb(0x24, 0x28, 0x3b),
                    rgb(0x33, 0x46, 0x7c),
                    rgb(0xa9, 0xb1, 0xd6),
                    rgb(0x56, 0x5f, 0x89),
                    rgb(0xbb, 0x9a, 0xf7),
                    rgb(0x73, 0xda, 0xca),
                    rgb(0x9e, 0xce, 0x6a),
                    rgb(0xe0, 0xaf, 0x68),
                    rgb(0xf7, 0x76, 0x8e),
                ]),
            ),
        }
    }
}

impl std::fmt::Display for ThemeName {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.label())
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Theme {
    pub name: ThemeName,
    pub is_light: bool,
    pub background: Color,
    pub chrome_bg: Color,
    pub surface: Color,
    pub text: Color,
    pub muted: Color,
    pub accent: Color,
    pub accent_alt: Color,
    pub selection_fg: Color,
    pub selection_bg: Color,
    pub success: Color,
    pub warning: Color,
    pub error: Color,
    pub info: Color,
    pub inactive: Color,
    pub directory: Color,
    pub diff_context_bg: Color,
    pub diff_add_bg: Color,
    pub diff_delete_bg: Color,
    pub diff_hunk_bg: Color,
    pub diff_meta_bg: Color,
    pub diff_code_fg: Color,
    pub diff_gutter_fg: Color,
    pub ansi: [Color; 16],
    pub syntax_theme: &'static str,
}

impl Theme {
    pub fn contrast_text(self, background: Color) -> Color {
        let light = rgb(0xf8, 0xf8, 0xf2);
        let dark = rgb(0x1a, 0x1b, 0x26);
        if contrast_ratio(self.text, background) >= 4.5 {
            self.text
        } else if contrast_ratio(light, background) >= contrast_ratio(dark, background) {
            light
        } else {
            dark
        }
    }
}

pub fn fill_area(buffer: &mut Buffer, area: Rect, background: Color) {
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            buffer[(x, y)].bg = background;
        }
    }
}

/// Apply the CST theme to terminal defaults and standard ANSI colors without
/// rewriting the child's higher indexed or truecolor choices.
pub fn apply_terminal_theme(buffer: &mut Buffer, area: Rect, theme: Theme) {
    if theme.name == ThemeName::Classic {
        return;
    }
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            let cell = &mut buffer[(x, y)];
            let original_fg = cell.fg;
            let mapped_bg = map_terminal_color(cell.bg, theme, false);
            let mapped_fg = if original_fg == Color::Reset
                && mapped_bg != theme.background
                && theme.name != ThemeName::Classic
            {
                theme.contrast_text(mapped_bg)
            } else {
                map_terminal_color(original_fg, theme, true)
            };
            cell.fg = mapped_fg;
            cell.bg = mapped_bg;
        }
    }
}

fn map_terminal_color(color: Color, theme: Theme, foreground: bool) -> Color {
    match color {
        Color::Reset if theme.name == ThemeName::Classic => Color::Reset,
        Color::Reset if foreground => theme.text,
        Color::Reset => theme.background,
        Color::Indexed(index @ 0..=15) => theme.ansi[index as usize],
        Color::Black => theme.ansi[0],
        Color::Red => theme.ansi[1],
        Color::Green => theme.ansi[2],
        Color::Yellow => theme.ansi[3],
        Color::Blue => theme.ansi[4],
        Color::Magenta => theme.ansi[5],
        Color::Cyan => theme.ansi[6],
        Color::Gray => theme.ansi[7],
        Color::DarkGray => theme.ansi[8],
        Color::LightRed => theme.ansi[9],
        Color::LightGreen => theme.ansi[10],
        Color::LightYellow => theme.ansi[11],
        Color::LightBlue => theme.ansi[12],
        Color::LightMagenta => theme.ansi[13],
        Color::LightCyan => theme.ansi[14],
        Color::White => theme.ansi[15],
        other => other,
    }
}

#[derive(Clone, Copy)]
struct Palette {
    bg: Color,
    chrome: Color,
    surface: Color,
    text: Color,
    muted: Color,
    accent: Color,
    accent_alt: Color,
    success: Color,
    warning: Color,
    error: Color,
}

impl Palette {
    const fn new(colors: [Color; 10]) -> Self {
        let [bg, chrome, surface, text, muted, accent, accent_alt, success, warning, error] =
            colors;
        Self {
            bg,
            chrome,
            surface,
            text,
            muted,
            accent,
            accent_alt,
            success,
            warning,
            error,
        }
    }
}

fn build(name: ThemeName, p: Palette) -> Theme {
    let ansi = [
        if name.is_light() { p.text } else { p.surface },
        p.error,
        p.success,
        p.warning,
        p.accent,
        p.accent,
        p.accent_alt,
        p.text,
        p.muted,
        p.error,
        p.success,
        p.warning,
        p.accent,
        p.accent,
        p.accent_alt,
        p.text,
    ];
    Theme {
        name,
        is_light: name.is_light(),
        background: p.bg,
        chrome_bg: p.chrome,
        surface: p.surface,
        text: p.text,
        muted: p.muted,
        accent: p.accent,
        accent_alt: p.accent_alt,
        selection_fg: best_contrast_text(p.accent),
        selection_bg: p.accent,
        success: p.success,
        warning: p.warning,
        error: p.error,
        info: p.accent_alt,
        inactive: p.muted,
        directory: p.accent_alt,
        diff_context_bg: p.bg,
        diff_add_bg: blend(p.bg, p.success, if name.is_light() { 18 } else { 25 }),
        diff_delete_bg: blend(p.bg, p.error, if name.is_light() { 16 } else { 25 }),
        diff_hunk_bg: blend(p.bg, p.accent, if name.is_light() { 14 } else { 30 }),
        diff_meta_bg: p.chrome,
        diff_code_fg: p.text,
        diff_gutter_fg: p.muted,
        ansi,
        syntax_theme: if name.is_light() {
            "inspired-github"
        } else {
            "dracula"
        },
    }
}

fn classic() -> Theme {
    Theme {
        name: ThemeName::Classic,
        is_light: false,
        background: Color::Reset,
        chrome_bg: rgb(30, 30, 40),
        surface: Color::Reset,
        text: Color::White,
        muted: Color::DarkGray,
        accent: Color::Magenta,
        accent_alt: Color::Cyan,
        selection_fg: Color::Black,
        selection_bg: Color::Cyan,
        success: Color::Green,
        warning: Color::Yellow,
        error: Color::Red,
        info: Color::Cyan,
        inactive: Color::DarkGray,
        directory: Color::Blue,
        diff_context_bg: rgb(15, 17, 22),
        diff_add_bg: rgb(18, 52, 31),
        diff_delete_bg: rgb(62, 25, 31),
        diff_hunk_bg: rgb(24, 40, 67),
        diff_meta_bg: rgb(31, 31, 42),
        diff_code_fg: rgb(205, 214, 244),
        diff_gutter_fg: rgb(108, 112, 134),
        ansi: [
            Color::Black,
            Color::Red,
            Color::Green,
            Color::Yellow,
            Color::Blue,
            Color::Magenta,
            Color::Cyan,
            Color::Gray,
            Color::DarkGray,
            Color::LightRed,
            Color::LightGreen,
            Color::LightYellow,
            Color::LightBlue,
            Color::LightMagenta,
            Color::LightCyan,
            Color::White,
        ],
        syntax_theme: "dracula",
    }
}

const fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color::Rgb(r, g, b)
}

fn blend(base: Color, tint: Color, tint_percent: u16) -> Color {
    let (br, bg, bb) = rgb_components(base).unwrap_or((0, 0, 0));
    let (tr, tg, tb) = rgb_components(tint).unwrap_or((br, bg, bb));
    let mix = |base: u8, tint: u8| {
        ((u16::from(base) * (100 - tint_percent) + u16::from(tint) * tint_percent) / 100) as u8
    };
    rgb(mix(br, tr), mix(bg, tg), mix(bb, tb))
}

fn contrast_ratio(a: Color, b: Color) -> f64 {
    let luminance = |color| {
        let (r, g, b) = rgb_components(color).unwrap_or((0, 0, 0));
        let channel = |value: u8| {
            let value = f64::from(value) / 255.0;
            if value <= 0.04045 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b)
    };
    let a = luminance(a);
    let b = luminance(b);
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

fn best_contrast_text(background: Color) -> Color {
    let light = rgb(0xf8, 0xf8, 0xf2);
    let dark = rgb(0x1a, 0x1b, 0x26);
    if contrast_ratio(light, background) >= contrast_ratio(dark, background) {
        light
    } else {
        dark
    }
}

fn rgb_components(color: Color) -> Option<(u8, u8, u8)> {
    match color {
        Color::Rgb(r, g, b) => Some((r, g, b)),
        Color::Indexed(index) => Some(xterm_rgb(index)),
        Color::Black => Some((0, 0, 0)),
        Color::Red => Some((205, 49, 49)),
        Color::Green => Some((13, 188, 121)),
        Color::Yellow => Some((229, 229, 16)),
        Color::Blue => Some((36, 114, 200)),
        Color::Magenta => Some((188, 63, 188)),
        Color::Cyan => Some((17, 168, 205)),
        Color::Gray => Some((229, 229, 229)),
        Color::DarkGray => Some((102, 102, 102)),
        Color::LightRed => Some((241, 76, 76)),
        Color::LightGreen => Some((35, 209, 139)),
        Color::LightYellow => Some((245, 245, 67)),
        Color::LightBlue => Some((59, 142, 234)),
        Color::LightMagenta => Some((214, 112, 214)),
        Color::LightCyan => Some((41, 184, 219)),
        Color::White => Some((255, 255, 255)),
        _ => None,
    }
}

fn xterm_rgb(index: u8) -> (u8, u8, u8) {
    match index {
        0..=15 => {
            const ANSI: [(u8, u8, u8); 16] = [
                (0, 0, 0),
                (205, 0, 0),
                (0, 205, 0),
                (205, 205, 0),
                (0, 0, 238),
                (205, 0, 205),
                (0, 205, 205),
                (229, 229, 229),
                (127, 127, 127),
                (255, 0, 0),
                (0, 255, 0),
                (255, 255, 0),
                (92, 92, 255),
                (255, 0, 255),
                (0, 255, 255),
                (255, 255, 255),
            ];
            ANSI[index as usize]
        }
        16..=231 => {
            let value = index - 16;
            let level = |component: u8| {
                if component == 0 {
                    0
                } else {
                    55 + component * 40
                }
            };
            (level(value / 36), level((value % 36) / 6), level(value % 6))
        }
        232..=255 => {
            let gray = 8 + (index - 232) * 10;
            (gray, gray, gray)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn theme_names_are_stable_unique_and_grouped() {
        assert_eq!(ThemeName::ALL.len(), 10);
        assert_eq!(
            ThemeName::ALL.iter().copied().collect::<HashSet<_>>().len(),
            ThemeName::ALL.len()
        );
        assert!(ThemeName::DARK.iter().all(|name| !name.is_light()));
        assert!(ThemeName::LIGHT.iter().all(|name| name.is_light()));
    }

    #[test]
    fn serde_defaults_to_classic_and_uses_kebab_case() {
        assert_eq!(ThemeName::default(), ThemeName::Classic);
        assert_eq!(
            serde_json::to_string(&ThemeName::CatppuccinMocha).unwrap(),
            r#""catppuccin-mocha""#
        );
        assert_eq!(
            serde_json::from_str::<ThemeName>(r#""solarized-light""#).unwrap(),
            ThemeName::SolarizedLight
        );
    }

    #[test]
    fn primary_text_has_readable_contrast_in_rgb_themes() {
        for name in ThemeName::ALL
            .into_iter()
            .filter(|name| *name != ThemeName::Classic)
        {
            let theme = name.theme();
            assert!(
                contrast_ratio(theme.text, theme.background) >= 4.5,
                "{} text contrast is {}",
                name.label(),
                contrast_ratio(theme.text, theme.background)
            );
            assert_eq!(theme.ansi.len(), 16);
            assert!(
                contrast_ratio(theme.selection_fg, theme.selection_bg) >= 3.0,
                "{} selection contrast is {}",
                name.label(),
                contrast_ratio(theme.selection_fg, theme.selection_bg)
            );
            if theme.is_light {
                assert!(
                    contrast_ratio(theme.muted, theme.surface) >= 4.5,
                    "{} muted surface contrast is {}",
                    name.label(),
                    contrast_ratio(theme.muted, theme.surface)
                );
            }
            assert!(
                edtui::THEME_SET.themes.contains_key(theme.syntax_theme),
                "{} syntax theme {:?} is unavailable",
                name.label(),
                theme.syntax_theme
            );
        }
    }

    #[test]
    fn light_themes_choose_dark_default_text() {
        for name in ThemeName::LIGHT {
            let theme = name.theme();
            assert!(theme.is_light);
            let Color::Rgb(r, g, b) = theme.text else {
                panic!("light theme text must be RGB");
            };
            assert!(u16::from(r) + u16::from(g) + u16::from(b) < 384);
        }
    }

    #[test]
    fn named_themes_report_appearance_but_classic_inherits_the_terminal() {
        assert_eq!(ThemeName::Classic.terminal_light_mode(), None);
        assert_eq!(ThemeName::CatppuccinLatte.terminal_light_mode(), Some(true));
        assert_eq!(ThemeName::SolarizedLight.terminal_light_mode(), Some(true));
        assert_eq!(ThemeName::Gruvbox.terminal_light_mode(), Some(false));
    }

    #[test]
    fn terminal_mapping_preserves_modifiers_high_colors_and_rgb() {
        let theme = ThemeName::CatppuccinLatte.theme();
        let mut buffer = Buffer::empty(Rect::new(0, 0, 4, 1));
        buffer[(0, 0)].set_symbol("A").set_style(
            ratatui::style::Style::default()
                .fg(Color::Reset)
                .bg(Color::Reset)
                .add_modifier(Modifier::BOLD | Modifier::ITALIC),
        );
        buffer[(1, 0)].set_symbol("B").set_style(
            ratatui::style::Style::default()
                .fg(Color::Indexed(1))
                .bg(Color::Indexed(2))
                .add_modifier(Modifier::UNDERLINED),
        );
        buffer[(2, 0)].set_style(
            ratatui::style::Style::default()
                .fg(Color::Indexed(42))
                .bg(Color::Indexed(200)),
        );
        buffer[(3, 0)].set_style(
            ratatui::style::Style::default()
                .fg(rgb(1, 2, 3))
                .bg(rgb(4, 5, 6)),
        );

        let area = buffer.area;
        apply_terminal_theme(&mut buffer, area, theme);

        assert_eq!(buffer[(0, 0)].fg, theme.text);
        assert_eq!(buffer[(0, 0)].bg, theme.background);
        assert!(buffer[(0, 0)].modifier.contains(Modifier::BOLD));
        assert!(buffer[(0, 0)].modifier.contains(Modifier::ITALIC));
        assert_eq!(buffer[(1, 0)].fg, theme.ansi[1]);
        assert_eq!(buffer[(1, 0)].bg, theme.ansi[2]);
        assert!(buffer[(1, 0)].modifier.contains(Modifier::UNDERLINED));
        assert_eq!(buffer[(2, 0)].fg, Color::Indexed(42));
        assert_eq!(buffer[(2, 0)].bg, Color::Indexed(200));
        assert_eq!(buffer[(3, 0)].fg, rgb(1, 2, 3));
        assert_eq!(buffer[(3, 0)].bg, rgb(4, 5, 6));
    }

    #[test]
    fn default_text_stays_readable_on_an_explicit_dark_code_background() {
        let theme = ThemeName::SolarizedLight.theme();
        let code_background = rgb(18, 52, 31);
        let mut buffer = Buffer::empty(Rect::new(0, 0, 1, 1));
        buffer[(0, 0)].set_style(
            ratatui::style::Style::default()
                .fg(Color::Reset)
                .bg(code_background),
        );

        let area = buffer.area;
        apply_terminal_theme(&mut buffer, area, theme);

        assert_eq!(buffer[(0, 0)].bg, code_background);
        assert!(contrast_ratio(buffer[(0, 0)].fg, code_background) >= 4.5);
    }

    #[test]
    fn default_text_uses_actual_high_index_background_for_contrast() {
        let theme = ThemeName::SolarizedLight.theme();
        let mut buffer = Buffer::empty(Rect::new(0, 0, 1, 1));
        buffer[(0, 0)].set_style(
            ratatui::style::Style::default()
                .fg(Color::Reset)
                .bg(Color::Indexed(42)),
        );

        apply_terminal_theme(&mut buffer, Rect::new(0, 0, 1, 1), theme);

        assert_eq!(buffer[(0, 0)].bg, Color::Indexed(42));
        assert!(contrast_ratio(buffer[(0, 0)].fg, Color::Indexed(42)) >= 4.5);
    }

    #[test]
    fn reverse_video_keeps_the_mapped_pair_readable() {
        let theme = ThemeName::SolarizedLight.theme();
        let background = rgb(18, 52, 31);
        let mut buffer = Buffer::empty(Rect::new(0, 0, 1, 1));
        buffer[(0, 0)].set_style(
            ratatui::style::Style::default()
                .fg(Color::Reset)
                .bg(background)
                .add_modifier(Modifier::REVERSED),
        );

        apply_terminal_theme(&mut buffer, Rect::new(0, 0, 1, 1), theme);

        assert!(buffer[(0, 0)].modifier.contains(Modifier::REVERSED));
        assert!(contrast_ratio(buffer[(0, 0)].fg, buffer[(0, 0)].bg) >= 4.5);
    }

    #[test]
    fn classic_leaves_terminal_defaults_untouched() {
        let mut buffer = Buffer::empty(Rect::new(0, 0, 1, 1));
        apply_terminal_theme(
            &mut buffer,
            Rect::new(0, 0, 1, 1),
            ThemeName::Classic.theme(),
        );
        assert_eq!(buffer[(0, 0)].fg, Color::Reset);
        assert_eq!(buffer[(0, 0)].bg, Color::Reset);
    }

    #[test]
    fn named_empty_cell_cursor_color_uses_the_theme_ansi_palette() {
        let theme = ThemeName::CatppuccinLatte.theme();
        let mut buffer = Buffer::empty(Rect::new(0, 0, 1, 1));
        buffer[(0, 0)].fg = Color::Gray;

        apply_terminal_theme(&mut buffer, Rect::new(0, 0, 1, 1), theme);

        assert_eq!(buffer[(0, 0)].fg, theme.ansi[7]);
        assert_eq!(buffer[(0, 0)].bg, theme.background);
    }

    #[test]
    fn terminal_mapping_stays_below_a_frame_budget_at_large_pane_size() {
        let area = Rect::new(0, 0, 240, 70);
        let mut buffer = Buffer::empty(area);
        let started = std::time::Instant::now();
        for _ in 0..100 {
            apply_terminal_theme(&mut buffer, area, ThemeName::CatppuccinLatte.theme());
        }
        assert!(
            started.elapsed() < std::time::Duration::from_secs(1),
            "100 visible-cell passes took {:?}",
            started.elapsed()
        );
    }
}
