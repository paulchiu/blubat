//! Dashboard state, and the one function allowed to change it.
//!
//! Everything here is pure: `update` takes the state by value and hands the
//! next state back, so no keypress, reading or clock tick can reach the
//! terminal or the poller from inside it. That is what lets the whole state
//! machine be tested by calling one function.

use std::time::Duration;

use blubat_core::{
    Advertised, AdvertisedThresholds, Config, Device, History, Raised, Snapshot, Thresholds,
    Timestamp,
};

use super::journal::Journal;
use super::theme::{self, Look};
use super::view::{Filter, Rows, View};

/// One advertised key: what to press, and what pressing it does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Binding {
    /// The keys as the footer prints them, several separated by `/`.
    pub keys: &'static str,
    pub label: &'static str,
}

/// The dashboard keymap, in the order the footer and the overlay list it.
pub const KEYMAP: [Binding; 9] = [
    Binding {
        keys: "q",
        label: "quit",
    },
    Binding {
        keys: "j/k",
        label: "move",
    },
    Binding {
        keys: "enter",
        label: "detail",
    },
    Binding {
        keys: "s",
        label: "sort",
    },
    Binding {
        keys: "/",
        label: "filter",
    },
    Binding {
        keys: "h",
        label: "hide",
    },
    Binding {
        keys: "H",
        label: "show hidden",
    },
    Binding {
        keys: "r",
        label: "reload",
    },
    Binding {
        keys: "?",
        label: "help",
    },
];

/// The key a filter that is no longer being typed still answers to.
///
/// Advertised beside the dashboard keymap while a kept filter is narrowing the
/// table, since that is the only state in which it does anything.
const CLEAR_FILTER: Binding = Binding {
    keys: "esc",
    label: "clear filter",
};

/// The keys the detail view binds, which are the only ones it leaves live.
pub const DETAIL_KEYS: [Binding; 2] = [
    Binding {
        keys: "esc/enter",
        label: "back",
    },
    Binding {
        keys: "q",
        label: "quit",
    },
];

/// What [`DETAIL_KEYS`] stand for, and the only actions the detail view performs.
///
/// Read from either end as [`OVERLAY_ACTIONS`] is, which is what keeps the
/// footer inside the detail view an account of what pressing something will do.
const DETAIL_ACTIONS: [Action; 3] = [Action::Back, Action::Detail, Action::Quit];

/// What the overlay says beyond the keys themselves.
pub const NOTES: [&str; 4] = [
    "the detail chart is this run only; a restart starts it empty.",
    "h lasts: it is the one write blubat makes to the config file.",
    "a hidden device is hidden here only, never unpaired from macOS.",
    "r re-reads the config file; one it cannot read changes nothing.",
];

/// The keys the keymap overlay leaves live, since it swallows every other one.
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

/// What [`OVERLAY_KEYS`] stand for, and the only actions the overlay performs.
///
/// The advertised keys and the accepted actions are one list read from either
/// end, which is what keeps the footer an account of what the overlay does.
const OVERLAY_ACTIONS: [Action; 2] = [Action::ToggleKeymap, Action::Quit];

/// The keys that mean something while the filter is being typed.
///
/// Every other key is text, which is what makes the filter narrow the table as
/// it is typed rather than when it is submitted.
const FILTER_KEYS: [Binding; 2] = [
    Binding {
        keys: "esc",
        label: "clear",
    },
    Binding {
        keys: "enter",
        label: "keep",
    },
];

/// Which of the dashboard's views has the keyboard.
///
/// Exactly one at a time, so the footer and the dispatcher read the same value
/// and a state that advertises one set of keys while acting on another cannot
/// be built.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    Dashboard,
    /// The filter has the keyboard, so every printable key is text.
    Filtering,
    /// The keymap overlay covers the dashboard and swallows its keys.
    Keymap,
    /// One device on its own: its history, its thresholds and what it raised.
    Detail,
}

impl Mode {
    /// The overlay opens over whatever is on screen and closes onto the dashboard.
    fn keymap_toggled(self) -> Self {
        match self {
            Mode::Keymap => Mode::Dashboard,
            _ => Mode::Keymap,
        }
    }
}

/// A key as the dashboard binds on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Enter,
    Escape,
    Backspace,
}

/// What a bound key does to the dashboard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Quit,
    Down,
    Up,
    ToggleKeymap,
    CycleSort,
    OpenFilter,
    /// Escape, which backs out of whatever the mode on screen is: the detail
    /// view, or a filter the dashboard is still narrowing by.
    Back,
    ToggleHidden,
    ShowHidden,
    /// Asks the loop to read the config file again, which the reducer cannot.
    Reload,
    /// Enter, which opens the detail view over the selected device and closes
    /// it again from inside.
    Detail,
}

impl Action {
    /// The action a key performs, absent for a key the dashboard does not bind.
    pub fn of(key: Key) -> Option<Self> {
        match key {
            Key::Char('q') => Some(Action::Quit),
            Key::Char('j') => Some(Action::Down),
            Key::Char('k') => Some(Action::Up),
            Key::Char('?') => Some(Action::ToggleKeymap),
            Key::Char('s') => Some(Action::CycleSort),
            Key::Char('/') => Some(Action::OpenFilter),
            Key::Char('h') => Some(Action::ToggleHidden),
            Key::Char('H') => Some(Action::ShowHidden),
            Key::Char('r') => Some(Action::Reload),
            Key::Enter => Some(Action::Detail),
            Key::Escape => Some(Action::Back),
            _ => None,
        }
    }
}

/// A line the dashboard puts up about itself rather than about a device.
///
/// Cleared by the next keypress, since what it reports is what the last one
/// came to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    pub text: String,
    /// Whether it reports a problem, which is drawn in the alert colour.
    pub problem: bool,
}

impl Notice {
    pub fn said(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            problem: false,
        }
    }

    pub fn problem(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            problem: true,
        }
    }
}

/// Everything the dashboard reacts to, whichever source it came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Key(Key),
    /// Ctrl+C, which raw mode leaves to blubat rather than to the terminal.
    Interrupt,
    /// A fresh reading from the poller.
    Reading(Snapshot),
    /// The redraw timer expired at this moment.
    Tick(Timestamp),
    /// What the loop made of the reload [`Action::Reload`] asked for.
    Reloaded(Result<Config, String>),
    /// What came of writing the hidden devices [`Action::ToggleHidden`] changed.
    Saved(Result<(), String>),
    /// What the event engine raised over the reading that just landed.
    Raised(Vec<Raised>),
    /// Something the loop did that the user needs telling about.
    Note(Notice),
}

/// Everything the dashboard draws, and nothing else.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct App {
    /// The last reading, absent until the first poll lands.
    pub reading: Option<Snapshot>,
    /// The levels seen this run, which the trend column and the detail chart read.
    pub history: History,
    /// What each device has raised this run, which the detail view lists.
    pub journal: Journal,
    /// The row the selection sits on, always a real index while there are rows.
    pub selected: usize,
    /// Which view has the keyboard, and so which keys the footer advertises.
    pub mode: Mode,
    /// Cleared by `q`, which is how the loop learns to stop.
    pub running: bool,
    /// The latest moment the clock reported, which the countdown measures from.
    pub now: Timestamp,
    /// How often the poller reads, which fixes when the next reading is due.
    pub interval: Duration,
    /// Which devices are shown, and in what order.
    pub view: View,
    /// The colours and glyphs to draw with, resolved from `config` outside the
    /// reducer since guessing at a terminal's font is not a pure question.
    pub look: Look,
    /// The config in force, which the loop judges each reading against.
    pub config: Config,
    /// What each device publishes about itself, which sits under the config in
    /// the same order the engine resolves thresholds in.
    pub advertised: AdvertisedThresholds,
    /// What the dashboard has to say about itself, if anything.
    pub notice: Option<Notice>,
    /// Set by `r` and cleared by what the loop reads: the reducer cannot touch
    /// a file, so the request travels as state and the answer comes back as an
    /// event.
    pub reload: bool,
    /// Set by `h` and cleared the same way, for the same reason: the hide is
    /// already in [`View::hidden`], and the file has yet to be told.
    pub save_hidden: bool,
}

impl App {
    pub fn new(interval: Duration, now: Timestamp, look: Look, config: Config) -> Self {
        Self {
            reading: None,
            history: History::default(),
            journal: Journal::default(),
            selected: 0,
            mode: Mode::Dashboard,
            running: true,
            now,
            interval,
            view: View::hiding(&config.dashboard.hidden),
            look,
            config,
            advertised: AdvertisedThresholds::new(),
            notice: None,
            reload: false,
            save_hidden: false,
        }
    }

    /// The thresholds one device is judged by, which are also the ones its row
    /// is coloured by.
    pub fn thresholds(&self, device: &Device) -> Thresholds {
        self.config.thresholds_for(
            device,
            self.advertised
                .get(&device.address)
                .copied()
                .unwrap_or(Advertised::NONE),
        )
    }

    /// Whether a device has gone quiet for longer than the config allows.
    ///
    /// The core's own rule against the clock the dashboard is drawing at, so a
    /// row marked stale is one blubat has raised `stale` for rather than one it
    /// merely drew that way.
    pub fn is_stale(&self, device: &Device) -> bool {
        device.is_stale(self.config.poll.stale_after, self.now)
    }

    /// The devices of the last reading, empty before the first one lands.
    pub fn devices(&self) -> &[Device] {
        self.reading
            .as_ref()
            .map_or(&[], |reading| reading.devices.as_slice())
    }

    /// The connected devices of the last reading, whatever the view is showing.
    ///
    /// Read from the reading rather than from the rows, so a filter narrows
    /// what is drawn without changing what the status line counts.
    pub fn connected(&self) -> impl Iterator<Item = &Device> {
        self.devices().iter().filter(|device| device.connected)
    }

    /// Connected devices low enough to want attention.
    ///
    /// A disconnected device can never count: its level is what macOS last
    /// persisted, so it is history rather than an alert.
    pub fn critical(&self) -> usize {
        self.connected()
            .filter(|device| theme::is_critical(device.active_level(), self.thresholds(device)))
            .count()
    }

    /// The devices on screen, which is what the table draws and `j` moves through.
    pub fn rows(&self) -> Rows<'_> {
        Rows::of(self.devices(), &self.view)
    }

    /// The device the selection sits on, absent while nothing is on screen.
    pub fn current(&self) -> Option<&Device> {
        self.rows().get(self.selected)
    }

    /// Whether the last reading is carrying a source's last good answer rather
    /// than a fresh one, which the status line says out loud.
    pub fn degraded(&self) -> bool {
        self.reading
            .as_ref()
            .is_some_and(|reading| reading.degraded)
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

    /// The keys the mode on screen binds, which the footer shows.
    ///
    /// Every key listed here reaches [`Action::of`] in this mode and no key
    /// outside it does anything, which is what makes the footer an account of
    /// what pressing something will do.
    pub fn keys(&self) -> Vec<Binding> {
        match self.mode {
            Mode::Keymap => OVERLAY_KEYS.to_vec(),
            Mode::Filtering => FILTER_KEYS.to_vec(),
            Mode::Detail => DETAIL_KEYS.to_vec(),
            Mode::Dashboard if self.view.filter.narrows() => {
                KEYMAP.iter().copied().chain([CLEAR_FILTER]).collect()
            }
            Mode::Dashboard => KEYMAP.to_vec(),
        }
    }
}

/// The whole state machine: one event in, the next state out.
///
/// A keypress clears whatever the dashboard was saying about itself first: a
/// notice reports what the last key came to, so the next one has read it.
pub fn update(app: App, event: Event) -> App {
    let app = match event {
        Event::Key(key) => pressed(
            App {
                notice: None,
                ..app
            },
            key,
        ),
        Event::Interrupt => act(app, Action::Quit),
        Event::Reading(reading) => receive(app, reading),
        Event::Tick(now) => App { now, ..app },
        Event::Reloaded(read) => reloaded(app, read),
        Event::Saved(written) => saved(app, written),
        Event::Raised(raised) => recorded(app, raised),
        Event::Note(notice) => App {
            notice: Some(notice),
            ..app
        },
    };

    onto_a_row(app)
}

/// Takes the config the loop read, or keeps the one in force and says why.
///
/// The dashboard never exits over a config file. A rejected reload moves
/// nothing but the line reporting it, so the thresholds, theme, glyphs and
/// hooks that were working a moment ago carry on working.
///
/// A reload takes the file's hidden devices too, since `h` writes them there:
/// the file is where hiding lives, so a hand edit is picked up by the key that
/// re-reads it rather than needing a restart.
fn reloaded(mut app: App, read: Result<Config, String>) -> App {
    app.reload = false;

    match read {
        Ok(config) => {
            app.look = app.look.reloaded(&config.theme);
            app.view.hidden = config.dashboard.hidden.clone();
            app.config = config;
            app.notice = Some(Notice::said("config reloaded"));
        }
        Err(problem) => app.notice = Some(Notice::problem(problem)),
    }

    app
}

/// Takes what came of writing the hidden devices to the config file.
///
/// A hide the file would not take is reported and otherwise left alone: the
/// device stays hidden on screen, so the dashboard says what did not survive
/// rather than putting the row back without explanation.
fn saved(mut app: App, written: Result<(), String>) -> App {
    app.save_hidden = false;

    if let Err(problem) = written {
        app.notice = Some(Notice::problem(problem));
    }

    app
}

/// Keeps what the engine raised, which is the detail view's event log.
fn recorded(mut app: App, raised: Vec<Raised>) -> App {
    app.journal.record(raised);

    app
}

/// A key means what the mode it was pressed in says it means.
///
/// The one dispatch [`App::keys`] advertises: the filter takes every key as
/// text, the overlay and the detail view accept only what they list, and the
/// dashboard binds its whole keymap.
fn pressed(app: App, key: Key) -> App {
    match app.mode {
        Mode::Filtering => typed(app, key),
        Mode::Keymap => acted(
            app,
            Action::of(key).filter(|action| OVERLAY_ACTIONS.contains(action)),
        ),
        Mode::Detail => acted(
            app,
            Action::of(key).filter(|action| DETAIL_ACTIONS.contains(action)),
        ),
        Mode::Dashboard => acted(app, Action::of(key)),
    }
}

/// The state after `action`, unchanged where the mode binds nothing to the key.
fn acted(app: App, action: Option<Action>) -> App {
    match action {
        Some(action) => act(app, action),
        None => app,
    }
}

/// Editing the filter, where every printable key narrows the table further.
fn typed(app: App, key: Key) -> App {
    match key {
        Key::Char(character) => viewed(app, |view| view.filter.query.push(character)),
        Key::Backspace => viewed(app, |view| {
            view.filter.query.pop();
        }),
        // Escape abandons the filter altogether; enter keeps what it matched
        // and hands the keys back to the dashboard.
        Key::Escape => cleared(app),
        Key::Enter => App {
            mode: Mode::Dashboard,
            ..app
        },
    }
}

/// Drops the filter and gives the keys back to the dashboard.
fn cleared(app: App) -> App {
    let app = App {
        mode: Mode::Dashboard,
        ..app
    };

    viewed(app, |view| view.filter = Filter::default())
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
            mode: app.mode.keymap_toggled(),
            ..app
        },
        Action::CycleSort => viewed(app, |view| view.sort = view.sort.next()),
        Action::OpenFilter => App {
            mode: Mode::Filtering,
            ..app
        },
        Action::Back => backed(app),
        Action::ToggleHidden => hide_selected(app),
        Action::ShowHidden => viewed(app, |view| view.show_hidden = !view.show_hidden),
        Action::Reload => App {
            reload: true,
            ..app
        },
        Action::Detail => detailed(app),
    }
}

/// Opens the detail view over the selected device, or closes it again.
///
/// One key both ways, as `?` is for the overlay. A dashboard with no row
/// selected has no device to detail, so enter does nothing there rather than
/// opening a view of nothing.
fn detailed(app: App) -> App {
    match app.mode {
        Mode::Detail => onto_the_dashboard(app),
        _ if app.current().is_some() => App {
            mode: Mode::Detail,
            ..app
        },
        _ => app,
    }
}

/// Backs out of the detail view, or drops the filter under the dashboard.
///
/// Escape means "leave what is on screen" in both, which is why one action
/// covers them: what is on screen is what the mode already says.
fn backed(app: App) -> App {
    match app.mode {
        Mode::Detail => onto_the_dashboard(app),
        _ => cleared(app),
    }
}

fn onto_the_dashboard(app: App) -> App {
    App {
        mode: Mode::Dashboard,
        ..app
    }
}

/// The same state with `change` applied to what it is showing.
fn viewed(mut app: App, change: impl FnOnce(&mut View)) -> App {
    change(&mut app.view);

    app
}

/// Moves the selection by `step` rows, stopping at either end.
///
/// Never wraps: the step saturates at the first row and `onto_a_row` catches
/// the last, so holding `j` settles on the final device rather than cycling.
fn moved(app: App, step: isize) -> App {
    let selected = app.selected.saturating_add_signed(step);

    App { selected, ..app }
}

/// Hides the selected device, or shows it again if it was already hidden.
///
/// One key both ways, so a device unhidden under `H` goes back with the same
/// press that hid it. Either way the config file has something new to say, and
/// a dashboard with no row selected has nothing to hide and so nothing to write.
fn hide_selected(app: App) -> App {
    let Some(device) = app.current().cloned() else {
        return app;
    };
    let app = App {
        save_hidden: true,
        ..app
    };

    viewed(app, |view| view.toggle_hidden(&device))
}

/// Takes a fresh reading, recording the levels the trend column reads.
///
/// A reading is delivered as it is taken, so it carries the clock forward too
/// and the countdown restarts from the moment the reading actually happened.
fn receive(mut app: App, reading: Snapshot) -> App {
    app.history.record(&reading);
    app.now = reading.read_at;
    app.reading = Some(reading);

    app
}

/// Pulls the selection back onto a row that exists, and the detail view with it.
///
/// Every way the table can shrink ends here, so a filter, a hide and a shorter
/// reading all leave the selection somewhere real without each having to say
/// so. The detail view draws whichever device the selection is on, so a table
/// that has shrunk out from under it leaves it on the device that took the
/// row, and one that has emptied leaves it nothing to draw at all: that is the
/// one case it backs out to the dashboard on its own.
fn onto_a_row(app: App) -> App {
    let last = app.rows().len().saturating_sub(1);
    let app = App {
        selected: app.selected.min(last),
        ..app
    };

    match app.mode {
        Mode::Detail if app.current().is_none() => onto_the_dashboard(app),
        _ => app,
    }
}

#[cfg(test)]
pub(super) mod tests {
    use blubat_core::{Address, ChargeState, Levels, Source, Theme};

    use super::super::glyph::Glyphs;
    use super::super::theme::Palette;
    use super::*;

    pub const READ_AT: Timestamp = Timestamp::from_unix(1_785_643_199);
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
            degraded: false,
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
        App::new(
            INTERVAL,
            READ_AT,
            Look::of(&Theme::default(), Glyphs::ASCII),
            Config::default(),
        )
    }

    /// A parsed config, for the reloads that have to land somewhere visible.
    pub fn config(written: &str) -> Config {
        Config::parse(written).expect("the test config parses")
    }

    /// An app holding a reading, which is the state most tests start from.
    pub fn loaded() -> App {
        update(app(), Event::Reading(three_devices()))
    }

    /// Presses each character of `keys` in turn, as a person would type them.
    pub fn press(app: App, keys: &str) -> App {
        keys.chars()
            .fold(app, |app, key| update(app, Event::Key(Key::Char(key))))
    }

    pub fn key(app: App, key: Key) -> App {
        update(app, Event::Key(key))
    }

    fn names(app: &App) -> Vec<String> {
        app.rows().all().map(|device| device.name.clone()).collect()
    }

    /// One state per set of keys the footer can carry.
    fn every_view() -> Vec<App> {
        vec![
            loaded(),
            press(loaded(), "?"),
            press(loaded(), "/key"),
            key(press(loaded(), "/key"), Key::Enter),
            key(loaded(), Key::Enter),
        ]
    }

    /// Every key a person can reach, bound or not.
    fn every_key() -> Vec<Key> {
        "qjksh H/?rxz1"
            .chars()
            .map(Key::Char)
            .chain([Key::Enter, Key::Escape, Key::Backspace])
            .collect()
    }

    /// Every key the footer of `app` names, as it would be pressed.
    fn advertised_in(app: &App) -> Vec<Key> {
        let pressed = |text: &str| match text {
            "esc" => Key::Escape,
            "enter" => Key::Enter,
            other => Key::Char(other.chars().next().expect("a key to press")),
        };

        app.keys()
            .iter()
            // `/` is a key of its own as well as the separator between two of them.
            .flat_map(|binding| match binding.keys {
                "/" => vec![Key::Char('/')],
                several => several.split('/').map(pressed).collect(),
            })
            .collect()
    }

    #[test]
    fn a_dashboard_starts_running_with_nothing_to_show() {
        let app = app();

        assert!(app.running);
        assert!(app.devices().is_empty());
        assert_eq!(app.next_poll_in(), None, "nothing has been read yet");
        assert_eq!(app.mode, Mode::Dashboard);
        assert_eq!(app.view, View::default());
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

        assert_eq!(open.mode, Mode::Keymap);
        assert_eq!(open.keys()[0].label, "close");
        assert_eq!(press(open, "?").mode, Mode::Dashboard);
        assert_eq!(closed.keys(), KEYMAP, "the dashboard advertises its keymap");
    }

    #[test]
    fn the_overlay_swallows_every_key_it_does_not_advertise() {
        let open = press(loaded(), "?");

        assert_eq!(
            press(open.clone(), "jksh/H"),
            open,
            "the dashboard keys do nothing underneath the overlay"
        );
        assert_eq!(key(open.clone(), Key::Enter), open);
        assert_eq!(key(open.clone(), Key::Escape), open);

        assert_eq!(
            press(open.clone(), "?").mode,
            Mode::Dashboard,
            "? closes it"
        );
        assert!(!press(open, "q").running, "and q still quits");
    }

    #[test]
    fn a_key_the_footer_advertises_is_never_swallowed_as_filter_text() {
        for app in every_view() {
            for key in advertised_in(&app) {
                let after = update(app.clone(), Event::Key(key));

                assert!(
                    after.view.filter.query.len() <= app.view.filter.query.len(),
                    "{key:?} is advertised in {:?} and types itself instead",
                    app.mode
                );
            }
        }
    }

    #[test]
    fn the_filter_cannot_be_opened_from_underneath_the_overlay() {
        let still_open = press(press(loaded(), "?"), "/");

        assert_eq!(still_open.mode, Mode::Keymap, "one view holds the keys");
        assert!(still_open.view.filter.query.is_empty());
        assert_eq!(
            press(still_open, "?/key").view.filter.query,
            "key",
            "and the filter opens once the overlay is closed"
        );
    }

    #[test]
    fn ctrl_c_stops_the_loop_from_wherever_it_arrives() {
        for app in [loaded(), press(loaded(), "?"), press(loaded(), "/key")] {
            assert!(!update(app, Event::Interrupt).running);
        }
    }

    #[test]
    fn q_is_the_only_way_the_loop_is_asked_to_stop() {
        assert!(!press(loaded(), "q").running);
        assert!(press(loaded(), "jk?sH").running);
    }

    #[test]
    fn an_unbound_key_changes_nothing_at_all() {
        let app = loaded();

        assert_eq!(press(app.clone(), "xyz1"), app);
        assert_eq!(key(app.clone(), Key::Backspace), app);
        assert_eq!(Action::of(Key::Char('z')), None);
        assert_eq!(Action::of(Key::Backspace), None);
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
        for app in every_view() {
            for key in advertised_in(&app) {
                assert!(Action::of(key).is_some(), "{key:?} is advertised unbound");
            }
        }
    }

    /// A modal view's footer is a complete account of it rather than a
    /// selection from a longer list: every key it names moves the state, and
    /// every key it does not is swallowed whole.
    #[test]
    fn a_modal_view_acts_on_its_own_keys_and_swallows_every_other_one() {
        for app in [press(loaded(), "?"), key(loaded(), Key::Enter)] {
            let advertised = advertised_in(&app);

            for key in every_key() {
                let moved = update(app.clone(), Event::Key(key)) != app;

                assert_eq!(
                    moved,
                    advertised.contains(&key),
                    "{key:?} in {:?}",
                    app.mode
                );
            }
        }
    }

    #[test]
    fn warnings_travel_with_the_reading_they_belong_to() {
        let app = update(
            app(),
            Event::Reading(Snapshot {
                degraded: false,
                warnings: vec!["system_profiler exited with 1".to_string()],
                ..three_devices()
            }),
        );

        assert_eq!(app.warnings(), ["system_profiler exited with 1"]);
    }

    #[test]
    fn s_cycles_the_order_the_table_is_listed_in() {
        let by_level = loaded();

        assert_eq!(by_level.view.sort.label(), "level");
        assert_eq!(press(by_level.clone(), "s").view.sort.label(), "name");
        assert_eq!(press(by_level.clone(), "ss").view.sort.label(), "last seen");
        assert_eq!(
            press(by_level.clone(), "sss").view.sort,
            by_level.view.sort,
            "three presses come back round"
        );
        assert_eq!(
            names(&press(by_level, "s")),
            ["Magic Trackpad", "MX Keys M Mac", "Soundcore Liberty"]
        );
    }

    #[test]
    fn the_filter_narrows_the_table_as_it_is_typed() {
        let filtering = press(loaded(), "/key");

        assert_eq!(filtering.mode, Mode::Filtering);
        assert_eq!(filtering.view.filter.query, "key");
        assert_eq!(names(&filtering), ["MX Keys M Mac"]);
    }

    #[test]
    fn a_key_bound_on_the_dashboard_is_text_while_the_filter_is_typed() {
        let filtering = press(loaded(), "/qs");

        assert!(filtering.running, "q types rather than quits");
        assert_eq!(filtering.view.filter.query, "qs");
        assert_eq!(filtering.view.sort.label(), "level", "and s types too");
    }

    #[test]
    fn backspace_takes_back_one_character_of_the_filter() {
        let filtering = key(press(loaded(), "/keys"), Key::Backspace);

        assert_eq!(filtering.view.filter.query, "key");
        assert_eq!(names(&filtering), ["MX Keys M Mac"]);
        assert_eq!(
            key(key(filtering, Key::Backspace), Key::Backspace)
                .view
                .filter
                .query,
            "k"
        );
    }

    #[test]
    fn enter_keeps_the_filter_and_hands_the_keys_back() {
        let kept = key(press(loaded(), "/keys"), Key::Enter);

        assert_eq!(kept.mode, Mode::Dashboard);
        assert_eq!(kept.view.filter.query, "keys");
        assert_eq!(names(&kept), ["MX Keys M Mac"]);
        assert!(!press(kept, "q").running, "q is a command again");
    }

    #[test]
    fn esc_clears_the_filter_while_typing_and_after_keeping_it() {
        let typing = press(loaded(), "/keys");
        let kept = key(typing.clone(), Key::Enter);

        for app in [typing, kept] {
            let cleared = key(app, Key::Escape);

            assert_eq!(cleared.view.filter, Filter::default());
            assert_eq!(names(&cleared).len(), 3);
        }
    }

    #[test]
    fn j_and_k_type_rather_than_move_while_the_filter_is_open() {
        let filtering = press(loaded(), "/k");

        assert_eq!(filtering.rows().len(), 2, "two devices are still on screen");

        let typed = press(filtering.clone(), "j");
        assert_eq!(typed.view.filter.query, "kj");
        assert_eq!(typed.selected, 0, "and the selection has not moved");

        let kept = key(filtering, Key::Enter);
        assert_eq!(
            press(kept, "j").selected,
            1,
            "j moves once the filter is kept"
        );
    }

    #[test]
    fn a_reading_arriving_leaves_the_view_alone() {
        let filtering = press(press(loaded(), "s"), "/key");
        let before = press(key(filtering, Key::Enter), "h");
        let after = update(before.clone(), Event::Reading(three_devices()));

        assert_eq!(
            after.view, before.view,
            "sort, filter and hidden all survive"
        );
        assert_eq!(after.mode, before.mode);
    }

    #[test]
    fn a_filter_cannot_talk_the_status_line_out_of_an_alert() {
        let low = device("Soundcore Liberty", "d0-03-4b-0b-e6-4e", Some(9));
        let app = update(
            app(),
            Event::Reading(reading(vec![
                device("Magic Trackpad", "30-82-16-f2-24-90", Some(85)),
                low.clone(),
                Device {
                    connected: false,
                    ..device("AirPods Pro", "74-15-f5-02-8e-38", Some(4))
                },
            ])),
        );

        assert_eq!(app.critical(), 1, "the disconnected 4% is history");
        assert_eq!(app.connected().count(), 2);

        let filtered = press(app, "/trackpad");
        assert_eq!(filtered.rows().len(), 1, "the low device is off screen");
        assert_eq!(filtered.critical(), 1, "and still counted");
        assert_eq!(filtered.connected().count(), 2);
    }

    #[test]
    fn the_alert_count_uses_the_thresholds_the_events_are_raised_by() {
        let jumpy = App {
            config: config("[[device]]\nmatch = \"keys\"\ncritical = 45\n"),
            ..loaded()
        };

        assert_eq!(
            jumpy.critical(),
            1,
            "42% is critical for a device configured to call it that"
        );
        assert_eq!(
            loaded().critical(),
            0,
            "and is nobody's problem under the built-in 10"
        );
    }

    #[test]
    fn a_device_that_advertises_its_own_threshold_is_counted_by_that() {
        let keys = device("MX Keys M Mac", "de-df-38-f0-46-9b", Some(42));
        let app = App {
            advertised: AdvertisedThresholds::from([(
                keys.address.clone(),
                Advertised {
                    low: Some(60),
                    critical: Some(45),
                },
            )]),
            ..update(app(), Event::Reading(reading(vec![keys])))
        };

        assert_eq!(
            app.critical(),
            1,
            "Apple's number, in the absence of a file"
        );
    }

    #[test]
    fn a_filter_that_matches_nothing_leaves_the_selection_on_no_row() {
        let empty = press(press(loaded(), "jj"), "/nothing here");

        assert!(empty.rows().is_empty());
        assert_eq!(empty.selected, 0);
        assert_eq!(empty.current(), None);
    }

    #[test]
    fn h_hides_the_selected_device_and_the_selection_stays_on_a_row() {
        let hidden = press(press(loaded(), "jj"), "h");

        assert_eq!(
            names(&hidden),
            ["MX Keys M Mac", "Magic Trackpad"],
            "the emptiest device leads the default order"
        );
        assert_eq!(hidden.selected, 1, "the last row is the last row");
        assert_eq!(hidden.view.hidden.len(), 1);
    }

    #[test]
    fn capital_h_brings_hidden_devices_back_so_one_can_be_unhidden() {
        let hidden = press(loaded(), "h");
        let showing = press(hidden.clone(), "H");

        assert_eq!(names(&hidden).len(), 2);
        assert!(showing.view.show_hidden);
        assert_eq!(names(&showing).len(), 3);

        let unhidden = press(showing, "h");
        assert!(unhidden.view.hidden.is_empty(), "the same key both ways");
        assert_eq!(names(&press(unhidden, "H")).len(), 3);
    }

    #[test]
    fn hiding_nothing_hides_nothing() {
        let empty = press(app(), "h");

        assert!(empty.view.hidden.is_empty());
        assert!(!empty.save_hidden, "and asks for no write either");
    }

    #[test]
    fn hiding_and_showing_both_ask_the_loop_to_write_the_config_file() {
        let hidden = press(loaded(), "h");
        let written = update(hidden.clone(), Event::Saved(Ok(())));

        assert!(hidden.save_hidden, "the file has yet to be told");
        assert_eq!(hidden.view.hidden, ["de-df-38-f0-46-9b"]);
        assert!(!written.save_hidden, "and once told, stops asking");
        assert_eq!(written.notice, None, "a hide that worked speaks for itself");
        assert!(
            press(press(hidden, "H"), "h").save_hidden,
            "showing a device again is a write too"
        );
    }

    #[test]
    fn a_hide_the_file_would_not_take_is_reported_and_stays_on_screen() {
        let refused = update(
            press(loaded(), "h"),
            Event::Saved(Err("config.toml: line 3".to_string())),
        );

        assert_eq!(refused.notice, Some(Notice::problem("config.toml: line 3")));
        assert_eq!(names(&refused).len(), 2, "the row is gone all the same");
    }

    #[test]
    fn the_dashboard_opens_without_the_devices_the_config_file_hides() {
        let opened = App::new(
            INTERVAL,
            READ_AT,
            Look::of(&Theme::default(), Glyphs::ASCII),
            config("[dashboard]\nhidden = [\"MX Keys\"]\n"),
        );
        let hiding = update(opened, Event::Reading(three_devices()));

        assert_eq!(names(&hiding), ["Magic Trackpad", "Soundcore Liberty"]);
    }

    #[test]
    fn reloading_takes_the_hidden_devices_the_file_now_holds() {
        let reloaded = update(
            press(loaded(), "h"),
            Event::Reloaded(Ok(config("[dashboard]\nhidden = [\"Soundcore\"]\n"))),
        );

        assert_eq!(
            names(&reloaded),
            ["MX Keys M Mac", "Magic Trackpad"],
            "the file is where hiding lives, so `r` is what settles it"
        );
    }

    #[test]
    fn enter_opens_the_detail_view_over_the_selected_device_and_closes_it() {
        let opened = key(press(loaded(), "j"), Key::Enter);

        assert_eq!(opened.mode, Mode::Detail);
        assert_eq!(
            opened.current().map(|device| device.name.as_str()),
            Some("Magic Trackpad"),
            "the row the selection was on"
        );
        assert_eq!(key(opened.clone(), Key::Enter).mode, Mode::Dashboard);
        assert_eq!(key(opened, Key::Escape).mode, Mode::Dashboard);
    }

    #[test]
    fn enter_and_esc_round_trip_from_every_mode_that_binds_them() {
        let dashboard = loaded();
        let filtered = key(press(loaded(), "/key"), Key::Enter);

        for app in [dashboard, filtered] {
            let opened = key(app.clone(), Key::Enter);

            assert_eq!(opened.mode, Mode::Detail);
            assert_eq!(key(opened.clone(), Key::Escape), app, "esc backs out whole");
            assert_eq!(key(opened, Key::Enter), app, "and so does enter");
        }
    }

    #[test]
    fn enter_on_an_empty_dashboard_opens_nothing() {
        let unread = app();
        let filtered_out = press(loaded(), "/nothing here");

        for app in [unread, key(filtered_out, Key::Enter)] {
            assert_eq!(app.current(), None);
            assert_eq!(
                key(app.clone(), Key::Enter),
                app,
                "there is nothing to show"
            );
        }
    }

    #[test]
    fn the_detail_view_swallows_every_key_it_does_not_advertise() {
        let open = key(loaded(), Key::Enter);

        assert_eq!(
            press(open.clone(), "jksh/H?r"),
            open,
            "the dashboard keys do nothing while one device is on screen"
        );
        assert_eq!(key(open.clone(), Key::Backspace), open);
        assert!(!press(open, "q").running, "and q still quits");
    }

    #[test]
    fn a_detail_view_whose_device_went_away_backs_out_to_the_table() {
        let open = key(press(loaded(), "jj"), Key::Enter);
        let alone = update(
            open.clone(),
            Event::Reading(reading(vec![device(
                "Magic Trackpad",
                "30-82-16-f2-24-90",
                Some(85),
            )])),
        );
        let emptied = update(open, Event::Reading(reading(Vec::new())));

        assert_eq!(alone.mode, Mode::Detail, "the selection took the last row");
        assert_eq!(
            alone.current().map(|device| device.name.as_str()),
            Some("Magic Trackpad")
        );
        assert_eq!(
            emptied.mode,
            Mode::Dashboard,
            "and a table with no rows leaves it nothing to draw"
        );
    }

    #[test]
    fn the_events_the_engine_raised_are_kept_for_the_device_they_belong_to() {
        let trackpad = device("Magic Trackpad", "30-82-16-f2-24-90", Some(9));
        let raised = Raised {
            event: blubat_core::Event::LowBattery,
            device: trackpad.name.clone(),
            address: trackpad.address.clone(),
            level: Some(9),
            previous: Some(21),
            charge: ChargeState::Discharging,
            source: Source::IoKit,
            threshold: Some(20),
            cycle: 0,
            at: READ_AT,
        };
        let app = update(loaded(), Event::Raised(vec![raised.clone()]));

        assert_eq!(
            app.journal.recent(&trackpad.address).collect::<Vec<_>>(),
            [&raised]
        );
        assert_eq!(
            app.journal
                .recent(&Address::parse("de-df-38-f0-46-9b").expect("valid address"))
                .count(),
            0,
            "and only for that device"
        );
    }

    #[test]
    fn r_asks_the_loop_for_a_reload_from_the_dashboard_alone() {
        let dashboard = press(loaded(), "r");
        let filtering = press(loaded(), "/r");
        let overlay = press(loaded(), "?");

        assert!(dashboard.reload, "the dashboard binds it");
        assert!(!filtering.reload, "the filter takes it as text");
        assert_eq!(filtering.view.filter.query, "r");
        assert_eq!(
            press(overlay.clone(), "r"),
            overlay,
            "and the overlay swallows what it does not advertise"
        );
    }

    #[test]
    fn a_reload_takes_every_value_the_new_file_carries() {
        let asked = press(loaded(), "r");

        let reloaded = update(
            asked,
            Event::Reloaded(Ok(config(
                "[defaults]\nlow = 30\n\n\
                 [theme]\nscheme = \"light\"\ncharging_glyph = \"^\"\n\n\
                 [[hook]]\nevent = \"charged\"\ncommand = \"unplug\"\n",
            ))),
        );

        assert!(!reloaded.reload, "the request is answered");
        assert_eq!(reloaded.config.defaults.low, Some(30), "thresholds");
        assert_eq!(reloaded.look.palette, Palette::LIGHT, "theme");
        assert_eq!(reloaded.look.glyphs.charging, "^", "glyphs");
        assert_eq!(reloaded.config.hooks.len(), 1, "hooks");
        assert_eq!(reloaded.notice, Some(Notice::said("config reloaded")));
    }

    #[test]
    fn a_reload_that_cannot_be_read_leaves_the_config_in_force_alone() {
        let configured = update(
            press(loaded(), "r"),
            Event::Reloaded(Ok(config(
                "[defaults]\nlow = 30\n\n[theme]\nscheme = \"light\"\n",
            ))),
        );

        let rejected = update(
            press(configured.clone(), "r"),
            Event::Reloaded(Err("config.toml: expected `=` at line 3".to_string())),
        );

        assert!(rejected.running, "a typo is not a reason to stop");
        assert_eq!(rejected.config, configured.config, "still 30");
        assert_eq!(rejected.look, configured.look, "and still the light scheme");
        assert!(!rejected.reload);
        assert_eq!(
            rejected.notice,
            Some(Notice::problem("config.toml: expected `=` at line 3"))
        );
    }

    #[test]
    fn a_notice_is_read_once_and_the_next_key_clears_it() {
        let said = update(loaded(), Event::Note(Notice::problem("a hook exited 1")));

        assert_eq!(
            said.notice.as_ref().map(|notice| notice.problem),
            Some(true)
        );
        assert_eq!(press(said.clone(), "j").notice, None);
        assert_eq!(
            update(said.clone(), Event::Tick(READ_AT)).notice,
            said.notice,
            "and a tick alone does not, since nobody has looked"
        );
    }

    #[test]
    fn readings_accumulate_into_the_trend_history() {
        let an_hour_ago = Timestamp::from_unix(READ_AT.unix() - 3_600);
        let earlier = Snapshot {
            read_at: an_hour_ago,
            devices: vec![Device {
                read_at: an_hour_ago,
                ..device("Magic Trackpad", "30-82-16-f2-24-90", Some(90))
            }],
            degraded: false,
            warnings: Vec::new(),
        };
        let app = update(app(), Event::Reading(earlier));
        let app = update(app, Event::Reading(three_devices()));

        let trackpad = &app.devices()[0].address;
        assert_eq!(app.history.samples(trackpad).count(), 2);
        assert!(
            app.history.trend(trackpad).expect("a trend").rate < 0.0,
            "90% an hour ago and 85% now is a drain"
        );
    }
}
