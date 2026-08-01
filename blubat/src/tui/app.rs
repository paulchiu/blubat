//! Dashboard state, and the one function allowed to change it.
//!
//! Everything here is pure: `update` takes the state by value and hands the
//! next state back, so no keypress, reading or clock tick can reach the
//! terminal or the poller from inside it. That is what lets the whole state
//! machine be tested by calling one function.

use std::time::Duration;

use blubat_core::{Device, Snapshot, Timestamp};

/// One advertised key: what to press, and what pressing it does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Binding {
    /// The keys as the footer prints them, several separated by `/`.
    pub keys: &'static str,
    pub label: &'static str,
}

/// The dashboard keymap, in the order the footer and the overlay list it.
pub const KEYMAP: [Binding; 3] = [
    Binding {
        keys: "q",
        label: "quit",
    },
    Binding {
        keys: "j/k",
        label: "move",
    },
    Binding {
        keys: "?",
        label: "help",
    },
];

/// The keys that stay live while the keymap overlay covers the dashboard.
const OVERLAY_KEYS: [Binding; 2] = [
    Binding {
        keys: "?",
        label: "close",
    },
    Binding {
        keys: "q",
        label: "quit",
    },
];

/// What a bound key does to the dashboard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Quit,
    Down,
    Up,
    ToggleKeymap,
}

impl Action {
    /// The action a key performs, absent for a key the dashboard does not bind.
    pub fn of(key: char) -> Option<Self> {
        match key {
            'q' => Some(Action::Quit),
            'j' => Some(Action::Down),
            'k' => Some(Action::Up),
            '?' => Some(Action::ToggleKeymap),
            _ => None,
        }
    }
}

/// Everything the dashboard reacts to, whichever source it came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Key(char),
    /// A fresh reading from the poller.
    Reading(Snapshot),
    /// The redraw timer expired at this moment.
    Tick(Timestamp),
}

/// Everything the dashboard draws, and nothing else.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct App {
    /// The last reading, absent until the first poll lands.
    pub reading: Option<Snapshot>,
    /// The row the selection sits on, always a real index while there are rows.
    pub selected: usize,
    pub keymap_open: bool,
    /// Cleared by `q`, which is how the loop learns to stop.
    pub running: bool,
    /// The latest moment the clock reported, which the countdown measures from.
    pub now: Timestamp,
    /// How often the poller reads, which fixes when the next reading is due.
    pub interval: Duration,
}

impl App {
    pub fn new(interval: Duration, now: Timestamp) -> Self {
        Self {
            reading: None,
            selected: 0,
            keymap_open: false,
            running: true,
            now,
            interval,
        }
    }

    /// The devices of the last reading, empty before the first one lands.
    pub fn devices(&self) -> &[Device] {
        self.reading
            .as_ref()
            .map_or(&[], |reading| reading.devices.as_slice())
    }

    /// What the sources could not use, for the status line to place.
    pub fn warnings(&self) -> &[String] {
        self.reading
            .as_ref()
            .map_or(&[], |reading| reading.warnings.as_slice())
    }

    /// How long until the next reading is due, absent before the first one.
    ///
    /// Never negative: a reading that is already overdue reads as due now
    /// rather than as a time in the past.
    pub fn next_poll_in(&self) -> Option<Duration> {
        let taken_at = self.reading.as_ref()?.read_at.unix();
        let interval = i64::try_from(self.interval.as_secs()).unwrap_or(i64::MAX);
        let remaining = taken_at.saturating_add(interval) - self.now.unix();

        Some(Duration::from_secs(u64::try_from(remaining).unwrap_or(0)))
    }

    /// The keys the current view binds, which the footer shows.
    pub fn keys(&self) -> &'static [Binding] {
        if self.keymap_open {
            &OVERLAY_KEYS
        } else {
            &KEYMAP
        }
    }
}

/// The whole state machine: one event in, the next state out.
pub fn update(app: App, event: Event) -> App {
    match event {
        Event::Key(key) => match Action::of(key) {
            Some(action) => act(app, action),
            None => app,
        },
        Event::Reading(reading) => receive(app, reading),
        Event::Tick(now) => App { now, ..app },
    }
}

fn act(app: App, action: Action) -> App {
    match action {
        Action::Quit => App {
            running: false,
            ..app
        },
        Action::Down => moved(app, 1),
        Action::Up => moved(app, -1),
        Action::ToggleKeymap => App {
            keymap_open: !app.keymap_open,
            ..app
        },
    }
}

/// Moves the selection by `step` rows, stopping at either end.
///
/// Clamped rather than wrapping, so holding `j` settles on the last device
/// instead of cycling back to the top.
fn moved(app: App, step: isize) -> App {
    let last = app.devices().len().saturating_sub(1);
    let selected = app.selected.saturating_add_signed(step).min(last);

    App { selected, ..app }
}

/// Takes a fresh reading, keeping the selection on a row that still exists.
///
/// A reading is delivered as it is taken, so it carries the clock forward too
/// and the countdown restarts from the moment the reading actually happened.
fn receive(app: App, reading: Snapshot) -> App {
    let selected = app.selected.min(reading.devices.len().saturating_sub(1));
    let now = reading.read_at;

    App {
        reading: Some(reading),
        selected,
        now,
        ..app
    }
}

#[cfg(test)]
pub(super) mod tests {
    use blubat_core::{Address, ChargeState, Levels, Source};

    use super::*;

    const READ_AT: Timestamp = Timestamp::from_unix(1_785_643_199);
    const INTERVAL: Duration = Duration::from_secs(5);

    /// A device that differs from its neighbours only where a test looks.
    pub fn device(name: &str, address: &str, level: Option<u8>) -> Device {
        Device {
            address: Address::parse(address).expect("valid address"),
            name: name.to_string(),
            kind: None,
            transport: None,
            levels: Levels {
                main: level,
                ..Levels::default()
            },
            charge: ChargeState::Discharging,
            source: Source::IoKit,
            connected: true,
            read_at: READ_AT,
        }
    }

    pub fn reading(devices: Vec<Device>) -> Snapshot {
        Snapshot {
            read_at: READ_AT,
            devices,
            warnings: Vec::new(),
        }
    }

    /// Three devices, which is enough for both ends of the selection.
    pub fn three_devices() -> Snapshot {
        reading(vec![
            device("Magic Trackpad", "30-82-16-f2-24-90", Some(85)),
            device("MX Keys M Mac", "de-df-38-f0-46-9b", Some(42)),
            device("Soundcore Liberty", "d0-03-4b-0b-e6-4e", None),
        ])
    }

    pub fn app() -> App {
        App::new(INTERVAL, READ_AT)
    }

    /// An app holding a reading, which is the state most tests start from.
    pub fn loaded() -> App {
        update(app(), Event::Reading(three_devices()))
    }

    fn press(app: App, keys: &str) -> App {
        keys.chars()
            .fold(app, |app, key| update(app, Event::Key(key)))
    }

    #[test]
    fn a_dashboard_starts_running_with_nothing_to_show() {
        let app = app();

        assert!(app.running);
        assert!(app.devices().is_empty());
        assert_eq!(app.next_poll_in(), None, "nothing has been read yet");
        assert!(!app.keymap_open);
    }

    #[test]
    fn a_reading_replaces_the_devices_and_carries_the_clock() {
        let app = update(app(), Event::Tick(Timestamp::from_unix(0)));
        let app = update(app, Event::Reading(three_devices()));

        assert_eq!(app.devices().len(), 3);
        assert_eq!(app.now, READ_AT);
        assert_eq!(app.next_poll_in(), Some(INTERVAL), "a full interval to go");
    }

    #[test]
    fn the_selection_moves_and_stops_at_either_end() {
        let app = loaded();

        assert_eq!(press(app.clone(), "j").selected, 1);
        assert_eq!(press(app.clone(), "jj").selected, 2);
        assert_eq!(press(app.clone(), "jjjjj").selected, 2, "stops at the last");
        assert_eq!(press(app.clone(), "jjk").selected, 1);
        assert_eq!(press(app, "k").selected, 0, "stops at the first");
    }

    #[test]
    fn the_selection_cannot_leave_an_empty_dashboard() {
        assert_eq!(press(app(), "jjkk").selected, 0);
    }

    #[test]
    fn a_shorter_reading_pulls_the_selection_back_onto_a_row() {
        let app = press(loaded(), "jj");
        let app = update(
            app,
            Event::Reading(reading(vec![device(
                "Magic Trackpad",
                "30-82-16-f2-24-90",
                Some(85),
            )])),
        );

        assert_eq!(app.selected, 0);
    }

    #[test]
    fn the_keymap_toggles_and_takes_the_footer_with_it() {
        let closed = loaded();
        let open = press(closed.clone(), "?");

        assert!(open.keymap_open);
        assert_eq!(open.keys()[0].label, "close");
        assert!(!press(open, "?").keymap_open);
        assert_eq!(closed.keys(), KEYMAP, "the dashboard advertises its keymap");
    }

    #[test]
    fn q_is_the_only_way_the_loop_is_asked_to_stop() {
        assert!(!press(loaded(), "q").running);
        assert!(press(loaded(), "jk?").running);
    }

    #[test]
    fn an_unbound_key_changes_nothing_at_all() {
        let app = loaded();

        assert_eq!(press(app.clone(), "xyz1"), app);
        assert_eq!(Action::of('z'), None);
    }

    #[test]
    fn the_countdown_runs_down_and_stops_at_due() {
        let app = loaded();
        let at = |second| {
            update(app.clone(), Event::Tick(Timestamp::from_unix(second)))
                .next_poll_in()
                .expect("a reading has landed")
        };

        assert_eq!(at(READ_AT.unix() + 2), Duration::from_secs(3));
        assert_eq!(at(READ_AT.unix() + 5), Duration::ZERO);
        assert_eq!(
            at(READ_AT.unix() + 90),
            Duration::ZERO,
            "overdue reads as 0"
        );
    }

    #[test]
    fn every_advertised_key_is_bound_to_an_action() {
        for binding in KEYMAP.iter().chain(&OVERLAY_KEYS) {
            for key in binding.keys.split('/') {
                let key = key.chars().next().expect("a key to press");

                assert!(Action::of(key).is_some(), "{key} is advertised unbound");
            }
        }
    }

    #[test]
    fn warnings_travel_with_the_reading_they_belong_to() {
        let app = update(
            app(),
            Event::Reading(Snapshot {
                warnings: vec!["system_profiler exited with 1".to_string()],
                ..three_devices()
            }),
        );

        assert_eq!(app.warnings(), ["system_profiler exited with 1"]);
    }
}
