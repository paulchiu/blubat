use std::fmt;
use std::path::PathBuf;

/// Everything blubat-core can fail at.
#[derive(Debug)]
pub enum Error {
    /// A macOS helper command could not be run, or ran and reported failure.
    Command(String),
    /// Input did not match the shape blubat expects.
    Parse(String),
    /// A file under the blubat state directory could not be read or written.
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    /// A blubat directory could not be resolved.
    Path(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Command(message) | Error::Parse(message) | Error::Path(message) => {
                f.write_str(message)
            }
            Error::Io { path, source } => write!(f, "{}: {source}", path.display()),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}
