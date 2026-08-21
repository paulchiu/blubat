//! One-shot watches, the files `blubat wait` drops for a running daemon.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::atomic;
use crate::error::{Error, Result};
use crate::timestamp::Timestamp;

/// A request to notify once, when one device reaches one level.
///
/// `blubat wait` writes a watch into the watch directory and exits; a running daemon
/// drains that directory on each poll. A file drop rather than a socket is what
/// lets the handover exist without blubat growing an IPC surface, and it means
/// an interrupted wait leaves behind only a file that will be consumed or expire.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Watch {
    /// Substring matched against a device name and address, as `--device` is.
    pub device: String,
    /// The level in percent that completes the watch.
    pub target: u8,
    pub created: Timestamp,
    /// When set, the watch is discarded unmet at this time.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deadline: Option<Timestamp>,
}

impl Watch {
    /// Creates a watch stamped now, giving up `timeout` after that moment.
    pub fn new(device: impl Into<String>, target: u8, timeout: Option<Duration>) -> Self {
        let created = Timestamp::now();

        Self {
            device: device.into(),
            target,
            created,
            deadline: timeout.map(|timeout| created.plus(timeout)),
        }
    }

    /// Parses one watch file, rejecting unknown keys and bad timestamps.
    fn parse(contents: &str) -> Result<Self> {
        toml::from_str(contents)
            .map_err(|error| Error::Format(format!("watch file is unreadable: {error}")))
    }

    /// Reads one watch file.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] if the file cannot be read, and [`Error::Format`]
    /// if it exists but is not a valid watch: bad TOML, an unknown key, or a
    /// timestamp that does not parse.
    pub fn read(path: &Path) -> Result<Self> {
        Self::parse(&fs::read_to_string(path).map_err(|source| Error::Io {
            path: path.to_path_buf(),
            source,
        })?)
    }

    /// Writes the watch into `directory`, creating it if needed.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Format`] if the watch cannot be serialised to TOML, or
    /// [`Error::Io`] if the file cannot be written.
    pub fn write(&self, directory: &Path) -> Result<PathBuf> {
        let path = directory.join(self.file_name());

        toml::to_string(self)
            .map_err(|error| Error::Format(format!("watch file is unwritable: {error}")))
            .and_then(|contents| atomic::write(&path, &contents))
            .map(|()| path)
    }

    /// Names the file after what it is waiting for, so the directory reads.
    ///
    /// The target is in the name because two waits on one device registered in
    /// the same second would otherwise silently overwrite each other.
    fn file_name(&self) -> String {
        format!(
            "{}-{}-{}.toml",
            self.created.unix(),
            slug(&self.device),
            self.target
        )
    }
}

fn slug(device: &str) -> String {
    let squashed: String = device
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();

    match squashed.trim_matches('-') {
        "" => "device".to_string(),
        trimmed => trimmed.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn watch() -> Watch {
        Watch {
            device: "trackpad".to_string(),
            target: 100,
            created: Timestamp::from_unix(1_785_643_199),
            deadline: None,
        }
    }

    #[test]
    fn writes_the_documented_toml_shape() {
        let contents = toml::to_string(&watch()).expect("serialisable");

        assert_eq!(
            contents,
            "device = \"trackpad\"\ntarget = 100\ncreated = \"2026-08-02T03:59:59Z\"\n"
        );
    }

    #[test]
    fn round_trips_through_toml_with_and_without_a_deadline() {
        for original in [
            watch(),
            Watch {
                deadline: Some(Timestamp::from_unix(1_785_646_799)),
                ..watch()
            },
        ] {
            let contents = toml::to_string(&original).expect("serialisable");

            assert_eq!(Watch::parse(&contents).expect("parses"), original);
        }
    }

    #[test]
    fn parses_a_hand_written_file() {
        let parsed = Watch::parse(
            r#"
            device = "Magic Trackpad"
            target = 80
            created = "2026-08-02T03:59:59Z"
            deadline = "2026-08-02T04:59:59Z"
            "#,
        )
        .expect("parses");

        assert_eq!(parsed.device, "Magic Trackpad");
        assert_eq!(parsed.target, 80);
        assert_eq!(parsed.created, Timestamp::from_unix(1_785_643_199));
        assert_eq!(parsed.deadline, Some(Timestamp::from_unix(1_785_646_799)));
    }

    #[test]
    fn rejects_files_it_cannot_act_on() {
        for contents in [
            "",
            "device = \"trackpad\"",
            "device = \"trackpad\"\ntarget = 100",
            "device = \"trackpad\"\ntarget = 999\ncreated = \"2026-08-02T03:59:59Z\"",
            "device = \"trackpad\"\ntarget = 100\ncreated = \"whenever\"",
            "device = \"trackpad\"\ntarget = 100\ncreated = \"2026-08-02T03:59:59Z\"\nnotify = true",
            "not toml at all {{",
        ] {
            assert!(
                matches!(Watch::parse(contents), Err(Error::Format(_))),
                "{contents:?} should be rejected"
            );
        }
    }

    #[test]
    fn the_file_name_says_what_the_watch_is_for() {
        assert_eq!(watch().file_name(), "1785643199-trackpad-100.toml");
        assert_eq!(
            Watch {
                device: "Paul\u{2019}s AirPods Pro".to_string(),
                ..watch()
            }
            .file_name(),
            "1785643199-paul-s-airpods-pro-100.toml"
        );
        assert_eq!(
            Watch {
                device: "  ".to_string(),
                ..watch()
            }
            .file_name(),
            "1785643199-device-100.toml"
        );
    }

    #[test]
    fn two_targets_for_one_device_in_one_second_get_their_own_files() {
        let eighty = Watch {
            target: 80,
            ..watch()
        };

        assert_ne!(watch().file_name(), eighty.file_name());
    }

    #[test]
    fn a_timeout_becomes_a_deadline_that_far_ahead() {
        let before = Timestamp::now().unix();

        let deadline = Watch::new("trackpad", 100, Some(Duration::from_secs(600)))
            .deadline
            .expect("a timeout sets one");

        assert!(deadline.unix() >= before + 600, "{deadline:?}");
        assert_eq!(Watch::new("trackpad", 100, None).deadline, None);
    }
}
