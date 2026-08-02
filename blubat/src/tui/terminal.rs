//! Owning the terminal for as long as the dashboard has it.

use std::io;

use ratatui::DefaultTerminal;
use ratatui::widgets::TableState;

use super::app::App;
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
}

impl Drop for Session {
    fn drop(&mut self) {
        ratatui::restore();
    }
}
