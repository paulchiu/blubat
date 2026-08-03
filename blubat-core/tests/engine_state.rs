//! The event engine's state file, exercised through a real state directory.
//!
//! The unit tests cover the hysteresis table and the file format; this covers
//! the part that only shows up on a filesystem: a first run creating the state
//! directory under the resolved paths, and a restart reading back what the
//! previous run knew.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use blubat_core::{
    Address, AdvertisedThresholds, ChargeState, Config, Device, Engine, Event, Levels, Paths,
    Snapshot, Source, Timestamp,
};

static NEXT: AtomicU32 = AtomicU32::new(0);

/// A directory that removes itself, so a failing test leaves nothing behind.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "blubat-engine-state-tests-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = fs::remove_dir_all(&path);

        Self(path)
    }

    fn state_file(&self) -> PathBuf {
        Paths::rooted(&self.0).state_file()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn reading(level: u8, second: i64) -> Snapshot {
    let at = Timestamp::from_unix(1_785_600_000 + second);

    Snapshot {
        read_at: at,
        devices: vec![Device {
            address: Address::parse("30-82-16-f2-24-90").expect("valid address"),
            name: "Paul\u{2019}s Magic Trackpad".to_string(),
            kind: None,
            transport: None,
            vendor_id: None,
            product_id: None,
            levels: Levels {
                main: Some(level),
                ..Levels::default()
            },
            charge: ChargeState::Discharging,
            source: Source::IoKit,
            connected: true,
            read_at: at,
        }],
        degraded: false,
        warnings: Vec::new(),
    }
}

/// One run of a daemon: load, step through the levels, save.
fn run(state_file: &std::path::Path, levels: &[u8]) -> Vec<Event> {
    let (engine, warning) = Engine::load(state_file);
    assert_eq!(warning, None, "nothing blubat wrote should warn");

    let (engine, raised) = levels.iter().enumerate().fold(
        (engine, Vec::new()),
        |(engine, mut raised), (tick, level)| {
            let (engine, step) = engine.step(
                &reading(*level, tick as i64),
                &Config::default(),
                &AdvertisedThresholds::new(),
                Timestamp::from_unix(1_785_600_000 + tick as i64),
            );
            raised.extend(step);

            (engine, raised)
        },
    );

    engine.save(state_file).expect("writes the state file");

    raised.into_iter().map(|event| event.event).collect()
}

#[test]
fn a_first_run_creates_the_state_directory_it_writes_into() {
    let scratch = Scratch::new();

    run(&scratch.state_file(), &[50]);

    assert!(scratch.state_file().is_file());
    assert!(
        fs::read_to_string(scratch.state_file())
            .expect("readable")
            .contains("30-82-16-f2-24-90")
    );
}

#[test]
fn a_restart_does_not_re_fire_what_the_previous_run_already_raised() {
    let scratch = Scratch::new();

    let first = run(&scratch.state_file(), &[50, 19]);
    let second = run(&scratch.state_file(), &[19, 18]);
    let third = run(&scratch.state_file(), &[21, 19]);

    assert_eq!(first, [Event::LowBattery]);
    assert!(second.is_empty(), "still below, still quiet: {second:?}");
    assert_eq!(
        third,
        [Event::LowBattery],
        "a genuine recovery and crossing raises again"
    );
}

#[test]
fn a_state_file_written_by_hand_nonsense_is_started_over_from() {
    let scratch = Scratch::new();
    run(&scratch.state_file(), &[50, 19]);
    fs::write(scratch.state_file(), "not toml at all {{").expect("a corrupted state file");

    let (engine, warning) = Engine::load(&scratch.state_file());

    assert_eq!(engine, Engine::default());
    assert!(warning.is_some_and(|warning| warning.contains("fresh event state")));
}
