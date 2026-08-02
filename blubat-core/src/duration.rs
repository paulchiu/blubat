//! Durations as the config file and the CLI write them, such as `45s` or `2h`.

use std::str::FromStr;
use std::time::Duration;

use serde::{Deserialize, Deserializer, de::Error as _};

use crate::error::{Error, Result};

/// Suffixes a written duration may carry, and what each one is worth.
const UNITS: [(char, u64); 3] = [('s', 1), ('m', 60), ('h', 3_600)];

/// Parses a duration written as bare seconds or with an `s`, `m` or `h` suffix.
pub fn parse_duration(text: &str) -> Result<Duration> {
    let text = text.trim();
    let (digits, per_unit) = UNITS
        .iter()
        .find_map(|(suffix, seconds)| text.strip_suffix(*suffix).map(|left| (left, *seconds)))
        .unwrap_or((text, 1));

    digits
        .parse::<u64>()
        .ok()
        .and_then(|count| count.checked_mul(per_unit))
        .map(Duration::from_secs)
        .ok_or_else(|| {
            Error::Format(format!(
                "`{text}` is not a duration such as `45s`, `30m` or `2h`"
            ))
        })
}

/// How long a hook waits before the same event may run it again.
///
/// Separate from and additive to the hysteresis the event engine applies:
/// hysteresis suppresses nonsense events, a debounce rate limits real ones.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(try_from = "String")]
pub enum Debounce {
    /// At most one run per re-arm cycle, however long that cycle lasts.
    Once,
    /// No second run inside this window, even when the event re-fires.
    Window(Duration),
}

impl Debounce {
    /// The window to hold off for, which a per cycle debounce does not have.
    pub fn window(self) -> Option<Duration> {
        match self {
            Debounce::Once => None,
            Debounce::Window(window) => Some(window),
        }
    }
}

impl FromStr for Debounce {
    type Err = Error;

    fn from_str(text: &str) -> Result<Self> {
        match text.trim() {
            "once" => Ok(Debounce::Once),
            window => parse_duration(window).map(Debounce::Window),
        }
    }
}

impl TryFrom<String> for Debounce {
    type Error = Error;

    fn try_from(text: String) -> Result<Self> {
        text.parse()
    }
}

/// Reads a duration written as a TOML string.
pub(crate) fn de_duration<'de, D: Deserializer<'de>>(
    de: D,
) -> std::result::Result<Duration, D::Error> {
    String::deserialize(de).and_then(|text| parse_duration(&text).map_err(D::Error::custom))
}

/// Reads an optional duration, where the key being absent is not an error.
pub(crate) fn de_optional_duration<'de, D: Deserializer<'de>>(
    de: D,
) -> std::result::Result<Option<Duration>, D::Error> {
    Option::<String>::deserialize(de)?
        .map(|text| parse_duration(&text).map_err(D::Error::custom))
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_suffix_scales_the_count_and_a_bare_number_is_seconds() {
        assert_eq!(
            parse_duration("45s").expect("parses"),
            Duration::from_secs(45)
        );
        assert_eq!(
            parse_duration("30m").expect("parses"),
            Duration::from_secs(1_800)
        );
        assert_eq!(
            parse_duration("2h").expect("parses"),
            Duration::from_secs(7_200)
        );
        assert_eq!(
            parse_duration("90").expect("parses"),
            Duration::from_secs(90)
        );
        assert_eq!(
            parse_duration(" 5m ").expect("parses"),
            Duration::from_secs(300)
        );
        assert_eq!(parse_duration("0s").expect("parses"), Duration::ZERO);
    }

    #[test]
    fn rejects_anything_that_is_not_a_duration() {
        for text in [
            "",
            "  ",
            "s",
            "m",
            "-5s",
            "5 m",
            "5 minutes",
            "1.5h",
            "5d",
            "30\u{e9}",
        ] {
            assert!(
                matches!(parse_duration(text), Err(Error::Format(_))),
                "{text:?} should be rejected"
            );
        }
    }

    #[test]
    fn a_count_that_would_overflow_seconds_is_rejected_rather_than_wrapped() {
        assert!(parse_duration(&format!("{}h", u64::MAX)).is_err());
    }

    #[test]
    fn a_debounce_is_a_window_or_the_re_arm_cycle() {
        assert_eq!("once".parse::<Debounce>().expect("parses"), Debounce::Once);
        assert_eq!(
            "30m".parse::<Debounce>().expect("parses"),
            Debounce::Window(Duration::from_secs(1_800))
        );
        assert_eq!(Debounce::Once.window(), None);
        assert_eq!(
            Debounce::Window(Duration::from_secs(60)).window(),
            Some(Duration::from_secs(60))
        );
        assert!("never".parse::<Debounce>().is_err());
    }
}
