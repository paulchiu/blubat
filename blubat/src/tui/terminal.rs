//! Owning the terminal for as long as the dashboard has it.

use std::io;

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
    pub fn suspended<T>(
        &mut self,
        admission: &Admission,
        during: impl FnOnce() -> T,
    ) -> io::Result<T> {
        admission.suspend();
        ratatui::try_restore()?;
        let result = during();
        self.terminal = ratatui::try_init()?;
        admission.resume();

        Ok(result)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        ratatui::restore();
    }
}
