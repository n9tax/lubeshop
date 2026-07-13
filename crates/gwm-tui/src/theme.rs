//! Colour themes. Each theme is a small palette the renderer reads; the active
//! theme name is persisted in settings and resolved to a [`Theme`] here.

use ratatui::style::Color;

#[derive(Debug, Clone, Copy)]
pub struct Theme {
    pub name: &'static str,
    pub desc: &'static str,
    pub bg: Color,
    pub text: Color,
    pub dim: Color,
    pub border: Color,
    pub accent: Color,
    pub hl_fg: Color,
    pub hl_bg: Color,
    pub success: Color,
    pub warning: Color,
    pub danger: Color,
}

/// Terminal-native dark theme (respects the terminal's own background).
pub const DARK: Theme = Theme {
    name: "dark",
    desc: "terminal-native dark",
    bg: Color::Reset,
    text: Color::Gray,
    dim: Color::DarkGray,
    border: Color::Gray,
    accent: Color::Cyan,
    hl_fg: Color::Black,
    hl_bg: Color::Cyan,
    success: Color::Green,
    warning: Color::Yellow,
    danger: Color::Red,
};

pub const LIGHT: Theme = Theme {
    name: "light",
    desc: "light paper",
    bg: Color::Rgb(240, 240, 240),
    text: Color::Rgb(30, 30, 30),
    dim: Color::Rgb(110, 110, 110),
    border: Color::Rgb(160, 160, 160),
    accent: Color::Rgb(0, 90, 180),
    hl_fg: Color::Rgb(255, 255, 255),
    hl_bg: Color::Rgb(0, 90, 180),
    success: Color::Rgb(20, 120, 40),
    warning: Color::Rgb(150, 100, 0),
    danger: Color::Rgb(180, 30, 30),
};

/// Classic Turbo/Borland IDE: blue field, yellow accents, cyan borders.
pub const BORLAND: Theme = Theme {
    name: "borland",
    desc: "Turbo blue & yellow",
    bg: Color::Rgb(0, 0, 168),
    text: Color::Rgb(238, 238, 238),
    dim: Color::Rgb(120, 160, 225),
    border: Color::Rgb(0, 255, 255),
    accent: Color::Rgb(255, 255, 85),
    hl_fg: Color::Rgb(0, 0, 0),
    hl_bg: Color::Rgb(0, 170, 170),
    success: Color::Rgb(85, 255, 85),
    warning: Color::Rgb(255, 255, 85),
    danger: Color::Rgb(255, 85, 85),
};

/// Commodore 64 boot screen (Pepto palette): blue field, light-blue text.
pub const C64: Theme = Theme {
    name: "c64",
    desc: "Commodore 64 blues",
    bg: Color::Rgb(53, 40, 121),      // Pepto blue (6)
    text: Color::Rgb(108, 94, 181),   // Pepto light blue (14)
    dim: Color::Rgb(84, 72, 150),
    border: Color::Rgb(108, 94, 181),
    accent: Color::Rgb(149, 149, 149), // Pepto light grey (15), reads as the classic near-white
    hl_fg: Color::Rgb(53, 40, 121),
    hl_bg: Color::Rgb(108, 94, 181),
    success: Color::Rgb(154, 210, 132), // Pepto light green (13)
    warning: Color::Rgb(184, 199, 111), // Pepto yellow (7)
    danger: Color::Rgb(154, 103, 89),   // Pepto light red (10)
};

/// VIC-20: blue field with a bright cyan border.
pub const VIC20: Theme = Theme {
    name: "vic20",
    desc: "VIC-20 cyan on blue",
    bg: Color::Rgb(28, 40, 175),
    text: Color::Rgb(238, 238, 238),
    dim: Color::Rgb(150, 170, 235),
    border: Color::Rgb(120, 220, 220),
    accent: Color::Rgb(120, 220, 220),
    hl_fg: Color::Rgb(0, 0, 0),
    hl_bg: Color::Rgb(120, 220, 220),
    success: Color::Rgb(120, 230, 120),
    warning: Color::Rgb(235, 225, 120),
    danger: Color::Rgb(235, 120, 120),
};

pub const THEMES: &[Theme] = &[DARK, LIGHT, BORLAND, C64, VIC20];

/// Resolve a theme name to its palette, falling back to [`DARK`].
pub fn by_name(name: &str) -> Theme {
    THEMES
        .iter()
        .copied()
        .find(|t| t.name == name)
        .unwrap_or(DARK)
}

/// The theme `delta` steps from `name` (wrapping), for cycling in the UI.
pub fn cycle(name: &str, delta: isize) -> Theme {
    let current = THEMES.iter().position(|t| t.name == name).unwrap_or(0) as isize;
    let len = THEMES.len() as isize;
    let next = (current + delta).rem_euclid(len) as usize;
    THEMES[next]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_name_falls_back_to_dark() {
        assert_eq!(by_name("nope").name, "dark");
        assert_eq!(by_name("c64").name, "c64");
    }

    #[test]
    fn cycle_wraps_both_ways() {
        assert_eq!(cycle("dark", -1).name, THEMES[THEMES.len() - 1].name);
        assert_eq!(cycle(THEMES[THEMES.len() - 1].name, 1).name, "dark");
    }

    #[test]
    fn every_theme_name_resolves() {
        for t in THEMES {
            assert_eq!(by_name(t.name).name, t.name);
        }
    }
}
