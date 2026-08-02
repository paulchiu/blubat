//! The blubat dashboard: a full screen view over the core poller.
//!
//! One loop over one channel. Events arrive from the keyboard and from the
//! poller, `update` folds each into the next state, and `render` draws that
//! state. The loop itself decides nothing: it waits, updates, draws, and hands
//! whatever a reading implies beyond redrawing to [`Effects`]. Because neither
//! `update` nor `render` does any I/O, the whole dashboard can be exercised
//! without a terminal, and because the loop only ever waits on the event
//! channel, a reading in flight can never delay a keystroke.

mod app;
mod columns;
mod detail;
mod events;
mod glyph;
mod journal;
mod render;
mod terminal;
mod theme;
mod view;

use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::Duration;

use blubat_core::{Config, Paths, Poll, Tiers, Timestamp};

use crate::effects::{Effects, Observed};
use crate::hooks::Outcome;
use crate::{Failure, lock};
use app::{App, Event, Notice, update};
use glyph::Glyphs;
use theme::Look;

/// How often the dashboard reads while nobody has asked for anything else.
///
/// Faster than the core's foreground default because a dashboard on screen is
/// being read as it changes: the fast tier is a single digit millisecond IOKit
/// read, so it can afford one every few seconds.
const DASHBOARD_INTERVAL: Duration = Duration::from_secs(5);

/// How long the loop waits for an event before drawing anyway.
///
/// The countdown to the next reading has to move with nobody pressing a key,
/// and this timeout is the only thing that wakes the loop when nothing happens.
const REDRAW: Duration = Duration::from_millis(250);

/// Opens the dashboard and holds the terminal until the user quits.
///
/// The caller decides there is a terminal to take: `blubat` piped into
/// something has a reading to offer instead, and that choice belongs where the
/// bare invocation is handled rather than here.
///
/// The lock is taken beside the terminal and released with it: for as long as
/// the dashboard is up it owns the notifications and the hooks, and a daemon
/// running behind it records what it sees and fires none of it.
pub fn run(paths: &Paths) -> Result<(), Failure> {
    let taken = lock::take(paths.tui_lock());
    let unlocked = taken.as_ref().err().cloned();
    let _dashboard = taken.ok();
    let (config, unreadable) = load(paths);
    let tiers = tiers(&config.poll);
    let (notes, events) = events::events(blubat_core::poll(tiers));
    let (mut effects, stale_state) = Effects::live(paths, reporter(notes));
    let mut session = terminal::Session::open()?;
    let mut app = App {
        notice: notice([unreadable, stale_state, unlocked]),
        advertised: effects.advertised().clone(),
        ..App::new(
            tiers.fast,
            Timestamp::now(),
            Look::of(&config.theme, Glyphs::detected()),
            config,
        )
    };

    while app.running {
        session.draw(&app)?;

        let Some(event) = next(&events) else { break };
        let observed = match &event {
            Event::Reading(reading) => effects.observe(reading, &app.config),
            _ => Observed::default(),
        };

        app = update(app, event);

        // After the reading they came from, so the detail view's log and the
        // chart under it are drawn from the same tick.
        if !observed.raised.is_empty() {
            app = update(app, Event::Raised(observed.raised));
        }
        if !observed.problems.is_empty() {
            app = update(
                app,
                Event::Note(Notice::problem(observed.problems.join("; "))),
            );
        }
        if app.reload {
            app = update(app, Event::Reloaded(effects.reload()));
        }
        if app.save_hidden {
            let written = effects.save_hidden(&app.view.hidden);

            app = update(app, Event::Saved(written));
        }
    }

    Ok(())
}

/// The config in force at startup, with whatever was wrong with the file.
///
/// A file blubat cannot read is reported rather than fatal. The dashboard is a
/// monitor: refusing to show a battery level over a typo in a threshold helps
/// nobody, and `r` is there to pick the file up once it is fixed.
fn load(paths: &Paths) -> (Config, Option<String>) {
    Config::load(paths.config_file()).map_or_else(
        |error| (Config::default(), Some(error.to_string())),
        |config| (config, None),
    )
}

/// The tiers the dashboard polls on.
///
/// Its own faster interval while the file has not asked for one, since a
/// dashboard on screen is read as it changes. A file naming an interval of its
/// own is taken at its word, which leaves a file repeating the built-in
/// defaults back behaving exactly as no file at all.
fn tiers(poll: &Poll) -> Tiers {
    let asked_for = poll.foreground_interval != Poll::default().foreground_interval;

    Tiers {
        fast: if asked_for {
            poll.foreground_interval
        } else {
            DASHBOARD_INTERVAL
        },
        slow: poll.profiler_interval,
        timeout: poll.profiler_timeout,
    }
}

/// The one line the dashboard opens with, out of everything that was wrong.
fn notice(problems: [Option<String>; 3]) -> Option<Notice> {
    let problems: Vec<String> = problems.into_iter().flatten().collect();

    (!problems.is_empty()).then(|| Notice::problem(problems.join("; ")))
}

/// Where a finished hook reports to, which is the status line rather than
/// stderr: a line printed under the dashboard lands on top of what it drew.
fn reporter(notes: Sender<Event>) -> impl Fn(Outcome) + Send + Sync + 'static {
    move |outcome| {
        if outcome.went_wrong() {
            let _ = notes.send(Event::Note(Notice::problem(outcome.to_string())));
        }
    }
}

/// The next event, or a redraw tick when the sources have nothing to say.
///
/// Absent once every source is gone, since from then on nothing can change.
fn next(events: &Receiver<Event>) -> Option<Event> {
    match events.recv_timeout(REDRAW) {
        Ok(event) => Some(event),
        Err(RecvTimeoutError::Timeout) => Some(Event::Tick(Timestamp::now())),
        Err(RecvTimeoutError::Disconnected) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn poll(written: &str) -> Poll {
        Config::parse(written).expect("the test config parses").poll
    }

    #[test]
    fn the_dashboard_polls_faster_than_the_core_default_it_overrides() {
        assert!(DASHBOARD_INTERVAL < Tiers::default().fast);
        assert!(
            REDRAW < DASHBOARD_INTERVAL,
            "the countdown has to move between readings"
        );
    }

    #[test]
    fn an_unconfigured_dashboard_keeps_its_own_faster_interval() {
        assert_eq!(tiers(&Poll::default()).fast, DASHBOARD_INTERVAL);
        assert_eq!(
            tiers(&poll("[poll]\nforeground_interval = \"30s\"\n")).fast,
            DASHBOARD_INTERVAL,
            "a file repeating the default back is a file that asked for nothing"
        );
    }

    #[test]
    fn an_interval_the_file_asks_for_is_the_one_it_gets() {
        let configured = tiers(&poll(
            "[poll]\nforeground_interval = \"90s\"\nprofiler_interval = \"10m\"\n",
        ));

        assert_eq!(configured.fast, Duration::from_secs(90));
        assert_eq!(
            configured.slow,
            Duration::from_secs(600),
            "the slow tier is the file's either way: the dashboard has no view \
             on how often system_profiler is worth calling"
        );
    }

    #[test]
    fn a_startup_problem_becomes_one_line_and_a_clean_start_becomes_none() {
        assert_eq!(notice([None, None, None]), None);
        assert_eq!(
            notice([Some("config.toml: line 3".to_string()), None, None]),
            Some(Notice::problem("config.toml: line 3"))
        );
        assert_eq!(
            notice([
                Some("bad config".to_string()),
                Some("bad state".to_string()),
                Some("no lock".to_string())
            ])
            .map(|notice| notice.text),
            Some("bad config; bad state; no lock".to_string())
        );
    }

    #[test]
    fn a_quiet_channel_becomes_a_redraw_rather_than_a_wait() {
        let (sender, events) = std::sync::mpsc::channel();

        assert!(matches!(next(&events), Some(Event::Tick(_))));
        drop(sender);
        assert_eq!(next(&events), None, "nothing can change once both are gone");
    }

    #[test]
    fn only_a_hook_that_went_wrong_is_worth_a_line() {
        let (notes, reported) = std::sync::mpsc::channel();
        let report = reporter(notes);
        let outcome = |ran| Outcome {
            command: String::from("~/bin/nag"),
            event: blubat_core::Event::LowBattery,
            device: String::from("Trackpad"),
            ran,
        };

        report(outcome(crate::hooks::Ran::Exited(0)));
        report(outcome(crate::hooks::Ran::TimedOut));

        assert_eq!(
            reported.try_iter().collect::<Vec<_>>(),
            [Event::Note(Notice::problem(
                "low_battery hook `~/bin/nag` for Trackpad: timed out and was killed"
            ))]
        );
    }
}
