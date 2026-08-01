//! The dashboard's visual language: one accent, one level scale, one
//! placeholder, so every view blubat grows reads as the same app.

use blubat_core::{Direction, Trend};
use ratatui::style::Color;

/// Used for the app name, the selection marker and the keymap.
pub const ACCENT: Color = Color::Cyan;

/// Selection background: a dark cyan tint rather than a full inverse, which
/// would destroy the per column colours that carry the meaning.
pub const SELECTION_BG: Color = Color::Rgb(0x14, 0x33, 0x3b);

/// Stands in for anything no source reported, as the CLI prints it.
pub const UNKNOWN: &str = "--";

/// Below this level a live reading wants attention rather than a glance.
pub const CRITICAL_BELOW: u8 = 15;

/// Cells in the battery bar.
pub const BAR_WIDTH: usize = 12;

/// Green from 50, yellow from 15, red below it, dim for no reading at all.
pub fn level_color(level: Option<u8>) -> Color {
    match level {
        Some(50..) => Color::Green,
        Some(CRITICAL_BELOW..) => Color::Yellow,
        Some(_) => Color::Red,
        None => Color::DarkGray,
    }
}

/// Whether a level is low enough to want attention.
///
/// Takes the level rather than the device, so a caller has to have decided the
/// reading is live: a disconnected device's level is last seen data and can be
/// arbitrarily old, which is never an alert.
pub fn is_critical(level: Option<u8>) -> bool {
    level.is_some_and(|level| level < CRITICAL_BELOW)
}

/// A level as the dashboard prints it.
pub fn percent(level: Option<u8>) -> String {
    level.map_or_else(|| UNKNOWN.to_string(), |level| format!("{level}%"))
}

/// The battery bar as its filled run and the trough behind it.
///
/// Two strings rather than one, so each half can take its own colour while the
/// pair always adds up to the same width and the rows keep their rhythm.
pub fn battery_bar(level: Option<u8>) -> (String, String) {
    let filled = level
        .map_or(0, |level| (usize::from(level) * BAR_WIDTH + 50) / 100)
        .min(BAR_WIDTH);

    (
        "\u{2588}".repeat(filled),
        "\u{2591}".repeat(BAR_WIDTH - filled),
    )
}

/// Which way a level is going and how fast, absent until history can say.
pub fn trend(trend: Option<Trend>) -> String {
    trend.map_or_else(
        || UNKNOWN.to_string(),
        |trend| format!("{} {}%/h", arrow(trend.direction), magnitude(trend.rate)),
    )
}

fn arrow(direction: Direction) -> char {
    match direction {
        Direction::Rising => '\u{2191}',
        Direction::Falling => '\u{2193}',
        Direction::Flat => '\u{2192}',
    }
}

/// A rate without its sign, which the arrow beside it already carries.
///
/// Clamped so a rate measured over a couple of seconds cannot widen the column.
fn magnitude(rate: f64) -> u32 {
    rate.abs().round().clamp(0.0, 999.0) as u32
}

/// How long ago a reading was taken, in the largest unit that still says something.
pub fn age(seconds: i64) -> String {
    match seconds {
        ..1 => "now".to_string(),
        1..60 => format!("{seconds}s ago"),
        60..3_600 => format!("{}m ago", seconds / 60),
        3_600..86_400 => format!("{}h ago", seconds / 3_600),
        _ => format!("{}d ago", seconds / 86_400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trend_of(rate: f64, direction: Direction) -> String {
        trend(Some(Trend { rate, direction }))
    }

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

    #[test]
    fn the_critical_band_is_the_red_one() {
        assert!(is_critical(Some(0)));
        assert!(is_critical(Some(14)));
        assert!(!is_critical(Some(15)), "the yellow band is a glance");
        assert!(!is_critical(None), "nothing read is not an alert");
    }

    #[test]
    fn the_bar_is_always_the_same_width() {
        for level in [None, Some(0), Some(1), Some(50), Some(99), Some(100)] {
            let (filled, trough) = battery_bar(level);

            assert_eq!(
                filled.chars().count() + trough.chars().count(),
                BAR_WIDTH,
                "at {level:?}"
            );
        }
    }

    #[test]
    fn the_bar_fills_in_proportion_to_the_level() {
        assert_eq!(battery_bar(None).0.chars().count(), 0);
        assert_eq!(battery_bar(Some(0)).0.chars().count(), 0);
        assert_eq!(battery_bar(Some(23)).0.chars().count(), 3);
        assert_eq!(battery_bar(Some(50)).0.chars().count(), 6);
        assert_eq!(battery_bar(Some(100)).0.chars().count(), BAR_WIDTH);
    }

    #[test]
    fn a_trend_reads_as_a_direction_and_a_rate() {
        assert_eq!(trend_of(-4.2, Direction::Falling), "\u{2193} 4%/h");
        assert_eq!(trend_of(10.0, Direction::Rising), "\u{2191} 10%/h");
        assert_eq!(trend_of(0.0, Direction::Flat), "\u{2192} 0%/h");
        assert_eq!(trend(None), UNKNOWN, "no history is not a flat line");
    }

    #[test]
    fn a_rate_measured_over_seconds_cannot_widen_the_column() {
        assert_eq!(trend_of(-7_200.0, Direction::Falling), "\u{2193} 999%/h");
        assert_eq!(trend_of(f64::NAN, Direction::Flat), "\u{2192} 0%/h");
    }

    #[test]
    fn an_age_reads_in_the_largest_unit_that_says_something() {
        assert_eq!(age(-1), "now", "a reading from the future is now");
        assert_eq!(age(0), "now");
        assert_eq!(age(2), "2s ago");
        assert_eq!(age(59), "59s ago");
        assert_eq!(age(60), "1m ago");
        assert_eq!(age(3_599), "59m ago");
        assert_eq!(age(3_600), "1h ago");
        assert_eq!(age(86_399), "23h ago");
        assert_eq!(age(172_800), "2d ago");
    }
}
