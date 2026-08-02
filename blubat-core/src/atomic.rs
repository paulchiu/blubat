//! Writing a file so that nothing ever reads half of one.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

/// Writes `contents` to `path` through a partial file and a rename.
///
/// A rename within one filesystem is atomic, so a daemon draining the watch
/// directory and a restart reading the state file both see either the previous
/// file or the whole new one. The parent directory is created because nothing
/// under the state directory exists until blubat first writes there.
pub(crate) fn write(path: &Path, contents: &str) -> Result<()> {
    let mut partial = OsString::from(path);
    partial.push(".partial");
    let partial = PathBuf::from(partial);

    path.parent()
        .map_or(Ok(()), fs::create_dir_all)
        .and_then(|()| fs::write(&partial, contents))
        .and_then(|()| fs::rename(&partial, path))
        .map_err(|source| Error::Io {
            path: path.to_path_buf(),
            source,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory that removes itself, so a failing test leaves nothing behind.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!("blubat-atomic-{name}"));
            let _ = fs::remove_dir_all(&path);

            Self(path)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn writes_into_a_directory_that_does_not_exist_yet() {
        let scratch = Scratch::new("missing-directory");
        let path = scratch.0.join("deeper").join("state.toml");

        write(&path, "level = 42\n").expect("writes");

        assert_eq!(fs::read_to_string(&path).expect("readable"), "level = 42\n");
    }

    #[test]
    fn replaces_an_existing_file_and_leaves_no_partial_behind() {
        let scratch = Scratch::new("replacement");
        let path = scratch.0.join("state.toml");

        write(&path, "first").expect("writes");
        write(&path, "second").expect("replaces");

        assert_eq!(fs::read_to_string(&path).expect("readable"), "second");
        assert!(!path.with_extension("toml.partial").exists());
    }

    #[test]
    fn a_path_that_cannot_be_written_is_an_error_rather_than_a_panic() {
        let scratch = Scratch::new("unwritable");
        let path = scratch.0.join("state.toml");

        fs::create_dir_all(&path).expect("a directory where the file should be");

        assert!(matches!(
            write(&path, "level = 42\n"),
            Err(Error::Io { .. })
        ));
    }
}
