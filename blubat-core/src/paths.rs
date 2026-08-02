//! Where blubat's files live, resolved in one place.
//!
//! The file the user owns and the files blubat owns are kept apart: intent in
//! the XDG config home, machine state in the XDG state home. Every path in the
//! program comes from here, so a test hands one directory to [`Paths::rooted`]
//! and nothing it runs can reach a real home directory.

use std::path::{Path, PathBuf};

use etcetera::base_strategy::{BaseStrategy, Xdg};

use crate::error::{Error, Result};

/// The directory blubat's files sit in under each XDG base.
const APP: &str = "blubat";
const CONFIG_FILE: &str = "config.toml";
const STATE_FILE: &str = "state.toml";
const WATCHES: &str = "watches";
const TUI_LOCK: &str = "tui.lock";
const DAEMON_LOCK: &str = "daemon.lock";
const LOG_FILE: &str = "daemon.log";
const ERROR_LOG_FILE: &str = "daemon.error.log";

/// The config file blubat reads and the state directory it writes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Paths {
    config_file: PathBuf,
    state_dir: PathBuf,
}

impl Paths {
    /// The XDG locations: `~/.config/blubat` and `~/.local/state/blubat`.
    pub fn resolve() -> Result<Self> {
        Xdg::new()
            .map_err(|error| Error::Path(error.to_string()))
            .map(|base| {
                Self::based(
                    &base.config_dir(),
                    &base.state_dir().unwrap_or_else(|| base.data_dir()),
                )
            })
    }

    /// Both trees under one directory, which is what a test hands a scratch dir.
    pub fn rooted(root: &Path) -> Self {
        Self {
            config_file: root.join(CONFIG_FILE),
            state_dir: root.join("state"),
        }
    }

    /// blubat's own layout under two XDG bases, which is the half of
    /// [`Paths::resolve`] that does not depend on the environment.
    fn based(config: &Path, state: &Path) -> Self {
        Self {
            config_file: config.join(APP).join(CONFIG_FILE),
            state_dir: state.join(APP),
        }
    }

    /// Replaces the config file, which is what the global `--config` flag does.
    pub fn with_config_file(self, config_file: PathBuf) -> Self {
        Self {
            config_file,
            ..self
        }
    }

    /// Replaces the state directory, which is what `--state-dir` does.
    pub fn with_state_dir(self, state_dir: PathBuf) -> Self {
        Self { state_dir, ..self }
    }

    /// The directory blubat keeps its own files in.
    pub fn state_dir(&self) -> &Path {
        &self.state_dir
    }

    /// The TOML file holding user intent, which may not exist.
    pub fn config_file(&self) -> &Path {
        &self.config_file
    }

    /// The event engine's armed and fired state, which is machine state.
    pub fn state_file(&self) -> PathBuf {
        self.state_dir.join(STATE_FILE)
    }

    /// The one-shot watches `blubat wait` drops for a running daemon.
    pub fn watch_dir(&self) -> PathBuf {
        self.state_dir.join(WATCHES)
    }

    /// The lock a dashboard holds while it owns notifications and hooks.
    ///
    /// Both resident modes evaluate the same events, so exactly one of them may
    /// act on them. The file names the process holding it, which is what lets a
    /// daemon tell a dashboard that is still up from one that was killed.
    pub fn tui_lock(&self) -> PathBuf {
        self.state_dir.join(TUI_LOCK)
    }

    /// The lock a daemon holds, which is how `blubat wait` finds one to hand to.
    pub fn daemon_lock(&self) -> PathBuf {
        self.state_dir.join(DAEMON_LOCK)
    }

    /// Where the daemon's stdout goes under launchd.
    pub fn log_file(&self) -> PathBuf {
        self.state_dir.join(LOG_FILE)
    }

    /// Where the daemon's stderr goes under launchd, kept apart so a problem is
    /// not buried in a log of ordinary readings.
    pub fn error_log_file(&self) -> PathBuf {
        self.state_dir.join(ERROR_LOG_FILE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bases the XDG strategy resolves to, handed in so no test depends on
    /// the home directory of whoever is running it.
    fn xdg() -> Paths {
        Paths::based(
            Path::new("/home/blubat/.config"),
            Path::new("/home/blubat/.local/state"),
        )
    }

    #[test]
    fn each_file_sits_under_its_own_xdg_base() {
        let paths = xdg();

        assert_eq!(
            paths.config_file(),
            Path::new("/home/blubat/.config/blubat/config.toml")
        );
        assert_eq!(
            paths.state_file(),
            PathBuf::from("/home/blubat/.local/state/blubat/state.toml")
        );
        assert_eq!(
            paths.watch_dir(),
            PathBuf::from("/home/blubat/.local/state/blubat/watches")
        );
        assert_eq!(
            paths.tui_lock(),
            PathBuf::from("/home/blubat/.local/state/blubat/tui.lock")
        );
    }

    #[test]
    fn everything_blubat_writes_about_itself_is_machine_state() {
        let paths = xdg();
        let state = PathBuf::from("/home/blubat/.local/state/blubat");

        for path in [
            paths.state_file(),
            paths.watch_dir(),
            paths.tui_lock(),
            paths.daemon_lock(),
            paths.log_file(),
            paths.error_log_file(),
        ] {
            assert_eq!(path.parent(), Some(state.as_path()), "{path:?}");
        }
    }

    #[test]
    fn the_two_locks_and_the_two_logs_are_four_distinct_files() {
        let paths = xdg();
        let mut named = vec![
            paths.tui_lock(),
            paths.daemon_lock(),
            paths.log_file(),
            paths.error_log_file(),
        ];
        let written = named.len();
        named.sort();
        named.dedup();

        assert_eq!(named.len(), written);
    }

    #[test]
    fn intent_and_machine_state_never_share_a_directory() {
        for paths in [xdg(), Paths::rooted(Path::new("/tmp/blubat-test-root"))] {
            assert_ne!(
                paths.config_file().parent(),
                Some(paths.state_dir.as_path()),
                "{paths:?}"
            );
        }
    }

    #[test]
    fn a_rooted_set_keeps_every_path_inside_the_root() {
        let root = Path::new("/tmp/blubat-test-root");
        let paths = Paths::rooted(root);

        for path in [
            paths.config_file().to_path_buf(),
            paths.state_file(),
            paths.watch_dir(),
            paths.tui_lock(),
            paths.daemon_lock(),
            paths.log_file(),
            paths.error_log_file(),
        ] {
            assert!(path.starts_with(root), "{path:?} escaped the root");
        }
    }

    #[test]
    fn an_explicit_config_file_replaces_the_resolved_one_and_nothing_else() {
        let paths = Paths::rooted(Path::new("/tmp/blubat-test-root"));
        let state = paths.state_file();

        let overridden = paths.with_config_file(PathBuf::from("/elsewhere/mine.toml"));

        assert_eq!(overridden.config_file(), Path::new("/elsewhere/mine.toml"));
        assert_eq!(overridden.state_file(), state);
    }

    #[test]
    fn an_explicit_state_directory_moves_every_file_blubat_writes() {
        let paths = xdg().with_state_dir(PathBuf::from("/elsewhere/state"));

        assert_eq!(paths.state_dir(), Path::new("/elsewhere/state"));
        assert_eq!(
            paths.config_file(),
            Path::new("/home/blubat/.config/blubat/config.toml"),
            "which says nothing about the file the user owns"
        );
        for path in [
            paths.state_file(),
            paths.watch_dir(),
            paths.tui_lock(),
            paths.daemon_lock(),
            paths.log_file(),
            paths.error_log_file(),
        ] {
            assert!(path.starts_with("/elsewhere/state"), "{path:?}");
        }
    }
}
