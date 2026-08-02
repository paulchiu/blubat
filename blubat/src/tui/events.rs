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
use crossterm::event::{self, Event as Terminal, KeyCode, KeyEventKind, KeyModifiers};

use super::app::{Event, Key};

/// Merges keypresses and readings into the one channel the loop waits on, and
/// hands back the end anything else can join them on.
///
/// A hook finishing is the third source, and it arrives on a thread of its own
/// whenever it finishes, so it needs the same door rather than a wait of its
/// own. The readings thread ends once the returned receiver is dropped, which
/// is what stops the poller. The keypress thread is parked inside a blocking
/// terminal read and cannot notice, so it is detached: the process exit
/// reclaims it, and anything that outlives the dashboard would have to hand it
/// a way to be woken.
pub fn events(readings: Receiver<Snapshot>) -> (Sender<Event>, Receiver<Event>) {
    let (sender, events) = mpsc::channel();
    let keys = sender.clone();
    let others = sender.clone();

    thread::spawn(move || forward(readings.into_iter().map(Event::Reading), &sender));
    thread::spawn(move || forward(keypresses(), &keys));

    (others, events)
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
                    if let Some(event) = pressed(&terminal) {
                        return Some(event);
                    }
                }
                // The terminal is gone, so no key will ever arrive again.
                Err(_) => return None,
            }
        }
    })
}

/// The event a keypress carries, absent for anything the dashboard cannot bind.
///
/// Ctrl+C is an interrupt rather than a key: raw mode stops the terminal
/// turning it into a signal, so blubat owes the habit an answer of its own, and
/// as a key it would type a `c` into the filter. Releases and repeats are
/// ignored so a held key acts once per press on the terminals that report them.
fn pressed(terminal: &Terminal) -> Option<Event> {
    match terminal {
        Terminal::Key(key) if key.kind == KeyEventKind::Press => {
            let control = key.modifiers.contains(KeyModifiers::CONTROL);

            match key.code {
                KeyCode::Char('c') if control => Some(Event::Interrupt),
                KeyCode::Char(_) if control => None,
                KeyCode::Char(key) => Some(Event::Key(Key::Char(key))),
                KeyCode::Enter => Some(Event::Key(Key::Enter)),
                KeyCode::Esc => Some(Event::Key(Key::Escape)),
                KeyCode::Backspace => Some(Event::Key(Key::Backspace)),
                _ => None,
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::KeyEvent;

    use super::*;

    fn key(code: KeyCode, kind: KeyEventKind) -> Terminal {
        Terminal::Key(KeyEvent::new_with_kind(code, KeyModifiers::NONE, kind))
    }

    fn control(code: KeyCode) -> Terminal {
        Terminal::Key(KeyEvent::new_with_kind(
            code,
            KeyModifiers::CONTROL,
            KeyEventKind::Press,
        ))
    }

    #[test]
    fn a_source_reaches_the_loop_in_order() {
        let (sender, events) = mpsc::channel();

        forward(
            ['j', 'k', 'q']
                .into_iter()
                .map(|key| Event::Key(Key::Char(key))),
            &sender,
        );
        drop(sender);

        assert_eq!(
            events.into_iter().collect::<Vec<_>>(),
            [
                Event::Key(Key::Char('j')),
                Event::Key(Key::Char('k')),
                Event::Key(Key::Char('q'))
            ]
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
                Event::Key(Key::Char('j'))
            }),
            &sender,
        );

        assert_eq!(produced, 1, "an endless source stops at the first failure");
    }

    #[test]
    fn only_a_pressed_key_the_dashboard_binds_reaches_it() {
        for (code, expected) in [
            (KeyCode::Char('q'), Key::Char('q')),
            (KeyCode::Enter, Key::Enter),
            (KeyCode::Esc, Key::Escape),
            (KeyCode::Backspace, Key::Backspace),
        ] {
            assert_eq!(
                pressed(&key(code, KeyEventKind::Press)),
                Some(Event::Key(expected))
            );
        }

        assert_eq!(
            pressed(&key(KeyCode::Char('q'), KeyEventKind::Release)),
            None
        );
        assert_eq!(pressed(&key(KeyCode::Tab, KeyEventKind::Press)), None);
        assert_eq!(pressed(&Terminal::Resize(80, 24)), None);
    }

    #[test]
    fn ctrl_c_interrupts_and_no_other_chord_types_a_character() {
        assert_eq!(
            pressed(&control(KeyCode::Char('c'))),
            Some(Event::Interrupt)
        );
        assert_eq!(
            pressed(&control(KeyCode::Char('u'))),
            None,
            "a chord blubat does not bind is not the letter in it"
        );
        assert_eq!(
            pressed(&key(KeyCode::Char('c'), KeyEventKind::Press)),
            Some(Event::Key(Key::Char('c'))),
            "and c on its own is still a c"
        );
    }
}
