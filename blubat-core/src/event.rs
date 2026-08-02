//! The six events blubat raises, the vocabulary the config file, the
//! notifications and the hooks all share.

use std::fmt;

use serde::{Deserialize, Serialize};

/// What happened to a device, as an event name rather than an event.
///
/// The engine pairs one of these with the device and the levels that produced
/// it; this type is the kind alone, which is what a `[[hook]]` selects on and
/// what `[notifications]` switches.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Event {
    LowBattery,
    CriticalBattery,
    /// The level reached the charged threshold: safe to unplug.
    Charged,
    Connected,
    Disconnected,
    /// No reading for the stale window, raised once and cleared silently.
    Stale,
}

impl Event {
    /// Every event, which is the list a frontend enumerates.
    pub const ALL: [Event; 6] = [
        Event::LowBattery,
        Event::CriticalBattery,
        Event::Charged,
        Event::Connected,
        Event::Disconnected,
        Event::Stale,
    ];

    /// The name the config file and the `BLUBAT_EVENT` variable use.
    pub fn as_str(self) -> &'static str {
        match self {
            Event::LowBattery => "low_battery",
            Event::CriticalBattery => "critical_battery",
            Event::Charged => "charged",
            Event::Connected => "connected",
            Event::Disconnected => "disconnected",
            Event::Stale => "stale",
        }
    }
}

impl fmt::Display for Event {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_event_prints_as_the_name_the_config_file_writes() {
        for event in Event::ALL {
            let quoted = format!("\"{event}\"");

            assert_eq!(
                serde_json::to_string(&event).expect("serialisable"),
                quoted,
                "{event} serialises as it prints"
            );
            assert_eq!(
                serde_json::from_str::<Event>(&quoted).expect("parses"),
                event
            );
        }
    }

    #[test]
    fn the_six_events_are_the_documented_six() {
        let names: Vec<&str> = Event::ALL.iter().map(|event| event.as_str()).collect();

        assert_eq!(
            names,
            [
                "low_battery",
                "critical_battery",
                "charged",
                "connected",
                "disconnected",
                "stale"
            ]
        );
    }

    #[test]
    fn an_unknown_event_name_is_rejected() {
        assert!(serde_json::from_str::<Event>("\"exploded\"").is_err());
        assert!(serde_json::from_str::<Event>("\"low\"").is_err());
    }
}
