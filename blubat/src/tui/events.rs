//! One stream of events out of two sources: the keyboard and the poller.
//!
//! Each source forwards on its own thread into one channel, so the loop waits
//! on exactly one thing. That is what keeps input responsive while a reading is
//! in flight: a `system_profiler` call that takes 170ms holds up its own tier
//! and nothing else, and a keyboard read that blocks for minutes holds up no
//! reading.

use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use blubat_core::Snapshot;
use crossterm::event::{self, Event as Terminal, KeyCode, KeyEventKind};

use super::app::Event;

/// Merges keypresses and readings into the one channel the loop waits on.
///
/// Both threads end once the returned receiver is dropped, and dropping the
/// readings with them is what stops the poller.
pub fn events(readings: Receiver<Snapshot>) -> Receiver<Event> {
    let (sender, events) = mpsc::channel();
    let keys = sender.clone();

    thread::spawn(move || forward(readings.into_iter().map(Event::Reading), &sender));
    thread::spawn(move || forward(keypresses(), &keys));

    events
}

/// Sends everything `source` produces until the loop stops listening.
fn forward(source: impl Iterator<Item = Event>, sink: &Sender<Event>) {
    for event in source {
        if sink.send(event).is_err() {
            break;
        }
    }
}

/// Blocking reads of the terminal, as the keys the dashboard binds on.
///
/// Anything that is not a keypress, a resize among them, is dropped here: the
/// loop redraws on its own tick and the next draw picks up the new size, so
/// nothing needs waking for it.
fn keypresses() -> impl Iterator<Item = Event> {
    std::iter::from_fn(|| {
        loop {
            match event::read() {
                Ok(terminal) => {
                    if let Some(key) = pressed(&terminal) {
                        return Some(Event::Key(key));
                    }
                }
                // The terminal is gone, so no key will ever arrive again.
                Err(_) => return None,
            }
        }
    })
}

/// The character a keypress carries, absent for anything else.
///
/// Releases and repeats are ignored so a held key acts once per press on the
/// terminals that report them.
fn pressed(terminal: &Terminal) -> Option<char> {
    match terminal {
        Terminal::Key(key) if key.kind == KeyEventKind::Press => match key.code {
            KeyCode::Char(key) => Some(key),
            _ => None,
        },
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyEvent, KeyModifiers};

    use super::*;

    fn key(code: KeyCode, kind: KeyEventKind) -> Terminal {
        Terminal::Key(KeyEvent::new_with_kind(code, KeyModifiers::NONE, kind))
    }

    #[test]
    fn a_source_reaches_the_loop_in_order() {
        let (sender, events) = mpsc::channel();

        forward(['j', 'k', 'q'].into_iter().map(Event::Key), &sender);
        drop(sender);

        assert_eq!(
            events.into_iter().collect::<Vec<_>>(),
            [Event::Key('j'), Event::Key('k'), Event::Key('q')]
        );
    }

    #[test]
    fn a_source_stops_once_the_loop_does() {
        let (sender, events) = mpsc::channel();
        let mut produced = 0;
        drop(events);

        forward(
            std::iter::repeat_with(|| {
                produced += 1;
                Event::Key('j')
            }),
            &sender,
        );

        assert_eq!(produced, 1, "an endless source stops at the first failure");
    }

    #[test]
    fn only_a_pressed_character_is_a_key_the_dashboard_sees() {
        assert_eq!(
            pressed(&key(KeyCode::Char('q'), KeyEventKind::Press)),
            Some('q')
        );
        assert_eq!(
            pressed(&key(KeyCode::Char('q'), KeyEventKind::Release)),
            None
        );
        assert_eq!(pressed(&key(KeyCode::Enter, KeyEventKind::Press)), None);
        assert_eq!(pressed(&Terminal::Resize(80, 24)), None);
    }
}
