use std::fmt;
use std::str::FromStr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::{Error, Result};

/// A whole second, rendered and parsed as RFC 3339 in UTC.
///
/// Whole seconds because every timestamp blubat records is the moment it took a
/// reading, and both the JSON output and the watch files are read by people.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Timestamp(i64);

impl Timestamp {
    pub fn now() -> Self {
        let seconds = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|since_epoch| since_epoch.as_secs() as i64)
            .unwrap_or_default();

        Self(seconds)
    }

    pub const fn from_unix(seconds: i64) -> Self {
        Self(seconds)
    }

    pub const fn unix(self) -> i64 {
        self.0
    }

    /// This moment advanced by `duration`, clamped rather than wrapped.
    pub(crate) fn plus(self, duration: Duration) -> Self {
        Self(
            self.0
                .saturating_add(i64::try_from(duration.as_secs()).unwrap_or(i64::MAX)),
        )
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (year, month, day) = civil_from_days(self.0.div_euclid(SECONDS_PER_DAY));
        let seconds = self.0.rem_euclid(SECONDS_PER_DAY);

        write!(
            f,
            "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
            seconds / 3600,
            seconds / 60 % 60,
            seconds % 60
        )
    }
}

impl FromStr for Timestamp {
    type Err = Error;

    fn from_str(text: &str) -> Result<Self> {
        parse_utc(text)
            .map(Timestamp)
            .ok_or_else(|| Error::Format(format!("`{text}` is not an RFC 3339 UTC timestamp")))
    }
}

impl Serialize for Timestamp {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Timestamp {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

const SECONDS_PER_DAY: i64 = 86_400;

/// Parses exactly `YYYY-MM-DDTHH:MM:SSZ`, the only shape blubat writes.
fn parse_utc(text: &str) -> Option<i64> {
    const DIGITS: [usize; 14] = [0, 1, 2, 3, 5, 6, 8, 9, 11, 12, 14, 15, 17, 18];

    let bytes = text.as_bytes();
    let shaped = bytes.len() == 20
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && matches!(bytes[10], b'T' | b't' | b' ')
        && bytes[13] == b':'
        && bytes[16] == b':'
        && matches!(bytes[19], b'Z' | b'z')
        && DIGITS.iter().all(|&i| bytes[i].is_ascii_digit());
    if !shaped {
        return None;
    }

    let field = |range: std::ops::Range<usize>| text[range].parse::<i64>().unwrap_or_default();
    let (year, month, day) = (field(0..4), field(5..7), field(8..10));
    let (hour, minute, second) = (field(11..13), field(14..16), field(17..19));

    let in_range = (1..=12).contains(&month)
        && (1..=31).contains(&day)
        && hour < 24
        && minute < 60
        && second < 60;

    in_range.then(|| {
        days_from_civil(year, month, day) * SECONDS_PER_DAY + hour * 3600 + minute * 60 + second
    })
}

/// Days since 1970-01-01 for a proleptic Gregorian date.
///
/// Howard Hinnant's `days_from_civil`, which is exact over the whole range
/// blubat can produce and needs no calendar tables.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = year - i64::from(month <= 2);
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;

    era * 146_097 + day_of_era - 719_468
}

/// The inverse of [`days_from_civil`], yielding year, month and day.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = month_index + if month_index < 10 { 3 } else { -9 };

    (year + i64::from(month <= 2), month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_rfc_3339_in_utc() {
        assert_eq!(Timestamp::from_unix(0).to_string(), "1970-01-01T00:00:00Z");
        assert_eq!(
            Timestamp::from_unix(1_754_107_200).to_string(),
            "2025-08-02T04:00:00Z"
        );
        assert_eq!(
            Timestamp::from_unix(1_785_643_199).to_string(),
            "2026-08-02T03:59:59Z"
        );
    }

    #[test]
    fn round_trips_through_its_own_rendering() {
        for seconds in [
            0,
            1,
            951_782_400,   // 2000-02-29, a leap day in a century leap year
            1_709_164_800, // 2024-02-29, an ordinary leap day
            1_785_643_199,
            4_102_444_800, // 2100-01-01, past a skipped century leap year
        ] {
            let timestamp = Timestamp::from_unix(seconds);
            let parsed: Timestamp = timestamp.to_string().parse().expect("round trip");

            assert_eq!(parsed, timestamp, "at {seconds}");
        }
    }

    #[test]
    fn parses_lowercase_and_space_separators() {
        let expected = Timestamp::from_unix(1_785_643_199);

        assert_eq!(
            "2026-08-02t03:59:59z".parse::<Timestamp>().expect("valid"),
            expected
        );
        assert_eq!(
            "2026-08-02 03:59:59Z".parse::<Timestamp>().expect("valid"),
            expected
        );
    }

    #[test]
    fn rejects_anything_outside_that_shape() {
        for text in [
            "",
            "2026-08-02",
            "2026-08-02T03:59:59",
            "2026-08-02T03:59:59+10:00",
            "2026-13-02T03:59:59Z",
            "2026-08-00T03:59:59Z",
            "2026-08-02T24:00:00Z",
            "2026-08-02T03:60:59Z",
            "2026-08-02T03:59:60Z",
            "+026-08-02T03:59:59Z",
        ] {
            assert!(
                text.parse::<Timestamp>().is_err(),
                "{text} should be rejected"
            );
        }
    }

    #[test]
    fn serialises_as_an_rfc_3339_string() {
        let timestamp = Timestamp::from_unix(1_785_643_199);

        let json = serde_json::to_string(&timestamp).expect("serialisable");
        assert_eq!(json, "\"2026-08-02T03:59:59Z\"");
        assert_eq!(
            serde_json::from_str::<Timestamp>(&json).expect("deserialisable"),
            timestamp
        );
    }

    #[test]
    fn now_round_trips_through_its_own_rendering() {
        let now = Timestamp::now();

        assert_eq!(
            now.to_string().parse::<Timestamp>().expect("round trip"),
            now
        );
    }

    #[test]
    fn a_deadline_is_this_moment_plus_a_duration() {
        let start = Timestamp::from_unix(1_785_643_199);

        assert_eq!(start.plus(Duration::ZERO), start);
        assert_eq!(
            start.plus(Duration::from_secs(600)),
            Timestamp::from_unix(1_785_643_799)
        );
        assert_eq!(
            Timestamp::from_unix(i64::MAX).plus(Duration::from_secs(1)),
            Timestamp::from_unix(i64::MAX),
            "a deadline no clock reaches beats an overflow"
        );
    }
}
