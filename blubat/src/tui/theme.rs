//! The dashboard's visual language: one accent, one level scale, one
//! placeholder, so every view blubat grows reads as the same app.
//!
//! What that language is drawn in is the config's to say. A [`Palette`] is one
//! named scheme with the `[theme]` overrides applied over it, and a [`Look`] is
//! that palette beside the glyphs, which the same table also has a say in. Both
//! are values rather than constants so `r` can replace them mid run.

use blubat_core::{Rgb, Scheme, Theme, Thresholds};
use ratatui::style::Color;

use super::glyph::Glyphs;

/// The colours the dashboard draws with.
///
/// The four the config file can name are the accent and the three bands of the
/// level scale. The rest are the structure those sit in and follow the scheme
/// alone, since a file that could recolour a heading could also make the
/// dashboard unreadable one key at a time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Palette {
    /// The app name, the selection marker and the keymap.
    pub accent: Color,
    /// The top band of the level scale, and anything that is fine.
    pub ok: Color,
    /// The middle band: worth a glance rather than attention.
    pub low: Color,
    /// The bottom band, and the count of what is in it.
    pub critical: Color,
    /// The critical colour where it has to be caught first: a device's name,
    /// and the state beside it.
    pub alert: Color,
    /// The charging mark, which is the one piece of good news on a row.
    pub charging: Color,
    /// Ordinary row text.
    pub text: Color,
    /// Device names, which sit above the rest of their row.
    pub strong: Color,
    /// Headings, troughs, and everything the eye should skip.
    pub dim: Color,
    /// The selected row's background: a tint rather than a full inverse, which
    /// would destroy the per column colours that carry the meaning.
    pub selection: Color,
}

impl Palette {
    /// The scheme blubat draws in unless the file says otherwise.
    pub const DARK: Self = Self {
        accent: Color::Cyan,
        ok: Color::Green,
        low: Color::Yellow,
        critical: Color::Red,
        alert: Color::LightRed,
        charging: Color::LightGreen,
        text: Color::Gray,
        strong: Color::White,
        dim: Color::DarkGray,
        selection: Color::Rgb(0x14, 0x33, 0x3b),
    };

    /// The same language over a light terminal.
    ///
    /// Spelled out rather than named, because a named colour is whatever the
    /// terminal makes of it and half of them assume something dark behind.
    pub const LIGHT: Self = Self {
        accent: Color::Rgb(0x09, 0x69, 0xda),
        ok: Color::Rgb(0x1a, 0x7f, 0x37),
        low: Color::Rgb(0x9a, 0x67, 0x00),
        critical: Color::Rgb(0xcf, 0x22, 0x2e),
        alert: Color::Rgb(0xa4, 0x0e, 0x26),
        charging: Color::Rgb(0x1a, 0x7f, 0x37),
        text: Color::Rgb(0x24, 0x29, 0x2f),
        strong: Color::Rgb(0x01, 0x04, 0x09),
        dim: Color::Rgb(0x6e, 0x77, 0x81),
        selection: Color::Rgb(0xdd, 0xf4, 0xff),
    };

    /// The terminal's own foreground and one grey, for a setup wanting no
    /// colour at all. The grey stays: without it a heading and a reading would
    /// be the same mark, and the layout is dense enough to need the difference.
    pub const MONO: Self = Self {
        accent: Color::Reset,
        ok: Color::Reset,
        low: Color::Reset,
        critical: Color::Reset,
        alert: Color::Reset,
        charging: Color::Reset,
        text: Color::Reset,
        strong: Color::Reset,
        dim: Color::DarkGray,
        selection: Color::DarkGray,
    };

    /// The palette one `[theme]` table asks for.
    ///
    /// An override replaces every use of the colour it names, the brighter
    /// variant the scheme pairs with it included: a file that recoloured
    /// critical and kept the scheme's light red would read as two reds.
    pub fn of(theme: &Theme) -> Self {
        let base = match theme.scheme {
            Scheme::Dark => Self::DARK,
            Scheme::Light => Self::LIGHT,
            Scheme::Mono => Self::MONO,
        };

        Self {
            accent: theme.accent.map_or(base.accent, colour),
            ok: theme.ok.map_or(base.ok, colour),
            charging: theme.ok.map_or(base.charging, colour),
            low: theme.low.map_or(base.low, colour),
            critical: theme.critical.map_or(base.critical, colour),
            alert: theme.critical.map_or(base.alert, colour),
            ..base
        }
    }

    /// Green from [`HEALTHY_FROM`], yellow down to the device's own critical
    /// threshold, red below it, dim for no reading at all.
    ///
    /// The red band is the engine's number rather than one of this module's, so
    /// a device the dashboard paints red is one blubat has raised
    /// `critical_battery` for and not merely one it drew that way.
    pub fn level(self, level: Option<u8>, thresholds: Thresholds) -> Color {
        match level {
            Some(HEALTHY_FROM..) => self.ok,
            Some(level) if !is_critical(Some(level), thresholds) => self.low,
            Some(_) => self.critical,
            None => self.dim,
        }
    }
}

impl Default for Palette {
    fn default() -> Self {
        Self::DARK
    }
}

/// Everything `[theme]` decides, resolved over what the environment suggested.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Look {
    pub palette: Palette,
    pub glyphs: Glyphs,
    /// The glyphs the environment was guessed to support, kept so a reload
    /// applies the file's override to the guess rather than to the last
    /// override, which a removed `charging_glyph` would otherwise leave behind.
    detected: Glyphs,
}

impl Look {
    /// What `theme` asks for, over the glyphs the environment suggested.
    pub fn of(theme: &Theme, detected: Glyphs) -> Self {
        Self {
            palette: Palette::of(theme),
            glyphs: detected.clone().overridden(theme.charging_glyph.as_deref()),
            detected,
        }
    }

    /// The look a freshly read config asks for, over the same guess.
    pub fn reloaded(&self, theme: &Theme) -> Self {
        Self::of(theme, self.detected.clone())
    }
}

/// One config colour as ratatui draws it.
fn colour(rgb: Rgb) -> Color {
    Color::Rgb(rgb.red, rgb.green, rgb.blue)
}

/// Stands in for anything no source reported, as the CLI prints it.
pub const UNKNOWN: &str = "--";

/// At or above this level a battery is nobody's problem, whatever is configured.
///
/// The top of the scale rather than the bottom of it: what counts as critical
/// is the device's own threshold, which the config and the device itself have a
/// say in, but half full is half full everywhere.
pub const HEALTHY_FROM: u8 = 50;

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

/// Whether a level is low enough to want attention.
///
/// The same test the event engine applies, so the count on the status line and
/// the events blubat raises cannot disagree. Takes the level rather than the
/// device, so a caller has to have decided the reading is live: a disconnected
/// device's level is last seen data and can be arbitrarily old, which is never
/// an alert.
pub fn is_critical(level: Option<u8>, thresholds: Thresholds) -> bool {
    level.is_some_and(|level| level < thresholds.critical)
}

/// A level as the dashboard prints it.
pub fn percent(level: Option<u8>) -> String {
    level.map_or_else(|| UNKNOWN.to_string(), |level| format!("{level}%"))
}

/// The battery bar as its filled run and the trough behind it.
///
/// Two strings rather than one, so each half can take its own colour while the
/// pair always adds up to the same width and the rows keep their rhythm.
pub fn bar(level: Option<u8>, width: usize) -> (String, String) {
    let filled = level
        .map_or(0, |level| (usize::from(level) * width + 50) / 100)
        .min(width);

    ("\u{2588}".repeat(filled), "\u{2591}".repeat(width - filled))
}

/// The table's bar, which is [`BAR_WIDTH`] cells of [`bar`].
pub fn battery_bar(level: Option<u8>) -> (String, String) {
    bar(level, BAR_WIDTH)
}

/// A charge or drain rate, in the percent per hour the detail view names it by.
///
/// Unsigned: the direction is spelled out beside it, and a minus in front of a
/// drain rate would say the same thing twice.
pub fn rate(percent_per_hour: f64) -> String {
    format!("{:.1}%/h", percent_per_hour.abs())
}

/// How long something takes, in the two largest units that say anything.
///
/// Two rather than one, since `1h 40m` and `2h` are different answers to
/// whether there is time to go and find the cable.
pub fn span(seconds: i64) -> String {
    let minutes = (seconds.max(0) + 30) / 60;
    let (hours, minutes) = (minutes / 60, minutes % 60);

    match (hours / 24, hours % 24, minutes) {
        (0, 0, minutes) => format!("{minutes}m"),
        (0, hours, 0) => format!("{hours}h"),
        (0, hours, minutes) => format!("{hours}h {minutes}m"),
        (days, 0, _) => format!("{days}d"),
        (days, hours, _) => format!("{days}d {hours}h"),
    }
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

    /// A `[theme]` table as the config file writes it.
    fn theme(written: &str) -> Theme {
        blubat_core::Config::parse(&format!("[theme]\n{written}"))
            .expect("the test theme parses")
            .theme
    }

    /// The thresholds a device with nothing configured for it is judged by.
    fn built_in() -> Thresholds {
        Thresholds::BUILT_IN
    }

    #[test]
    fn the_level_scale_colours_each_band() {
        let palette = Palette::DARK;
        let level = |level| palette.level(level, built_in());

        assert_eq!(level(Some(100)), palette.ok);
        assert_eq!(level(Some(50)), palette.ok);
        assert_eq!(level(Some(49)), palette.low);
        assert_eq!(level(Some(10)), palette.low, "the built-in critical is 10");
        assert_eq!(level(Some(9)), palette.critical);
        assert_eq!(level(Some(0)), palette.critical, "empty is still a reading");
        assert_eq!(level(None), palette.dim);
    }

    #[test]
    fn the_red_band_is_the_one_the_device_is_judged_by() {
        let palette = Palette::DARK;
        let jumpy = Thresholds {
            critical: 40,
            ..Thresholds::BUILT_IN
        };

        assert_eq!(palette.level(Some(39), jumpy), palette.critical);
        assert_eq!(palette.level(Some(40), jumpy), palette.low);
        assert!(is_critical(Some(39), jumpy));
        assert!(
            !is_critical(Some(39), built_in()),
            "the same level under the built-in threshold is a glance"
        );
    }

    #[test]
    fn an_unconfigured_dashboard_draws_in_the_dark_scheme() {
        assert_eq!(Palette::of(&Theme::default()), Palette::DARK);
        assert_eq!(Palette::default(), Palette::DARK);
    }

    #[test]
    fn the_named_scheme_chooses_the_palette_under_the_overrides() {
        assert_eq!(Palette::of(&theme("scheme = \"light\"")), Palette::LIGHT);
        assert_eq!(Palette::of(&theme("scheme = \"mono\"")), Palette::MONO);
        assert_eq!(
            Palette::of(&theme("scheme = \"light\"")).level(Some(5), built_in()),
            Palette::LIGHT.critical,
            "the level scale follows the scheme too"
        );
    }

    #[test]
    fn each_override_replaces_one_colour_of_the_scheme_it_sits_on() {
        let palette = Palette::of(&theme(
            "scheme = \"light\"\naccent = \"#39c5cf\"\nlow = \"#c69026\"\n",
        ));

        assert_eq!(
            palette.accent,
            Color::Rgb(0x39, 0xc5, 0xcf),
            "what the file named"
        );
        assert_eq!(palette.low, Color::Rgb(0xc6, 0x90, 0x26));
        assert_eq!(palette.ok, Palette::LIGHT.ok, "and nothing else moved");
        assert_eq!(palette.dim, Palette::LIGHT.dim);
    }

    #[test]
    fn an_override_carries_the_brighter_variant_paired_with_it() {
        let palette = Palette::of(&theme("critical = \"#f47067\"\nok = \"#57ab5a\"\n"));

        assert_eq!(palette.alert, palette.critical, "one red, not two");
        assert_eq!(palette.charging, palette.ok);
        assert_ne!(
            Palette::DARK.alert,
            Palette::DARK.critical,
            "which the scheme itself does distinguish"
        );
    }

    #[test]
    fn a_look_takes_its_glyph_from_the_file_and_its_colours_from_the_scheme() {
        let look = Look::of(
            &theme("scheme = \"mono\"\ncharging_glyph = \"^\"\n"),
            Glyphs::NERD_FONT,
        );

        assert_eq!(look.palette, Palette::MONO);
        assert_eq!(look.glyphs.charging, "^");
        assert_eq!(
            Look::of(&Theme::default(), Glyphs::NERD_FONT).glyphs,
            Glyphs::NERD_FONT,
            "an unconfigured glyph stays the guessed one"
        );
    }

    #[test]
    fn a_reload_applies_the_new_theme_to_the_guess_rather_than_to_the_old_one() {
        let overridden = Look::of(&theme("charging_glyph = \"^\""), Glyphs::NERD_FONT);

        let removed = overridden.reloaded(&Theme::default());

        assert_eq!(
            removed.glyphs,
            Glyphs::NERD_FONT,
            "taking the override out puts the guess back"
        );
        assert_eq!(
            overridden.reloaded(&theme("scheme = \"light\"")).palette,
            Palette::LIGHT
        );
    }

    #[test]
    fn a_level_prints_as_a_percentage_or_as_nothing_read() {
        assert_eq!(percent(Some(85)), "85%");
        assert_eq!(percent(Some(0)), "0%");
        assert_eq!(percent(None), UNKNOWN);
    }

    #[test]
    fn the_critical_band_is_the_red_one() {
        assert!(is_critical(Some(0), built_in()));
        assert!(is_critical(Some(9), built_in()));
        assert!(
            !is_critical(Some(10), built_in()),
            "the yellow band is a glance"
        );
        assert!(
            !is_critical(None, built_in()),
            "nothing read is not an alert"
        );
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
    fn a_bar_of_any_width_fills_in_proportion_and_stays_that_width() {
        for width in [0, 1, 8, BAR_WIDTH, 40] {
            for level in [None, Some(0), Some(37), Some(100)] {
                let (filled, trough) = bar(level, width);

                assert_eq!(
                    filled.chars().count() + trough.chars().count(),
                    width,
                    "{level:?} at {width}"
                );
            }
        }

        assert_eq!(bar(Some(50), 40).0.chars().count(), 20);
        assert_eq!(bar(Some(100), 40).0.chars().count(), 40);
        assert_eq!(bar(None, 40).0.chars().count(), 0);
    }

    #[test]
    fn a_rate_reads_as_percent_per_hour_whichever_way_it_points() {
        assert_eq!(rate(4.24), "4.2%/h");
        assert_eq!(rate(-4.24), "4.2%/h", "the direction is named beside it");
        assert_eq!(rate(0.0), "0.0%/h");
        assert_eq!(rate(18.0), "18.0%/h");
    }

    #[test]
    fn a_span_reads_in_the_two_largest_units_that_say_something() {
        assert_eq!(span(0), "0m");
        assert_eq!(span(-90), "0m", "nothing left is not a negative wait");
        assert_eq!(span(90), "2m", "rounded to the nearest minute");
        assert_eq!(span(3_600), "1h");
        assert_eq!(span(6_000), "1h 40m");
        assert_eq!(span(86_400), "1d");
        assert_eq!(span(180_000), "2d 2h");
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
