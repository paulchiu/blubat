//! One classification of a device per frame, and the views read it.
//!
//! Every view says something about the same device: the state column its
//! badge, the name and gutter its alert, the status line its count, the detail
//! view its link and staleness. Each used to re-derive its piece from the
//! device, the config and the clock; [`App::status`] derives the lot once, so
//! the row, the count and the detail view cannot come to disagree.

use blubat_core::{ChargeState, Device};

use super::app::App;
use super::theme;

/// Whether the level on show is a live reading or what macOS last persisted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Link {
    /// Connected: the charge state is the reading's own.
    Live(ChargeState),
    /// Disconnected: the level is history, never an alert.
    LastSeen,
}

/// The one badge the state column shows for a device.
///
/// A device no source has a level for is unreported rather than absent; one
/// that has stopped reporting is stale, which is the same rule the `stale`
/// event is raised by; and a disconnected one's level is labelled last seen,
/// since macOS keeps reporting it with no timestamp long after the device went
/// away.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Badge {
    Unreported,
    Stale,
    LastSeen,
    Charging,
    /// Discharging or unknown, printed via [`ChargeState`]'s `Display`.
    /// Never carries [`ChargeState::Charging`]: that is [`Badge::Charging`].
    Doing(ChargeState),
}

/// Everything any view says about one device, derived once per row per frame.
///
/// The invariants the views used to keep by convention, written down once:
///
/// 1. [`Status::badge`] resolves by fixed precedence, unreported over stale
///    over last seen over charging over doing, first arm winning. The ordering
///    is part of the interface.
/// 2. `alerting` implies the link is live: a last seen or unreported device
///    never alerts.
/// 3. `alerting` is independent of the badge: a device whose badge says
///    charging or stale can still alert.
/// 4. `stale` is the raw clock fact, true even while the badge says
///    unreported, which is what the detail view's stale marker reads.
/// 5. A `Status` is a per frame snapshot: computed once per row and handed
///    down, never stored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Status {
    /// Live or last seen; the label text follows it.
    pub link: Link,
    /// Quiet longer than `poll.stale_after` against the frame's clock.
    pub stale: bool,
    /// Some source has a battery level for this device.
    pub reported: bool,
    /// Live level below the device's own critical threshold: the same test the
    /// event engine raises `critical_battery` by. Drives the gutter mark, the
    /// red name, the trend colour and the alert count.
    pub alerting: bool,
}

impl Status {
    /// The single winning badge, derived from the fields so the two can never
    /// drift apart.
    pub fn badge(self) -> Badge {
        if !self.reported {
            Badge::Unreported
        } else if self.stale {
            Badge::Stale
        } else {
            match self.link {
                Link::LastSeen => Badge::LastSeen,
                Link::Live(ChargeState::Charging) => Badge::Charging,
                Link::Live(charge) => Badge::Doing(charge),
            }
        }
    }
}

impl App {
    /// The one classification of a device this frame.
    ///
    /// Pure over the device, the config, the advertised thresholds and the
    /// frame's clock: same frame, same answer. Owns everything a view used to
    /// work out for itself: what counts as unreported, the stale clock, the
    /// connected split, and the rule that only a live level can alert.
    ///
    /// [`Engine::step`] resolves the same thresholds and judges the same stale
    /// clock in the core. Should a consumer beyond the dashboard ever need
    /// this classification, the move is a pure
    /// `Status::of(device, thresholds, stale_after, now)` in the core,
    /// mirroring `Engine::judge`'s arguments; folding [`theme::is_critical`]
    /// into the core's `Thresholds` alongside it would let the engine converge
    /// on the same construction. One consumer does not earn that yet.
    ///
    /// [`Engine::step`]: blubat_core::Engine::step
    pub fn status(&self, device: &Device) -> Status {
        Status {
            link: if device.connected {
                Link::Live(device.charge)
            } else {
                Link::LastSeen
            },
            stale: device.is_stale(self.config.poll.stale_after, self.now),
            reported: device.has_battery(),
            alerting: device.connected
                && theme::is_critical(device.active_level(), self.thresholds(device)),
        }
    }
}

#[cfg(test)]
mod tests {
    use blubat_core::{Advertised, AdvertisedThresholds, Timestamp};

    use super::super::app::tests::{READ_AT, app, device};
    use super::*;

    /// Two days past the reading, which is quiet by any configured window.
    fn later(app: App) -> App {
        App {
            now: Timestamp::from_unix(READ_AT.unix() + 172_800),
            ..app
        }
    }

    fn status(fields: (Link, bool, bool, bool)) -> Status {
        let (link, stale, reported, alerting) = fields;

        Status {
            link,
            stale,
            reported,
            alerting,
        }
    }

    #[test]
    fn the_badge_resolves_by_fixed_precedence() {
        use ChargeState::{Charging, Discharging};

        let cases = [
            (
                (Link::LastSeen, true, false, false),
                Badge::Unreported,
                "unreported beats stale",
            ),
            (
                (Link::LastSeen, true, true, false),
                Badge::Stale,
                "stale beats last seen",
            ),
            (
                (Link::Live(Charging), false, true, true),
                Badge::Charging,
                "charging beats the alert colouring",
            ),
            (
                (Link::Live(Discharging), false, true, true),
                Badge::Doing(Discharging),
                "an alert never changes the badge",
            ),
            (
                (Link::Live(Charging), false, true, false),
                Badge::Charging,
                "doing never carries charging",
            ),
        ];

        for (fields, badge, why) in cases {
            assert_eq!(status(fields).badge(), badge, "{why}");
        }
    }

    #[test]
    fn an_unreported_quiet_device_is_unreported_first_and_stale_in_fact() {
        let judged = later(app()).status(&device("Soundcore Liberty", "d0-03-4b-0b-e6-4e", None));

        assert_eq!(judged.badge(), Badge::Unreported);
        assert!(
            judged.stale,
            "the clock fact stays readable under the badge"
        );
        assert!(!judged.alerting, "no level, nothing to alert on");
    }

    #[test]
    fn a_stale_disconnected_device_reads_stale_rather_than_last_seen() {
        let gone = Device {
            connected: false,
            ..device("AirPods Pro", "74-15-f5-02-8e-38", Some(45))
        };
        let judged = later(app()).status(&gone);

        assert_eq!(judged.badge(), Badge::Stale);
        assert_eq!(judged.link, Link::LastSeen);
    }

    #[test]
    fn a_charging_device_below_threshold_shows_charging_and_still_alerts() {
        let plugged = Device {
            charge: ChargeState::Charging,
            ..device("Magic Trackpad", "30-82-16-f2-24-90", Some(5))
        };
        let judged = app().status(&plugged);

        assert_eq!(judged.badge(), Badge::Charging);
        assert!(judged.alerting, "the badge does not mask the alert");
    }

    #[test]
    fn a_stale_live_low_device_reads_stale_and_still_alerts() {
        let judged = later(app()).status(&device("Magic Trackpad", "30-82-16-f2-24-90", Some(5)));

        assert_eq!(judged.badge(), Badge::Stale);
        assert!(judged.alerting);
    }

    #[test]
    fn a_disconnected_low_level_is_history_rather_than_an_alert() {
        let gone = Device {
            connected: false,
            ..device("AirPods Pro", "74-15-f5-02-8e-38", Some(4))
        };
        let judged = app().status(&gone);

        assert_eq!(judged.badge(), Badge::LastSeen);
        assert!(!judged.alerting, "a last seen level can be arbitrarily old");
    }

    #[test]
    fn the_alerting_edge_sits_strictly_under_the_critical_threshold() {
        let at = |level| {
            app()
                .status(&device("MX Keys M Mac", "de-df-38-f0-46-9b", Some(level)))
                .alerting
        };

        assert!(!at(10), "on the built-in threshold is not under it");
        assert!(at(9));
    }

    #[test]
    fn an_advertised_threshold_moves_the_alerting_edge() {
        let keys = device("MX Keys M Mac", "de-df-38-f0-46-9b", Some(42));
        let advertised = App {
            advertised: AdvertisedThresholds::from([(
                keys.address.clone(),
                Advertised {
                    low: Some(60),
                    critical: Some(45),
                },
            )]),
            ..app()
        };

        assert!(!app().status(&keys).alerting, "not under the built-in 10");
        assert!(
            advertised.status(&keys).alerting,
            "Apple's number, in the absence of a file"
        );
    }
}
