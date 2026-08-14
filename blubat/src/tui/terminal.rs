//! Owning the terminal for as long as the dashboard has it.

use std::io;
use std::time::Duration;

use ratatui::DefaultTerminal;
use ratatui::widgets::TableState;

use super::app::App;
use super::events::Admission;
use super::render::render;

/// The terminal in raw mode on the alternate screen, restored when dropped.
///
/// Every way out of the dashboard restores the screen. A normal return, an
/// error and a `?` all drop this guard, and a panic runs the hook `try_init`
/// installs, which restores before printing. Nothing leaves a shell with no
/// echo and no cursor.
pub struct Session {
    terminal: DefaultTerminal,
    /// The table's scroll offset, which has to outlive the frame that set it
    /// for the view to follow the selection rather than jump to it.
    table: TableState,
}

impl Session {
    /// Takes the screen, entering raw mode and the alternate buffer.
    pub fn open() -> io::Result<Self> {
        ratatui::try_init().map(|terminal| Self {
            terminal,
            table: TableState::new(),
        })
    }

    /// Draws one frame of `app`.
    pub fn draw(&mut self, app: &App) -> io::Result<()> {
        let table = &mut self.table;

        self.terminal
            .draw(|frame| render(frame, app, table))
            .map(|_| ())
    }

    /// Leaves raw mode and the alternate screen for as long as `during` runs,
    /// then takes them back, the same way `open` first did.
    ///
    /// What runs in between owns the real terminal, which is what the editor
    /// `c` opens needs: a child process inherits this process's stdio, and
    /// that is the dashboard's screen until `during` hands it back. blubat's
    /// own keypress reader is gated shut around it too: without that, the
    /// reader and the editor would both be reading the one terminal at once,
    /// and whichever the kernel wakes would get the keystroke.
    ///
    /// The editor can leave bytes behind it that never made it to `during`
    /// as a keystroke: its own exit chatter, and late replies to queries it
    /// made of the terminal, a background-colour query's OSC reply among
    /// them, that land after it has already exited. Re-init happens before
    /// [`Admission::resume`] reopens the reader's gate specifically so
    /// [`drain_typeahead`] can clear that buffer while the reader is still
    /// parked: nothing else touches the terminal until `resume` returns.
    pub fn suspended<T>(
        &mut self,
        admission: &Admission,
        during: impl FnOnce() -> T,
    ) -> io::Result<T> {
        admission.suspend();
        ratatui::try_restore()?;
        let result = during();
        self.terminal = ratatui::try_init()?;
        drain_typeahead();
        admission.resume();

        Ok(result)
    }
}

/// How long [`drain_typeahead`] waits for the next byte before deciding the
/// buffer is empty. Short enough not to be felt as a pause, long enough to
/// still be there for a query reply that has not landed yet: an in-flight
/// one arrives a poll after the last stale byte, not zero polls after, so a
/// zero-duration poll would stop draining before it did.
const DRAIN_GRACE: Duration = Duration::from_millis(20);

/// Discards whatever is in the terminal's input buffer, plus anything that
/// trickles in within a short grace window: the editor's exit chatter and
/// late replies to queries it made, which would otherwise be read as
/// keystrokes once the gate reopens.
fn drain_typeahead() {
    while matches!(crossterm::event::poll(DRAIN_GRACE), Ok(true)) {
        if crossterm::event::read().is_err() {
            break;
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        ratatui::restore();
    }
}
