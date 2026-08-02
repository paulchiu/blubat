//! The dashboard's impure half: the event engine, the state file, the banners
//! and the hooks.
//!
//! `update` folds an event into the next state and touches nothing else, so
//! everything a reading sets off beyond a redraw happens here, driven by the
//! loop between the two. Both sinks are traits and both paths are handed in, so
//! a test drives the whole chain from a reading to a started hook with no
//! notification centre and no shell.

use std::path::PathBuf;

use blubat_core::{AdvertisedThresholds, Config, Engine, Paths, Snapshot};

use crate::hooks::{self, Hooks, Outcome, Runner};
use crate::notify::{self, Desktop, Notifier};

/// Everything one reading sets off outside the reducer.
pub struct Effects {
    /// The file `r` re-reads.
    config_file: PathBuf,
    /// Where the arm flags and the debounce clocks are kept between runs.
    state_file: PathBuf,
    engine: Engine,
    /// The engine as the state file last held it, so a reading that moved
    /// nothing costs no write. Most of them move nothing.
    saved: Engine,
    /// What each device's own IOKit node publishes, read once: it describes the
    /// hardware rather than the config, so a reload says nothing about it, and
    /// a device paired mid session contributes its own thresholds next run.
    advertised: AdvertisedThresholds,
    notifier: Box<dyn Notifier>,
    hooks: Box<dyn Hooks>,
}

impl Effects {
    /// The live set: desktop banners and shell hooks, over the state blubat
    /// last wrote about itself.
    ///
    /// Hands back what was wrong with that state rather than failing on it. A
    /// fresh engine only means the next reading records the devices it finds
    /// instead of announcing them, which is a quiet first tick, not an error.
    pub fn live(
        paths: &Paths,
        report: impl Fn(Outcome) + Send + Sync + 'static,
    ) -> (Self, Option<String>) {
        let (engine, problem) = Engine::load(&paths.state_file());

        (
            Self::new(
                paths,
                engine,
                blubat_core::advertised(),
                Box::new(Desktop),
                Box::new(Runner::reporting(report)),
            ),
            problem,
        )
    }

    /// The same wiring over whichever engine, notifier and hook sink are given.
    pub fn new(
        paths: &Paths,
        engine: Engine,
        advertised: AdvertisedThresholds,
        notifier: Box<dyn Notifier>,
        hooks: Box<dyn Hooks>,
    ) -> Self {
        Self {
            config_file: paths.config_file().to_path_buf(),
            state_file: paths.state_file(),
            saved: engine.clone(),
            engine,
            advertised,
            notifier,
            hooks,
        }
    }

    /// What each device advertises, which the dashboard judges levels by too.
    pub fn advertised(&self) -> &AdvertisedThresholds {
        &self.advertised
    }

    /// Steps the engine over one reading, announces and dispatches what that
    /// raised, and persists what it moved.
    ///
    /// The reading's own moment is the clock, so what the engine makes of a
    /// reading does not depend on how long it took to arrive. Problems come
    /// back to be placed rather than printed: stderr would land on top of
    /// whatever the dashboard is drawing.
    pub fn observe(&mut self, reading: &Snapshot, config: &Config) -> Vec<String> {
        let now = reading.read_at;
        // `step` consumes the engine and hands the next one back, which is what
        // keeps it a pure function of the state it was given.
        let (engine, raised) =
            std::mem::take(&mut self.engine).step(reading, config, &self.advertised, now);
        self.engine = engine;

        let mut problems: Vec<String> = raised
            .iter()
            .filter_map(|raised| {
                notify::announce(raised, &config.notifications, self.notifier.as_ref())
                    .and_then(Result::err)
            })
            .collect();

        for raised in &raised {
            hooks::dispatch(
                raised,
                reading,
                config,
                &mut self.engine,
                self.hooks.as_ref(),
                now,
            );
        }

        // After dispatch, since allowing a hook to run is what records it.
        problems.extend(self.persist());

        problems
    }

    /// Reads the config file again, which is what `r` asks for.
    ///
    /// A file that cannot be read comes back as its message for the caller to
    /// keep the config already in force over: a dashboard that exited on a typo
    /// would be worse than one running on yesterday's thresholds.
    pub fn reload(&self) -> Result<Config, String> {
        Config::load(&self.config_file).map_err(|error| error.to_string())
    }

    /// Writes the state file, and only when the last reading moved anything.
    ///
    /// A dashboard tick that raised nothing has nothing new to say about
    /// itself, and at one reading every few seconds those are almost all of
    /// them. A write that failed leaves the last saved state where it was, so
    /// the next reading tries again rather than treating the file as current.
    fn persist(&mut self) -> Option<String> {
        if self.engine == self.saved {
            return None;
        }

        match self.engine.save(&self.state_file) {
            Ok(()) => {
                self.saved = self.engine.clone();

                None
            }
            Err(error) => Some(error.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    use blubat_core::{
        Address, ChargeState, Device, Event, Levels, Notifications, Source, Timestamp,
    };

    use crate::hooks::fake::Recorder as StartedHooks;
    use crate::notify::fake::Recorder as PostedBanners;

    use super::*;

    const TRACKPAD: &str = "30-82-16-f2-24-90";
    const READ_AT: i64 = 1_785_643_199;

    static NEXT: AtomicU32 = AtomicU32::new(0);

    /// A directory that removes itself, so a failing test leaves nothing behind
    /// and no test ever reaches a real config or state file.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "blubat-effects-tests-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::SeqCst)
            ));
            let _ = fs::remove_dir_all(&path);

            Self(path)
        }

        fn paths(&self) -> Paths {
            Paths::rooted(&self.0)
        }

        fn write_config(&self, contents: &str) {
            fs::create_dir_all(&self.0).expect("a scratch directory");
            fs::write(self.paths().config_file(), contents).expect("a written config");
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn device(level: Option<u8>, second: i64) -> Device {
        Device {
            address: Address::parse(TRACKPAD).expect("valid address"),
            name: "Paul\u{2019}s Magic Trackpad".to_string(),
            kind: None,
            transport: None,
            levels: Levels {
                main: level,
                ..Levels::default()
            },
            charge: ChargeState::Discharging,
            source: Source::IoKit,
            connected: true,
            read_at: Timestamp::from_unix(READ_AT + second),
        }
    }

    fn reading(level: Option<u8>, second: i64) -> Snapshot {
        Snapshot {
            read_at: Timestamp::from_unix(READ_AT + second),
            devices: vec![device(level, second)],
            warnings: Vec::new(),
        }
    }

    /// The same device with the link down, which is what raises `disconnected`.
    fn away(second: i64) -> Snapshot {
        Snapshot {
            devices: vec![Device {
                connected: false,
                ..device(Some(50), second)
            }],
            ..reading(Some(50), second)
        }
    }

    /// The effects under test, with both sinks recording instead of acting.
    fn effects(scratch: &Scratch) -> (Effects, Arc<PostedBanners>, Arc<StartedHooks>) {
        recording(scratch, PostedBanners::new())
    }

    /// The same, over whichever notifier the test wants to give it.
    fn recording(
        scratch: &Scratch,
        banners: PostedBanners,
    ) -> (Effects, Arc<PostedBanners>, Arc<StartedHooks>) {
        let banners = Arc::new(banners);
        let hooks = Arc::new(StartedHooks::new());
        let effects = Effects::new(
            &scratch.paths(),
            Engine::default(),
            AdvertisedThresholds::new(),
            Box::new(Arc::clone(&banners)),
            Box::new(Arc::clone(&hooks)),
        );

        (effects, banners, hooks)
    }

    /// Runs the levels a device reports over successive ticks through the whole
    /// chain, the first of which only records the device.
    fn observe(effects: &mut Effects, config: &Config, levels: &[u8]) -> Vec<String> {
        levels
            .iter()
            .enumerate()
            .flat_map(|(tick, level)| effects.observe(&reading(Some(*level), tick as i64), config))
            .collect()
    }

    #[test]
    fn a_crossing_reaches_the_banner_and_the_hook_it_was_configured_for() {
        let scratch = Scratch::new();
        let config = Config::parse(
            "[[hook]]\nevent = \"low_battery\"\ncommand = \"nag\"\n\n\
             [[hook]]\nevent = \"charged\"\ncommand = \"unplug\"\n",
        )
        .expect("parses");
        let (mut effects, banners, hooks) = effects(&scratch);

        let problems = observe(&mut effects, &config, &[50, 19]);

        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(banners.posted().len(), 1);
        assert_eq!(banners.posted()[0].body, "Battery low at 19%");
        assert_eq!(banners.posted()[0].sound.as_deref(), Some("Glass"));
        assert_eq!(hooks.commands(), ["nag"], "the charged hook waits its turn");
    }

    #[test]
    fn an_event_the_notification_toggles_silence_still_runs_its_hook() {
        let scratch = Scratch::new();
        let config = Config::parse(
            "[notifications]\nlow = false\n\n\
             [[hook]]\nevent = \"low_battery\"\ncommand = \"nag\"\n",
        )
        .expect("parses");
        let (mut effects, banners, hooks) = effects(&scratch);

        observe(&mut effects, &config, &[50, 19]);

        assert!(
            banners.posted().is_empty(),
            "the toggle is honoured: {:?}",
            banners.posted()
        );
        assert_eq!(hooks.commands(), ["nag"], "which says nothing about hooks");
    }

    #[test]
    fn the_sound_and_the_toggles_are_the_ones_the_config_carries() {
        let scratch = Scratch::new();
        let config =
            Config::parse("[notifications]\nconnect = true\nsound = \"Ping\"\n").expect("parses");
        let (mut effects, banners, _) = effects(&scratch);

        effects.observe(&reading(Some(50), 0), &config);
        // Twice, since a link change is announced only once it has held past
        // the coalesce window rather than on sight.
        for second in [60, 120] {
            effects.observe(&away(second), &config);
        }

        assert_eq!(banners.posted().len(), 1, "a link event, on by request");
        assert_eq!(banners.posted()[0].body, "Disconnected");
        assert_eq!(banners.posted()[0].sound.as_deref(), Some("Ping"));
        assert!(
            !Notifications::default().enabled(Event::Disconnected),
            "which the built-in default would not have posted"
        );
    }

    #[test]
    fn the_thresholds_a_device_is_judged_by_are_the_configured_ones() {
        let scratch = Scratch::new();
        let config = Config::parse("[[device]]\nmatch = \"trackpad\"\nlow = 40\n").expect("parses");
        let (mut effects, banners, _) = effects(&scratch);

        observe(&mut effects, &config, &[50, 39]);

        assert_eq!(banners.posted().len(), 1);
        assert_eq!(banners.posted()[0].body, "Battery low at 39%");
    }

    #[test]
    fn what_was_already_raised_survives_into_the_next_run() {
        let scratch = Scratch::new();
        let config = Config::default();
        let (mut effects, first, _) = effects(&scratch);
        observe(&mut effects, &config, &[50, 19]);

        let (engine, problem) = Engine::load(&scratch.paths().state_file());
        let banners = Arc::new(PostedBanners::new());
        let mut restarted = Effects::new(
            &scratch.paths(),
            engine,
            AdvertisedThresholds::new(),
            Box::new(Arc::clone(&banners)),
            Box::new(StartedHooks::new()),
        );

        restarted.observe(&reading(Some(19), 2), &config);

        assert_eq!(problem, None, "the state file read back cleanly");
        assert_eq!(first.posted().len(), 1, "the crossing was announced once");
        assert!(
            banners.posted().is_empty(),
            "and a restart at the same level says nothing: {:?}",
            banners.posted()
        );
    }

    #[test]
    fn a_banner_that_could_not_be_posted_comes_back_while_the_hook_still_runs() {
        let scratch = Scratch::new();
        let config = Config::parse("[[hook]]\nevent = \"low_battery\"\ncommand = \"nag\"\n")
            .expect("parses");
        let (mut effects, _, hooks) =
            recording(&scratch, PostedBanners::failing("no notification centre"));

        let problems = observe(&mut effects, &config, &[50, 19]);

        assert_eq!(problems, ["no notification centre"]);
        assert_eq!(
            hooks.commands(),
            ["nag"],
            "a banner nobody saw says nothing about the hooks"
        );
    }

    #[test]
    fn state_that_cannot_be_written_comes_back_rather_than_being_printed() {
        let scratch = Scratch::new();
        let (mut effects, _, _) = effects(&scratch);
        // A directory where the state file belongs, which is the shape of every
        // unwritable path a test can arrange without changing permissions.
        fs::create_dir_all(scratch.paths().state_file()).expect("a directory in the way");

        let problems = observe(&mut effects, &Config::default(), &[50, 19]);

        assert_eq!(
            problems.len(),
            2,
            "one per reading: a failed write is retried rather than assumed done"
        );
        assert!(
            problems
                .iter()
                .all(|problem| problem.contains("state.toml")),
            "each names the file: {problems:?}"
        );
    }

    #[test]
    fn a_reading_that_moved_nothing_leaves_the_state_file_alone() {
        let scratch = Scratch::new();
        let config = Config::default();
        let (mut effects, _, _) = effects(&scratch);
        observe(&mut effects, &config, &[50, 19]);

        let written = fs::metadata(scratch.paths().state_file()).expect("a written state file");
        effects.observe(&reading(Some(19), 2), &config);

        assert_eq!(
            fs::metadata(scratch.paths().state_file())
                .expect("still there")
                .modified()
                .ok(),
            written.modified().ok(),
            "the level was already fired, so there was nothing to write"
        );
    }

    #[test]
    fn a_hooks_debounce_clock_is_in_the_state_the_reading_wrote() {
        let scratch = Scratch::new();
        let config = Config::parse(
            "[[hook]]\nevent = \"low_battery\"\ncommand = \"nag\"\ndebounce = \"30m\"\n",
        )
        .expect("parses");
        let (mut effects, _, _) = effects(&scratch);

        observe(&mut effects, &config, &[50, 19]);

        let written =
            fs::read_to_string(scratch.paths().state_file()).expect("a written state file");
        assert!(
            written.contains("hook.nag"),
            "the state was saved after dispatch, so the clock is in it: {written}"
        );
    }

    #[test]
    fn a_reload_hands_back_the_file_or_the_reason_it_could_not() {
        let scratch = Scratch::new();
        let (effects, _, _) = effects(&scratch);

        assert_eq!(
            effects.reload(),
            Ok(Config::default()),
            "no file is the built-in defaults rather than a problem"
        );

        scratch.write_config("[defaults]\nlow = 30\n");
        assert_eq!(
            effects.reload().expect("parses").defaults.low,
            Some(30),
            "and the file once there is one"
        );

        scratch.write_config("[defaults]\nlow = 20\ncritical = \"ten\"\n");
        let problem = effects
            .reload()
            .expect_err("a string threshold is not a number");
        assert!(problem.contains("line 3"), "{problem}");
    }
}
