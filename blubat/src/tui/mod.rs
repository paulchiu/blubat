//! The blubat dashboard: a full screen view over the core poller.
//!
//! One loop over one channel. Events arrive from the keyboard and from the
//! poller, `update` folds each into the next state, and `render` draws that
//! state. The loop itself decides nothing: it waits, updates, draws. Because
//! neither `update` nor `render` does any I/O, the whole dashboard can be
//! exercised without a terminal, and because the loop only ever waits on the
//! event channel, a reading in flight can never delay a keystroke.

mod app;
mod columns;
mod events;
mod glyph;
mod render;
mod terminal;
mod theme;
mod view;

use std::io::{self, IsTerminal};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::Duration;

use blubat_core::{Tiers, Timestamp};

use crate::Failure;
use app::{App, Event, update};
use glyph::Glyphs;

/// How often the dashboard reads while someone is watching it.
///
/// Faster than the core default, which is set for a daemon polling in the
/// background: the fast tier is a single digit millisecond IOKit read, so a
/// dashboard on screen can afford one every few seconds.
const TIERS: Tiers = Tiers {
    fast: Duration::from_secs(5),
    slow: Duration::from_secs(300),
};

/// How long the loop waits for an event before drawing anyway.
///
/// The countdown to the next reading has to move with nobody pressing a key,
/// and this timeout is the only thing that wakes the loop when nothing happens.
const REDRAW: Duration = Duration::from_millis(250);

/// Opens the dashboard and holds the terminal until the user quits.
pub fn run() -> Result<(), Failure> {
    if !io::stdout().is_terminal() {
        return Err(Failure::Error(
            "the dashboard needs a terminal; run `blubat list` for a reading instead".to_string(),
        ));
    }

    let events = events::events(blubat_core::poll(TIERS));
    let mut session = terminal::Session::open()?;
    let mut app = App::new(TIERS.fast, Timestamp::now(), Glyphs::detected());

    while app.running {
        session.draw(&app)?;

        match next(&events) {
            Some(event) => app = update(app, event),
            None => break,
        }
    }

    Ok(())
}

/// The next event, or a redraw tick when the sources have nothing to say.
///
/// Absent once both sources are gone, since from then on nothing can change.
fn next(events: &Receiver<Event>) -> Option<Event> {
    match events.recv_timeout(REDRAW) {
        Ok(event) => Some(event),
        Err(RecvTimeoutError::Timeout) => Some(Event::Tick(Timestamp::now())),
        Err(RecvTimeoutError::Disconnected) => None,
    }
}

impl From<io::Error> for Failure {
    fn from(error: io::Error) -> Self {
        Failure::Error(error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_dashboard_polls_faster_than_a_background_daemon_does() {
        assert!(TIERS.fast < Tiers::default().fast);
        assert!(
            REDRAW < TIERS.fast,
            "the countdown has to move between readings"
        );
    }

    #[test]
    fn a_quiet_channel_becomes_a_redraw_rather_than_a_wait() {
        let (sender, events) = std::sync::mpsc::channel();

        assert!(matches!(next(&events), Some(Event::Tick(_))));
        drop(sender);
        assert_eq!(next(&events), None, "nothing can change once both are gone");
    }
}
