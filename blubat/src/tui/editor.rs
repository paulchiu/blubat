//! Where the config editor `c` opens runs.
//!
//! Reached through a trait, the same shape as the daemon's `Launchctl`: the
//! real side resolves `$EDITOR` (or `$VISUAL`) and spawns it, and a test hands
//! the plumbing around it a fake that does neither, so `c`'s reducer flag, the
//! reload it asks for on return and the notice a missing editor leaves are all
//! exercised without spawning anything.

use std::path::Path;

use crate::Failure;
use crate::config;

/// Somewhere the dashboard's `c` opens the config file, which a test fills
/// with a fake.
pub trait Editor {
    /// Resolves the editor and waits for it to close over `path`.
    fn edit(&self, path: &Path) -> Result<(), Failure>;
}

/// The real one: the same resolution and spawn `blubat config edit` uses.
#[derive(Clone, Copy, Debug, Default)]
pub struct Cli;

impl Editor for Cli {
    fn edit(&self, path: &Path) -> Result<(), Failure> {
        let editor = config::editor(|name| std::env::var(name).ok())?;

        config::edit(path, &editor)
    }
}
