use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;
#[cfg(test)]
use ratatui::style::Modifier;
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

/// A theme, by the name config.json knows it by.
///
/// The colours are not here: each named theme is a file in `src/themes`, read by
/// [`ThemeFile`]. Adding one is a file, a variant, and a line in each of
/// [`ThemeName::ALL`] and [`FILES`]; a test fails until all four agree.
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
    NuDisco,
    OneDark,
    RosePine,
    RosePineDawn,
    #[serde(rename = "synthwave-84")]
    Synthwave84,
    EverforestDark,
    Kanagawa,
    GruvboxLight,
    GithubLight,
    TokyoNightDay,
}

/// The named themes' files, compiled in.
const FILES: [(ThemeName, &str); 19] = [
    (ThemeName::Gruvbox, include_str!("themes/gruvbox.json")),
    (ThemeName::Nord, include_str!("themes/nord.json")),
    (ThemeName::Dracula, include_str!("themes/dracula.json")),
    (
        ThemeName::CatppuccinMocha,
        include_str!("themes/catppuccin-mocha.json"),
    ),
    (ThemeName::Palenight, include_str!("themes/palenight.json")),
    (
        ThemeName::SolarizedDark,
        include_str!("themes/solarized-dark.json"),
    ),
    (
        ThemeName::TokyoNight,
        include_str!("themes/tokyo-night.json"),
    ),
    (ThemeName::NuDisco, include_str!("themes/nu-disco.json")),
    (ThemeName::OneDark, include_str!("themes/one-dark.json")),
    (ThemeName::RosePine, include_str!("themes/rose-pine.json")),
    (
        ThemeName::Synthwave84,
        include_str!("themes/synthwave-84.json"),
    ),
    (
        ThemeName::EverforestDark,
        include_str!("themes/everforest-dark.json"),
    ),
    (ThemeName::Kanagawa, include_str!("themes/kanagawa.json")),
    (
        ThemeName::CatppuccinLatte,
        include_str!("themes/catppuccin-latte.json"),
    ),
    (
        ThemeName::SolarizedLight,
        include_str!("themes/solarized-light.json"),
    ),
    (
        ThemeName::GruvboxLight,
        include_str!("themes/gruvbox-light.json"),
    ),
    (
        ThemeName::GithubLight,
        include_str!("themes/github-light.json"),
    ),
    (
        ThemeName::RosePineDawn,
        include_str!("themes/rose-pine-dawn.json"),
    ),
    (
        ThemeName::TokyoNightDay,
        include_str!("themes/tokyo-night-day.json"),
    ),
];

/// A theme as its file gives it: ten colours with a part to play each, and the
/// sixteen a terminal program can ask for by number.
///
/// This is the whole of what a theme's author writes. Everything else a [`Theme`]
/// holds is worked out from these by [`build`], which also moves a colour where two
/// of them could not be told apart, so a file cannot make CST unreadable.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThemeFile {
    /// The title shown in the picker.
    pub name: String,
    pub appearance: Appearance,
    /// Where the colours were taken from.
    #[cfg_attr(not(test), allow(dead_code))]
    #[serde(default)]
    pub source: Vec<String>,
    /// What was changed from the source, and why. For whoever reads the file; CST
    /// only checks it is text.
    #[allow(dead_code)]
    #[serde(default)]
    pub note: String,
    pub colors: FileColors,
    /// Black, red, green, yellow, blue, magenta, cyan, white, then the bright
    /// eight. Left out, they are filled from the ten.
    #[serde(default)]
    pub ansi: Option<[Hex; 16]>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Appearance {
    Dark,
    Light,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileColors {
    /// The canvas.
    pub background: Hex,
    /// Bars and popups: a tone slightly apart from the canvas.
    pub chrome: Hex,
    /// Raised panels: a step further from the canvas.
    pub surface: Hex,
    pub text: Hex,
    /// Secondary text.
    pub muted: Hex,
    /// The colour the scheme is known by: the selection bar, a focused border.
    pub accent: Hex,
    /// A second accent: information, folders, links.
    pub accent_alt: Hex,
    pub success: Hex,
    pub warning: Hex,
    pub error: Hex,
}

/// A colour written `#rrggbb`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hex(pub Color);

impl<'de> Deserialize<'de> for Hex {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        let digits = text
            .strip_prefix('#')
            .filter(|digits| digits.len() == 6 && digits.is_ascii())
            .ok_or_else(|| serde::de::Error::custom(format!("{text:?} is not #rrggbb")))?;
        let channel = |at: usize| {
            u8::from_str_radix(&digits[at..at + 2], 16)
                .map_err(|_| serde::de::Error::custom(format!("{text:?} is not #rrggbb")))
        };
        Ok(Self(Color::Rgb(channel(0)?, channel(2)?, channel(4)?)))
    }
}

/// The compiled-in files, read once. A file that does not read is a build that
/// should not have shipped, and a test reads every one, so this does not fail in
/// a released binary.
fn files() -> &'static [(ThemeName, ThemeFile)] {
    static READ_FILES: OnceLock<Vec<(ThemeName, ThemeFile)>> = OnceLock::new();
    READ_FILES.get_or_init(|| {
        FILES
            .iter()
            .filter_map(|(name, text)| Some((*name, serde_json::from_str(text).ok()?)))
            .collect()
    })
}

impl ThemeName {
    /// Every theme in the order the picker lists them: Classic, the dark ones, the
    /// light ones.
    pub const ALL: [Self; 20] = [
        Self::Classic,
        Self::Gruvbox,
        Self::Nord,
        Self::Dracula,
        Self::CatppuccinMocha,
        Self::Palenight,
        Self::SolarizedDark,
        Self::TokyoNight,
        Self::NuDisco,
        Self::OneDark,
        Self::RosePine,
        Self::Synthwave84,
        Self::EverforestDark,
        Self::Kanagawa,
        Self::CatppuccinLatte,
        Self::SolarizedLight,
        Self::GruvboxLight,
        Self::GithubLight,
        Self::RosePineDawn,
        Self::TokyoNightDay,
    ];

    pub fn dark() -> Vec<Self> {
        Self::ALL
            .into_iter()
            .filter(|name| name.terminal_light_mode() == Some(false))
            .collect()
    }

    pub fn light() -> Vec<Self> {
        Self::ALL
            .into_iter()
            .filter(|name| name.is_light())
            .collect()
    }

    fn file(self) -> Option<&'static ThemeFile> {
        files()
            .iter()
            .find(|(name, _)| *name == self)
            .map(|(_, file)| file)
    }

    pub fn label(self) -> &'static str {
        match self.file() {
            Some(file) => &file.name,
            None => "CST Classic",
        }
    }

    /// The name as config.json spells it.
    pub fn id(self) -> String {
        serde_json::to_value(self)
            .ok()
            .and_then(|value| value.as_str().map(str::to_string))
            .unwrap_or_default()
    }

    /// What the theme reports itself as. Classic takes on the terminal's own
    /// colours and so is neither.
    pub fn appearance(self) -> &'static str {
        match self.terminal_light_mode() {
            Some(true) => "light",
            Some(false) => "dark",
            None => "terminal",
        }
    }

    pub fn is_light(self) -> bool {
        self.file()
            .is_some_and(|file| file.appearance == Appearance::Light)
    }

    pub fn terminal_light_mode(self) -> Option<bool> {
        self.file().map(|file| file.appearance == Appearance::Light)
    }

    /// The theme, built once: asked for on every frame and by every draw function,
    /// while building one checks and corrects dozens of colour pairs.
    pub fn theme(self) -> Theme {
        static BUILT: OnceLock<Vec<Theme>> = OnceLock::new();
        let built = BUILT.get_or_init(|| Self::ALL.iter().map(|name| name.built()).collect());
        let index = Self::ALL
            .iter()
            .position(|name| *name == self)
            .unwrap_or_default();
        built[index]
    }

    fn built(self) -> Theme {
        match self.file() {
            Some(file) => build(self, file),
            None => classic(),
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
    /// Where a program's subtle panel grey lands: see [`panel_tone`].
    pub panel_subtle: Color,
    /// Where its deepest panel grey lands.
    pub panel_raised: Color,
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
                let mapped = map_terminal_color(original_fg, theme, true);
                // A palette colour was fitted to the canvas. On a fill the program
                // chose itself, the dark green of a light theme on a dark diff row,
                // it may not show, and then it gives way like the default text does.
                if mapped != original_fg
                    && mapped_bg != theme.background
                    && contrast_ratio(mapped, mapped_bg) < SEEN
                {
                    moved_until(mapped, theme.contrast_text(mapped_bg), &[(mapped_bg, SEEN)])
                } else {
                    mapped
                }
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
        other => panel_tone(other, theme),
    }
}

/// How far apart a colour's channels may be for it still to count as a grey.
/// Copilot's panel greys lean blue by up to 21; its accents are past 100.
const GREY_CHANNEL_SPREAD: u8 = 28;
/// A grey this far from the canvas end of the scale is a subtle panel: the fill
/// behind a prompt or inline code.
const SUBTLE_PANEL_DEPTH: u16 = 20;
/// Past this a grey is text or an icon, not a fill, and is left as painted.
const DEEPEST_PANEL_DEPTH: u16 = 80;

/// Bring a program's own panel greys into the theme.
///
/// Copilot paints nearly everything in fixed colours of its own, picked for a
/// black or white terminal: the fill behind a prompt, inline code and its input
/// box is a blue-grey whatever the theme. On Solarized that is a grey slab on a
/// teal canvas. Those fills, and the borders drawn in the same tones, are moved
/// onto the theme's own steps away from its canvas, keeping how far each stood
/// from the program's idea of the canvas.
///
/// Only greys near the canvas end are touched. Text greys stay as painted, since
/// a program chose them for contrast and a theme's own text can be dimmer; and
/// anything with a hue stays, which is what keeps a code viewer's syntax colours
/// and a diff's red and green.
fn panel_tone(color: Color, theme: Theme) -> Color {
    let (r, g, b) = match color {
        Color::Rgb(r, g, b) => (r, g, b),
        Color::Indexed(index @ 16..) => xterm_rgb(index),
        _ => return color,
    };
    if r.max(g).max(b) - r.min(g).min(b) > GREY_CHANNEL_SPREAD {
        return color;
    }
    let level = (u16::from(r) + u16::from(g) + u16::from(b)) / 3;
    let depth = if theme.is_light { 255 - level } else { level };
    if depth > DEEPEST_PANEL_DEPTH {
        return color;
    }
    if depth <= SUBTLE_PANEL_DEPTH {
        blend(
            theme.background,
            theme.panel_subtle,
            depth * 100 / SUBTLE_PANEL_DEPTH,
        )
    } else {
        let span = DEEPEST_PANEL_DEPTH - SUBTLE_PANEL_DEPTH;
        blend(
            theme.panel_subtle,
            theme.panel_raised,
            (depth - SUBTLE_PANEL_DEPTH) * 100 / span,
        )
    }
}

/// The colours the real Copilot paints with, read off its screen under a dark and
/// a light CST theme (1.0.112 to 1.0.114). It follows the appearance CST reports
/// and nothing else about the theme, so these are what every theme has to live
/// with. `.claude/skills/theme-review` says how to refresh them.
pub(crate) struct CopilotColours {
    /// The fills behind a prompt, inline code and the input box.
    #[cfg_attr(not(test), allow(dead_code))]
    pub panels: &'static [Color],
    pub text: &'static [Color],
    pub muted: &'static [Color],
}

pub(crate) const COPILOT_ON_DARK: CopilotColours = CopilotColours {
    panels: &[
        rgb(0x14, 0x1b, 0x22),
        rgb(0x23, 0x26, 0x2c),
        rgb(0x26, 0x2c, 0x36),
    ],
    text: &[rgb(0xf0, 0xf6, 0xfc), rgb(0xd3, 0xd9, 0xdf)],
    muted: &[rgb(0x91, 0x98, 0xa1)],
};

pub(crate) const COPILOT_ON_LIGHT: CopilotColours = CopilotColours {
    panels: &[rgb(0xe8, 0xe9, 0xeb)],
    text: &[rgb(0x1f, 0x23, 0x28)],
    muted: &[rgb(0x59, 0x63, 0x6e), rgb(0x6e, 0x77, 0x81)],
};

pub(crate) const fn copilot_colours(is_light: bool) -> CopilotColours {
    if is_light {
        COPILOT_ON_LIGHT
    } else {
        COPILOT_ON_DARK
    }
}

/// Text somebody reads, by the usual 4.5 rule.
pub(crate) const READ: f64 = 4.5;
/// Secondary text, marks and coloured words: findable, not for reading at length.
pub(crate) const SEEN: f64 = 3.0;

/// Move `colour` towards `target`, no further than it takes for every pair in
/// `against` to reach its contrast, where `colour` is one side of each pair.
///
/// This is how a palette copied faithfully from its author is made to work in
/// places the author never put it: a comment grey on a raised panel, a pastel
/// yellow as text on a light canvas. A colour that already works is returned
/// untouched, so a palette shows its own colours wherever they can be told apart.
fn moved_until(colour: Color, target: Color, against: &[(Color, f64)]) -> Color {
    const STEPS: u16 = 20;
    for step in 0..=STEPS {
        let candidate = blend(colour, target, step * 100 / STEPS);
        if against
            .iter()
            .all(|(other, least)| contrast_ratio(candidate, *other) >= *least)
        {
            return candidate;
        }
    }
    target
}

/// The ten colours of a file, unwrapped.
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

fn build(name: ThemeName, file: &ThemeFile) -> Theme {
    let is_light = file.appearance == Appearance::Light;
    let colors = file.colors;
    // The main text is what every other colour is moved towards, so it is settled
    // first: Solarized's own text on its own light canvas is under the line.
    let strongest = if is_light {
        rgb(0, 0, 0)
    } else {
        rgb(255, 255, 255)
    };
    let p = Palette {
        bg: colors.background.0,
        chrome: colors.chrome.0,
        surface: colors.surface.0,
        text: moved_until(colors.text.0, strongest, &[(colors.background.0, READ)]),
        muted: colors.muted.0,
        accent: colors.accent.0,
        accent_alt: colors.accent_alt.0,
        success: colors.success.0,
        warning: colors.warning.0,
        error: colors.error.0,
    };
    // Fills give way to the text on them: a panel too close to the text goes back
    // towards the canvas, where the text is known to read.
    let chrome = moved_until(p.chrome, p.bg, &[(p.text, READ)]);
    let surface = moved_until(p.surface, p.bg, &[(p.text, READ)]);
    // Everything drawn as text gives way to what it is drawn on, towards the main
    // text colour.
    let on_canvas = [(p.bg, SEEN)];
    let secondary = secondary_contrast(is_light);
    let muted = moved_until(
        p.muted,
        p.text,
        &[(p.bg, secondary), (chrome, secondary), (surface, secondary)],
    );
    let accent = moved_until(p.accent, p.text, &on_canvas);
    let accent_alt = moved_until(p.accent_alt, p.text, &on_canvas);
    let success = moved_until(p.success, p.text, &on_canvas);
    let warning = moved_until(p.warning, p.text, &on_canvas);
    let error = moved_until(p.error, p.text, &on_canvas);

    let diff_row = |tint: Color, percent: u16| {
        moved_until(blend(p.bg, tint, percent), p.bg, &[(p.text, READ)])
    };
    let diff_add_bg = diff_row(success, if is_light { 18 } else { 25 });
    let diff_delete_bg = diff_row(error, if is_light { 16 } else { 25 });
    let diff_hunk_bg = diff_row(accent, if is_light { 14 } else { 30 });
    let diff_gutter_fg = moved_until(
        muted,
        p.text,
        &[
            (diff_add_bg, SEEN),
            (diff_delete_bg, SEEN),
            (diff_hunk_bg, SEEN),
            (chrome, SEEN),
        ],
    );

    // The tones a program's own panel greys are moved onto. What Copilot writes on
    // its panels is fixed, so the panels give way: back towards the canvas until
    // its text and its secondary text can both be read on them.
    let copilot = copilot_colours(is_light);
    let copilot_writes: Vec<(Color, f64)> = copilot
        .text
        .iter()
        .map(|colour| (*colour, READ))
        .chain(copilot.muted.iter().map(|colour| (*colour, SEEN)))
        .collect();
    let panel_subtle = moved_until(surface, p.bg, &copilot_writes);
    let panel_raised = moved_until(blend(surface, muted, 50), p.bg, &copilot_writes);

    // A file's sixteen are its scheme's own terminal colours. Without them the ten
    // fill in, which leaves blue and magenta the same colour and every bright one
    // equal to its plain twin.
    let filled_in = [
        if is_light { p.text } else { surface },
        error,
        success,
        warning,
        accent,
        accent,
        accent_alt,
        p.text,
        muted,
        error,
        success,
        warning,
        accent,
        accent,
        accent_alt,
        p.text,
    ];
    // The selection bar is the accent, with whichever of light and dark text reads
    // better on it. A mid-tone accent suits neither, and then the bar gives way.
    let selection_fg = best_contrast_text(accent);
    let away_from_its_text = if contrast_ratio(selection_fg, rgb(0, 0, 0))
        > contrast_ratio(selection_fg, rgb(255, 255, 255))
    {
        rgb(0, 0, 0)
    } else {
        rgb(255, 255, 255)
    };
    let selection_bg = moved_until(accent, away_from_its_text, &[(selection_fg, READ)]);
    let ansi = match file.ansi {
        Some(given) => {
            let mut fitted = [Color::Reset; 16];
            for (index, colour) in given.iter().enumerate() {
                fitted[index] = if stands_for_canvas(index, is_light) {
                    colour.0
                } else {
                    moved_until(colour.0, p.text, &[(p.bg, ansi_contrast(index, is_light))])
                };
            }
            fitted
        }
        None => filled_in,
    };
    Theme {
        name,
        is_light,
        background: p.bg,
        chrome_bg: chrome,
        surface,
        text: p.text,
        muted,
        accent,
        accent_alt,
        selection_fg,
        selection_bg,
        success,
        warning,
        error,
        info: accent_alt,
        inactive: muted,
        directory: accent_alt,
        diff_context_bg: p.bg,
        diff_add_bg,
        diff_delete_bg,
        diff_hunk_bg,
        diff_meta_bg: chrome,
        diff_code_fg: p.text,
        diff_gutter_fg,
        panel_subtle,
        panel_raised,
        ansi,
        syntax_theme: if is_light {
            "inspired-github"
        } else {
            "dracula"
        },
    }
}

/// The palette colour that is the canvas itself, which nothing is asked of as text:
/// black on a dark theme, the two whites on a light one.
pub(crate) fn stands_for_canvas(index: usize, is_light: bool) -> bool {
    if is_light {
        matches!(index, 7 | 15)
    } else {
        index == 0
    }
}

/// How well secondary text must show. A light theme is what somebody switches to
/// in daylight, where the 3.0 that does for a dim room washes out.
pub(crate) fn secondary_contrast(is_light: bool) -> f64 {
    if is_light {
        READ
    } else {
        SEEN
    }
}

/// How well a palette colour must show as text on the canvas. White on a dark
/// theme is what a program writes its main text in.
pub(crate) fn ansi_contrast(index: usize, is_light: bool) -> f64 {
    if !is_light && matches!(index, 7 | 15) {
        READ
    } else {
        SEEN
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
        panel_subtle: Color::Reset,
        panel_raised: Color::Reset,
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
        assert_eq!(
            ThemeName::ALL.iter().copied().collect::<HashSet<_>>().len(),
            ThemeName::ALL.len()
        );
        // The picker walks ALL top to bottom and draws it as three groups, so the
        // order has to be the groups' order: Classic, the dark ones, the light ones.
        let mut grouped = vec![ThemeName::Classic];
        grouped.extend(ThemeName::dark());
        grouped.extend(ThemeName::light());
        assert_eq!(grouped, ThemeName::ALL);
    }

    #[test]
    fn every_named_theme_has_a_file_that_reads_and_is_named_as_config_spells_it() {
        // A theme is a variant, a line in ALL, a line in FILES and a file. Any one
        // forgotten would otherwise show as a theme silently drawn as Classic.
        let named: Vec<ThemeName> = ThemeName::ALL
            .into_iter()
            .filter(|name| *name != ThemeName::Classic)
            .collect();
        let with_files: Vec<ThemeName> = FILES.iter().map(|(name, _)| *name).collect();
        assert_eq!(with_files, named, "FILES lists the themes in ALL's order");
        let mut titles = HashSet::new();
        for (name, text) in FILES {
            let file: ThemeFile =
                serde_json::from_str(text).unwrap_or_else(|error| panic!("{}: {error}", name.id()));
            assert!(titles.insert(file.name.clone()), "{} twice", file.name);
            assert!(
                !file.source.is_empty(),
                "{} says where its colours came from",
                name.id()
            );
            assert!(file.ansi.is_some(), "{} gives its sixteen", name.id());
        }
        // The file is found by the name config.json uses, so the two must agree.
        for name in named {
            let path = format!("src/themes/{}.json", name.id());
            assert!(std::path::Path::new(&path).exists(), "{path}");
            assert_eq!(
                serde_json::from_str::<ThemeName>(&format!("\"{}\"", name.id())).unwrap(),
                name
            );
        }
    }

    #[test]
    fn a_theme_file_is_refused_for_a_colour_that_is_not_one_or_a_key_it_does_not_know() {
        let good = FILES[0].1;
        assert!(serde_json::from_str::<ThemeFile>(good).is_ok());
        let bad_colour = good.replacen("\"#282828\"", "\"282828\"", 1);
        assert!(serde_json::from_str::<ThemeFile>(&bad_colour).is_err());
        let unknown_key = good.replacen("\"colors\"", "\"colours\"", 1);
        assert!(serde_json::from_str::<ThemeFile>(&unknown_key).is_err());
        let short_palette = good.replacen("\"#282828\", ", "", 1);
        assert!(serde_json::from_str::<ThemeFile>(&short_palette).is_err());
    }

    #[test]
    fn a_file_with_its_own_sixteen_keeps_blue_and_magenta_apart() {
        // With ten colours to fill sixteen, blue and magenta were both the accent
        // and every bright colour its plain twin, in every theme.
        for theme in named_themes() {
            assert_ne!(theme.ansi[4], theme.ansi[5], "{}", theme.name.label());
        }
        let gruvbox = ThemeName::Gruvbox.theme();
        assert_ne!(gruvbox.ansi[1], gruvbox.ansi[9]);
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

    /// One pair of colours that meet on screen, and how far apart they must be.
    struct Pair {
        what: String,
        ratio: f64,
        least: f64,
    }

    /// Every pair of colours a theme puts together, in CST's own screens and in
    /// what a program in a pane paints through it.
    fn pairs(theme: Theme) -> Vec<Pair> {
        let mut pairs = Vec::new();
        let mut pair = |what: String, front: Color, back: Color, least: f64| {
            pairs.push(Pair {
                what,
                ratio: contrast_ratio(front, back),
                least,
            });
        };
        for (name, back) in [
            ("background", theme.background),
            ("chrome", theme.chrome_bg),
            ("surface", theme.surface),
        ] {
            pair(format!("text on {name}"), theme.text, back, READ);
            pair(
                format!("muted on {name}"),
                theme.muted,
                back,
                secondary_contrast(theme.is_light),
            );
        }
        pair(
            "selection".to_string(),
            theme.selection_fg,
            theme.selection_bg,
            READ,
        );
        for (name, front) in [
            ("accent", theme.accent),
            ("accent_alt", theme.accent_alt),
            ("success", theme.success),
            ("warning", theme.warning),
            ("error", theme.error),
            ("info", theme.info),
            ("directory", theme.directory),
            ("inactive", theme.inactive),
        ] {
            pair(
                format!("{name} on background"),
                front,
                theme.background,
                SEEN,
            );
        }
        for (name, back) in [
            ("added", theme.diff_add_bg),
            ("deleted", theme.diff_delete_bg),
            ("hunk", theme.diff_hunk_bg),
            ("meta", theme.diff_meta_bg),
        ] {
            pair(
                format!("diff code on {name}"),
                theme.diff_code_fg,
                back,
                READ,
            );
            pair(
                format!("diff gutter on {name}"),
                theme.diff_gutter_fg,
                back,
                SEEN,
            );
        }
        // The sixteen colours a program has, as text on the canvas. The one that
        // stands for the canvas itself is left out: black on a dark theme, and the
        // two whites on a light one where the palette keeps them light.
        for (index, colour) in theme.ansi.iter().enumerate() {
            if stands_for_canvas(index, theme.is_light) {
                continue;
            }
            let least = ansi_contrast(index, theme.is_light);
            pair(
                format!("ansi {index} on background"),
                *colour,
                theme.background,
                least,
            );
        }
        // What Copilot paints as text, on the canvas and on its panels once those
        // are moved into the theme.
        let copilot = if theme.is_light {
            COPILOT_ON_LIGHT
        } else {
            COPILOT_ON_DARK
        };
        let hex = |colour: Color| {
            let (r, g, b) = rgb_components(colour).unwrap_or_default();
            format!("#{r:02x}{g:02x}{b:02x}")
        };
        let mut backs = vec![("canvas".to_string(), theme.background)];
        for panel in copilot.panels {
            backs.push((
                format!("its panel {}", hex(*panel)),
                map_terminal_color(*panel, theme, false),
            ));
        }
        for (name, back) in backs {
            for text in copilot.text {
                pair(
                    format!("Copilot text {} on {name}", hex(*text)),
                    *text,
                    back,
                    READ,
                );
            }
            for muted in copilot.muted {
                pair(
                    format!("Copilot muted {} on {name}", hex(*muted)),
                    *muted,
                    back,
                    SEEN,
                );
            }
        }
        pairs
    }

    fn named_themes() -> impl Iterator<Item = Theme> {
        ThemeName::ALL
            .into_iter()
            .filter(|name| *name != ThemeName::Classic)
            .map(ThemeName::theme)
    }

    #[test]
    fn every_pair_of_colours_a_theme_puts_together_can_be_told_apart() {
        // A palette copied faithfully from its author can still put two of its
        // colours together in a way the author never did. This is the half of the
        // theme review a machine can do; the pictures are the other half.
        let mut failures = Vec::new();
        for theme in named_themes() {
            for pair in pairs(theme) {
                if pair.ratio < pair.least {
                    failures.push(format!(
                        "{}: {} is {:.2}, needs {:.1}",
                        theme.name.label(),
                        pair.what,
                        pair.ratio,
                        pair.least
                    ));
                }
            }
        }
        assert!(failures.is_empty(), "\n{}", failures.join("\n"));
    }

    #[test]
    fn panel_greys_take_the_tones_of_the_theme_and_hues_are_left_alone() {
        for theme in named_themes() {
            let copilot = if theme.is_light {
                COPILOT_ON_LIGHT
            } else {
                COPILOT_ON_DARK
            };
            for panel in copilot.panels {
                let mapped = map_terminal_color(*panel, theme, false);
                assert_ne!(mapped, *panel, "{}", theme.name.label());
                assert_eq!(
                    mapped,
                    map_terminal_color(*panel, theme, true),
                    "a box edge drawn in the colour of the panel has to match its fill"
                );
            }
            // Text greys and anything with a hue pass through.
            for kept in copilot.text.iter().chain(copilot.muted) {
                assert_eq!(map_terminal_color(*kept, theme, true), *kept);
            }
            for kept in [rgb(0x09, 0x69, 0xda), rgb(18, 52, 31), Color::Indexed(42)] {
                assert_eq!(map_terminal_color(kept, theme, false), kept);
            }
        }
        // The canvas a program assumes becomes the canvas of the theme.
        let dark = ThemeName::SolarizedDark.theme();
        assert_eq!(
            map_terminal_color(rgb(0, 0, 0), dark, false),
            dark.background
        );
        assert_eq!(
            map_terminal_color(Color::Indexed(232), dark, false),
            blend(dark.background, dark.panel_subtle, 40)
        );
    }

    #[test]
    #[ignore = "prints the colour pairs of every theme with their contrast, for the theme review"]
    fn print_the_theme_audit() {
        for theme in named_themes() {
            println!(
                "\n== {} ({})",
                theme.name.label(),
                if theme.is_light { "light" } else { "dark" }
            );
            for pair in pairs(theme) {
                println!(
                    "{} {:>5.2} (needs {:.1})  {}",
                    if pair.ratio < pair.least {
                        "FAIL"
                    } else {
                        "  ok"
                    },
                    pair.ratio,
                    pair.least,
                    pair.what
                );
            }
        }
    }

    #[test]
    fn light_themes_choose_dark_default_text() {
        for name in ThemeName::light() {
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
        // Red on green: kept where the pair shows, moved where it would not.
        assert!(contrast_ratio(buffer[(1, 0)].fg, theme.ansi[2]) >= SEEN);
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
