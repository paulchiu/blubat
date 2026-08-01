//! Owning the terminal for as long as the dashboard has it.

use std::io;

use ratatui::DefaultTerminal;

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
}

impl Session {
    /// Takes the screen, entering raw mode and the alternate buffer.
    pub fn open() -> io::Result<Self> {
        ratatui::try_init().map(|terminal| Self { terminal })
    }

    /// Draws one frame of `app`.
    pub fn draw(&mut self, app: &App) -> io::Result<()> {
        self.terminal.draw(|frame| render(frame, app)).map(|_| ())
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        ratatui::restore();
    }
}
