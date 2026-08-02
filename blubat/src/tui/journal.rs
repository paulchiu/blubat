//! A short tail of what the engine raised, kept per device for the detail view.
//!
//! In memory and for this run only, like the level history it sits beside: the
//! panel it feeds answers what has just happened to one device, so it holds the
//! last few events rather than everything since the machine booted.

use std::collections::{BTreeMap, VecDeque};

use blubat_core::{Address, Raised};

/// The recent events of every device blubat has raised anything for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Journal {
    devices: BTreeMap<Address, VecDeque<Raised>>,
}

impl Journal {
    /// Events kept per device, comfortably more than the panel has room for.
    pub const CAPACITY: usize = 8;

    /// Keeps everything one reading raised, oldest first within each device.
    pub fn record(&mut self, raised: impl IntoIterator<Item = Raised>) {
        for raised in raised {
            let events = self.devices.entry(raised.address.clone()).or_default();

            while events.len() >= Self::CAPACITY {
                events.pop_front();
            }
            events.push_back(raised);
        }
    }

    /// One device's events, newest first, empty for a device that raised none.
    pub fn recent(&self, address: &Address) -> impl Iterator<Item = &Raised> {
        self.devices.get(address).into_iter().flatten().rev()
    }
}

#[cfg(test)]
mod tests {
    use blubat_core::{ChargeState, Event, Source, Timestamp};

    use super::*;

    const TRACKPAD: &str = "30-82-16-f2-24-90";
    const KEYS: &str = "de-df-38-f0-46-9b";

    fn address(raw: &str) -> Address {
        Address::parse(raw).expect("valid address")
    }

    fn raised(raw: &str, event: Event, level: u8) -> Raised {
        Raised {
            event,
            device: "Magic Trackpad".to_string(),
            address: address(raw),
            level: Some(level),
            previous: None,
            charge: ChargeState::Discharging,
            source: Source::IoKit,
            threshold: Some(20),
            cycle: 0,
            at: Timestamp::from_unix(i64::from(level)),
        }
    }

    fn levels(journal: &Journal, raw: &str) -> Vec<Option<u8>> {
        journal
            .recent(&address(raw))
            .map(|raised| raised.level)
            .collect()
    }

    #[test]
    fn a_device_that_raised_nothing_has_nothing_to_show() {
        let journal = Journal::default();

        assert_eq!(journal.recent(&address(TRACKPAD)).count(), 0);
    }

    #[test]
    fn the_newest_event_leads_the_list() {
        let mut journal = Journal::default();

        journal.record([raised(TRACKPAD, Event::LowBattery, 19)]);
        journal.record([raised(TRACKPAD, Event::CriticalBattery, 9)]);

        assert_eq!(levels(&journal, TRACKPAD), [Some(9), Some(19)]);
    }

    #[test]
    fn each_device_keeps_its_own_events() {
        let mut journal = Journal::default();

        journal.record([
            raised(TRACKPAD, Event::LowBattery, 19),
            raised(KEYS, Event::Charged, 100),
        ]);

        assert_eq!(levels(&journal, TRACKPAD), [Some(19)]);
        assert_eq!(levels(&journal, KEYS), [Some(100)]);
    }

    #[test]
    fn a_full_ring_forgets_its_oldest_event() {
        let mut journal = Journal::default();

        for level in 0..=u8::try_from(Journal::CAPACITY).expect("a small capacity") {
            journal.record([raised(TRACKPAD, Event::LowBattery, level)]);
        }

        let kept = levels(&journal, TRACKPAD);
        assert_eq!(kept.len(), Journal::CAPACITY);
        assert_eq!(kept.first().copied(), Some(Some(8)), "the newest is kept");
        assert_eq!(kept.last().copied(), Some(Some(1)), "and the first is gone");
    }
}
