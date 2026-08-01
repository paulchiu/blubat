//! The dashboard's visual language: one accent, one level scale, one
//! placeholder, so every view blubat grows reads as the same app.

use ratatui::style::Color;

/// Used for the app name, the selection marker and the keymap.
pub const ACCENT: Color = Color::Cyan;

/// Selection background: a dark cyan tint rather than a full inverse, which
/// would destroy the per column colours that carry the meaning.
pub const SELECTION_BG: Color = Color::Rgb(0x14, 0x33, 0x3b);

/// Stands in for anything no source reported, as the CLI prints it.
pub const UNKNOWN: &str = "--";

/// Green from 50, yellow from 15, red below it, dim for no reading at all.
pub fn level_color(level: Option<u8>) -> Color {
    match level {
        Some(50..) => Color::Green,
        Some(15..) => Color::Yellow,
        Some(_) => Color::Red,
        None => Color::DarkGray,
    }
}

/// A level as the dashboard prints it.
pub fn percent(level: Option<u8>) -> String {
    level.map_or_else(|| UNKNOWN.to_string(), |level| format!("{level}%"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_level_scale_colours_each_band() {
        assert_eq!(level_color(Some(100)), Color::Green);
        assert_eq!(level_color(Some(50)), Color::Green);
        assert_eq!(level_color(Some(49)), Color::Yellow);
        assert_eq!(level_color(Some(15)), Color::Yellow);
        assert_eq!(level_color(Some(14)), Color::Red);
        assert_eq!(level_color(Some(0)), Color::Red, "empty is still a reading");
        assert_eq!(level_color(None), Color::DarkGray);
    }

    #[test]
    fn a_level_prints_as_a_percentage_or_as_nothing_read() {
        assert_eq!(percent(Some(85)), "85%");
        assert_eq!(percent(Some(0)), "0%");
        assert_eq!(percent(None), UNKNOWN);
    }
}
