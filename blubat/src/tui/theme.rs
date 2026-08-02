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

/// Below this level a live reading wants attention rather than a glance.
pub const CRITICAL_BELOW: u8 = 15;

/// Cells in the battery bar.
pub const BAR_WIDTH: usize = 12;

/// Cells in the trend sparkline, which is as many recent levels as it draws.
///
/// Six is enough to answer which way a battery is going, which is the question
/// the level on its own cannot.
pub const SPARK_WIDTH: usize = 6;

/// The sparkline's eight heights, lowest first.
const SPARKS: [char; 8] = [
    '\u{2581}', '\u{2582}', '\u{2583}', '\u{2584}', '\u{2585}', '\u{2586}', '\u{2587}', '\u{2588}',
];

/// Stands in for a cell no reading has reached yet.
const NO_DATA: char = '\u{b7}';

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

/// Recent levels as a sparkline, oldest first and always [`SPARK_WIDTH`] cells.
///
/// Scaled over the range the levels themselves cover rather than over nothing
/// to full, since the question is which way a battery is going rather than how
/// full it is. Cells no reading has reached yet are dots, so a device blubat
/// has never sampled reads as no data rather than as a flat line.
pub fn sparkline(levels: &[u8]) -> String {
    let recent = &levels[levels.len().saturating_sub(SPARK_WIDTH)..];
    let lowest = recent.iter().copied().min().unwrap_or_default();
    let highest = recent.iter().copied().max().unwrap_or_default();

    std::iter::repeat_n(NO_DATA, SPARK_WIDTH - recent.len())
        .chain(recent.iter().map(|level| spark(*level, lowest, highest)))
        .collect()
}

/// One level's height against the lowest and highest of the run it is in.
fn spark(level: u8, lowest: u8, highest: u8) -> char {
    let top = SPARKS.len() - 1;
    let span = f64::from(highest.saturating_sub(lowest));
    // A run that never changed is steady rather than empty, so it draws mid
    // scale: along the bottom it would read as a flat empty battery.
    let height = if span > 0.0 {
        (f64::from(level.saturating_sub(lowest)) / span * top as f64).round() as usize
    } else {
        SPARKS.len() / 2
    };

    SPARKS[height.min(top)]
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
    fn a_sparkline_is_always_the_same_width() {
        for levels in [
            vec![],
            vec![50],
            vec![10, 90],
            vec![1, 2, 3, 4, 5, 6],
            (0..40).collect(),
        ] {
            assert_eq!(
                sparkline(&levels).chars().count(),
                SPARK_WIDTH,
                "at {levels:?}"
            );
        }
    }

    #[test]
    fn a_sparkline_draws_the_most_recent_levels_oldest_first() {
        assert_eq!(sparkline(&[0, 20, 40, 60, 80, 100]), "▁▂▄▅▇█");
        assert_eq!(
            sparkline(&[9, 9, 0, 20, 40, 60, 80, 100]),
            "▁▂▄▅▇█",
            "an older reading than the line holds is dropped rather than scaled in"
        );
        assert_eq!(sparkline(&[100, 0]), "\u{b7}\u{b7}\u{b7}\u{b7}█▁");
    }

    #[test]
    fn a_line_is_scaled_over_what_it_holds_rather_than_over_a_full_battery() {
        assert_eq!(
            sparkline(&[80, 81, 82]),
            "\u{b7}\u{b7}\u{b7}▁▅█",
            "three points apart still read as a climb"
        );
    }

    #[test]
    fn no_data_is_dots_and_an_unchanged_level_is_a_flat_line() {
        assert_eq!(sparkline(&[]), "\u{b7}".repeat(SPARK_WIDTH));
        assert_eq!(sparkline(&[77, 77, 77, 77, 77, 77]), "▅▅▅▅▅▅");
        assert_eq!(
            sparkline(&[0, 0, 0, 0, 0, 0]),
            "▅▅▅▅▅▅",
            "an empty battery is as flat as a full one"
        );
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
