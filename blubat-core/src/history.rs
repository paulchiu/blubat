use std::collections::{BTreeMap, VecDeque};
use std::fmt;

use crate::address::Address;
use crate::snapshot::Snapshot;
use crate::timestamp::Timestamp;

const SECONDS_PER_HOUR: f64 = 3_600.0;

/// One battery level, at the moment the reading that carried it was taken.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sample {
    pub at: Timestamp,
    pub level: u8,
}

/// Which way a device's level is moving.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Rising,
    Falling,
    Flat,
}

impl fmt::Display for Direction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Direction::Rising => "rising",
            Direction::Falling => "falling",
            Direction::Flat => "flat",
        })
    }
}

/// How fast a device is charging or draining, and which way.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Trend {
    /// Percent per hour, positive charging and negative draining.
    pub rate: f64,
    pub direction: Direction,
}

impl Trend {
    /// The slope of the line of best fit through `samples`, in percent per hour.
    ///
    /// Least squares rather than first against last, so one quantised step does
    /// not stand for the whole window, and so the answer does not depend on the
    /// order the samples arrived in. Absent while every sample shares one
    /// moment, including the single sample case, because no time has passed to
    /// measure a rate over.
    pub fn from_samples(samples: impl IntoIterator<Item = Sample>) -> Option<Self> {
        let (seconds, levels): (Vec<f64>, Vec<f64>) = samples
            .into_iter()
            .map(|sample| (sample.at.unix() as f64, f64::from(sample.level)))
            .unzip();

        let mean_second = mean(&seconds)?;
        let mean_level = mean(&levels)?;
        let spread: f64 = seconds
            .iter()
            .map(|second| (second - mean_second).powi(2))
            .sum();
        let paired: f64 = seconds
            .iter()
            .zip(&levels)
            .map(|(second, level)| (second - mean_second) * (level - mean_level))
            .sum();

        (spread > 0.0).then(|| Self::at_rate(paired / spread * SECONDS_PER_HOUR))
    }

    /// A rate and the direction its sign names, the only way a trend is built.
    fn at_rate(rate: f64) -> Self {
        let direction = match rate {
            rate if rate > 0.0 => Direction::Rising,
            rate if rate < 0.0 => Direction::Falling,
            _ => Direction::Flat,
        };

        Self { rate, direction }
    }
}

fn mean(values: &[f64]) -> Option<f64> {
    (!values.is_empty()).then(|| values.iter().sum::<f64>() / values.len() as f64)
}

/// A bounded ring of recent levels per device, keyed by address.
///
/// In memory and per process by design, so it starts empty on every run. It is
/// what the sparkline and the drain rate read, which is why it holds a fixed
/// number of samples per device rather than growing for as long as blubat runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct History {
    capacity: usize,
    devices: BTreeMap<Address, VecDeque<Sample>>,
}

impl History {
    /// Samples kept per device, which is what bounds the memory a long run
    /// takes. How much time they stand for follows whatever interval the caller
    /// polls at, so the window is stated in samples rather than in hours.
    pub const DEFAULT_CAPACITY: usize = 1_800;

    /// Keeps at most `capacity` samples per device, and at least one.
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            devices: BTreeMap::new(),
        }
    }

    /// Records the live level of every device in `reading`.
    ///
    /// A disconnected device has no active level, so it contributes nothing
    /// rather than a flat line at whatever macOS last persisted for it. Each
    /// sample is stamped with the moment its own device was read rather than
    /// the moment the reading was assembled, which is what keeps a cached slow
    /// tier level from being counted once per fast tick.
    pub fn record(&mut self, reading: &Snapshot) {
        for device in &reading.devices {
            if let Some(level) = device.active_level() {
                self.push(
                    &device.address,
                    Sample {
                        at: device.read_at,
                        level,
                    },
                );
            }
        }
    }

    /// Samples for one device, oldest first, empty for a device never seen.
    ///
    /// Reversible, since a sparkline only has room for the most recent few.
    pub fn samples(&self, address: &Address) -> impl DoubleEndedIterator<Item = Sample> {
        self.devices.get(address).into_iter().flatten().copied()
    }

    /// The trend over everything retained for one device.
    pub fn trend(&self, address: &Address) -> Option<Trend> {
        Trend::from_samples(self.samples(address))
    }

    /// Appends a sample taken later than everything already held.
    ///
    /// A stamp that does not move the series forward is dropped, so a reading
    /// carrying a level this device was already sampled at, and a clock that
    /// steps backwards, both leave the ring ordered oldest first.
    fn push(&mut self, address: &Address, sample: Sample) {
        let samples = self.devices.entry(address.clone()).or_default();

        if samples.back().is_some_and(|newest| newest.at >= sample.at) {
            return;
        }

        while samples.len() >= self.capacity {
            samples.pop_front();
        }
        samples.push_back(sample);
    }
}

impl Default for History {
    fn default() -> Self {
        Self::new(Self::DEFAULT_CAPACITY)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::{ChargeState, Device, Levels, Source};

    const HOUR: i64 = 3_600;
    const TRACKPAD: &str = "30-82-16-f2-24-90";

    fn address(raw: &str) -> Address {
        Address::parse(raw).expect("valid address")
    }

    fn trackpad() -> Address {
        address(TRACKPAD)
    }

    fn sample(second: i64, level: u8) -> Sample {
        Sample {
            at: Timestamp::from_unix(1_785_600_000 + second),
            level,
        }
    }

    fn rate_of(samples: [Sample; 3]) -> f64 {
        Trend::from_samples(samples).expect("a trend").rate
    }

    fn assert_rate(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() < 1e-9,
            "{actual} %/h is not {expected} %/h"
        );
    }

    /// A reading whose devices were all read at the moment it was taken, which
    /// is what the fast tier produces.
    fn reading(at: i64, devices: Vec<Device>) -> Snapshot {
        Snapshot {
            read_at: Timestamp::from_unix(at),
            devices: devices
                .into_iter()
                .map(|device| Device {
                    read_at: Timestamp::from_unix(at),
                    ..device
                })
                .collect(),
            warnings: Vec::new(),
        }
    }

    fn levels(history: &History) -> Vec<u8> {
        history
            .samples(&trackpad())
            .map(|sample| sample.level)
            .collect()
    }

    fn device(raw: &str, level: Option<u8>, connected: bool) -> Device {
        Device {
            address: address(raw),
            name: "Paul\u{2019}s Magic Trackpad".to_string(),
            kind: None,
            transport: None,
            levels: Levels {
                main: level,
                ..Levels::default()
            },
            charge: ChargeState::Unknown,
            source: Source::IoKit,
            connected,
            read_at: Timestamp::from_unix(0),
        }
    }

    #[test]
    fn a_climbing_level_is_a_charge_rate() {
        let trend = Trend::from_samples([sample(0, 50), sample(HOUR / 2, 55), sample(HOUR, 60)])
            .expect("a trend");

        assert_rate(trend.rate, 10.0);
        assert_eq!(trend.direction, Direction::Rising);
    }

    #[test]
    fn a_dropping_level_is_a_drain_rate() {
        let trend = Trend::from_samples([sample(0, 100), sample(HOUR, 96)]).expect("a trend");

        assert_rate(trend.rate, -4.0);
        assert_eq!(trend.direction, Direction::Falling);
    }

    #[test]
    fn an_unchanged_level_is_flat_rather_than_unknown() {
        let trend =
            Trend::from_samples([sample(0, 77), sample(30, 77), sample(60, 77)]).expect("a trend");

        assert_rate(trend.rate, 0.0);
        assert_eq!(trend.direction, Direction::Flat);
    }

    #[test]
    fn the_rate_is_per_hour_however_close_the_samples_are() {
        assert_rate(
            rate_of([sample(0, 80), sample(5, 79), sample(10, 78)]),
            -720.0,
        );
        assert_rate(
            rate_of([sample(0, 80), sample(HOUR, 79), sample(2 * HOUR, 78)]),
            -1.0,
        );
    }

    #[test]
    fn nothing_to_measure_over_has_no_trend() {
        assert_eq!(Trend::from_samples([]), None, "no samples");
        assert_eq!(Trend::from_samples([sample(0, 42)]), None, "one sample");
        assert_eq!(
            Trend::from_samples([sample(0, 42), sample(0, 90)]),
            None,
            "two readings stamped at one moment"
        );
    }

    #[test]
    fn out_of_order_stamps_read_the_same_as_ordered_ones() {
        let ordered = rate_of([sample(0, 50), sample(HOUR / 2, 55), sample(HOUR, 60)]);
        let shuffled = rate_of([sample(HOUR, 60), sample(0, 50), sample(HOUR / 2, 55)]);

        assert_rate(shuffled, ordered);
        assert_rate(shuffled, 10.0);
    }

    #[test]
    fn an_extreme_stamp_leaves_the_rate_finite() {
        let trend = Trend::from_samples([
            Sample {
                at: Timestamp::from_unix(i64::MIN),
                level: 10,
            },
            Sample {
                at: Timestamp::from_unix(i64::MAX),
                level: 90,
            },
        ])
        .expect("a trend");

        assert!(trend.rate.is_finite(), "{}", trend.rate);
        assert_eq!(trend.direction, Direction::Rising);
    }

    #[test]
    fn a_direction_names_itself_for_the_ui() {
        assert_eq!(Direction::Rising.to_string(), "rising");
        assert_eq!(Direction::Falling.to_string(), "falling");
        assert_eq!(Direction::Flat.to_string(), "flat");
    }

    #[test]
    fn only_live_levels_are_recorded() {
        let mut history = History::default();

        history.record(&reading(
            0,
            vec![
                device("30-82-16-f2-24-90", Some(80), true),
                device("de-df-38-f0-46-9b", Some(45), false),
                device("aa-bb-cc-dd-ee-ff", None, true),
            ],
        ));

        assert_eq!(history.samples(&trackpad()).count(), 1);
        assert_eq!(
            history.samples(&address("de-df-38-f0-46-9b")).count(),
            0,
            "a disconnected level is last seen, not a reading"
        );
        assert_eq!(history.samples(&address("aa-bb-cc-dd-ee-ff")).count(), 0);
    }

    #[test]
    fn a_device_records_one_sample_per_reading_and_trends_over_them() {
        let mut history = History::default();

        for (at, level) in [(0, 60), (HOUR, 55), (2 * HOUR, 50)] {
            history.record(&reading(
                at,
                vec![device("30-82-16-f2-24-90", Some(level), true)],
            ));
        }

        assert_eq!(levels(&history), [60, 55, 50], "oldest first");
        assert_eq!(
            history
                .samples(&trackpad())
                .next_back()
                .map(|sample| sample.level),
            Some(50),
            "and reversible for the sparkline"
        );
        assert_rate(history.trend(&trackpad()).expect("a trend").rate, -5.0);
    }

    #[test]
    fn a_device_with_no_samples_has_no_trend() {
        let history = History::default();

        assert_eq!(history.trend(&trackpad()), None);
        assert_eq!(history.samples(&trackpad()).count(), 0);
    }

    #[test]
    fn a_full_ring_forgets_its_oldest_sample() {
        let mut history = History::new(3);

        for (at, level) in [(0, 100), (HOUR, 90), (2 * HOUR, 80), (3 * HOUR, 70)] {
            history.record(&reading(
                at,
                vec![device("30-82-16-f2-24-90", Some(level), true)],
            ));
        }

        assert_eq!(levels(&history), [90, 80, 70]);
        assert_rate(history.trend(&trackpad()).expect("a trend").rate, -10.0);
    }

    #[test]
    fn a_capacity_of_none_still_keeps_the_latest_reading() {
        let mut history = History::new(0);

        for (at, level) in [(0, 100), (HOUR, 90)] {
            history.record(&reading(at, vec![device(TRACKPAD, Some(level), true)]));
        }

        assert_eq!(levels(&history), [90]);
    }

    #[test]
    fn the_default_ring_keeps_the_number_of_samples_it_documents() {
        let mut history = History::default();

        for at in 0..=(History::DEFAULT_CAPACITY as i64) {
            history.record(&reading(at, vec![device(TRACKPAD, Some(50), true)]));
        }

        assert_eq!(
            history.samples(&trackpad()).count(),
            History::DEFAULT_CAPACITY
        );
    }

    #[test]
    fn a_device_reread_at_the_stamp_it_already_carries_is_sampled_once() {
        let mut history = History::default();
        let cached = Device {
            read_at: Timestamp::from_unix(0),
            ..device(TRACKPAD, Some(80), true)
        };

        for at in [0, 5, 10, 15] {
            history.record(&Snapshot {
                read_at: Timestamp::from_unix(at),
                devices: vec![cached.clone()],
                warnings: Vec::new(),
            });
        }

        assert_eq!(
            levels(&history),
            [80],
            "a cached level held over is one reading, however often it is reused"
        );
        assert_eq!(
            history.trend(&trackpad()),
            None,
            "and one reading is nothing to measure a rate over"
        );
    }

    #[test]
    fn a_reading_older_than_the_last_one_leaves_the_series_oldest_first() {
        let mut history = History::default();

        history.record(&reading(HOUR, vec![device(TRACKPAD, Some(50), true)]));
        history.record(&reading(0, vec![device(TRACKPAD, Some(90), true)]));
        history.record(&reading(2 * HOUR, vec![device(TRACKPAD, Some(40), true)]));

        assert_eq!(
            levels(&history),
            [50, 40],
            "a stamp that steps back is dropped"
        );
        assert!(
            history
                .samples(&trackpad())
                .is_sorted_by_key(|sample| sample.at)
        );
    }
}
