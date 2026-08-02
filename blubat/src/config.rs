//! `blubat config`: where the file is, opening it, and checking it.
//!
//! blubat never writes the config file. `edit` hands it to the editor and
//! waits, and whether a file exists afterwards is the editor's decision, not
//! blubat's, so a machine that has never been configured stays that way.

use std::fs;
use std::io::{self, Write};
use std::path::Path;
use std::process::Command as Process;

use blubat_core::{Config, Device, Paths};

use crate::{Failure, reading};

/// What `blubat config` was asked to do.
#[derive(Debug, clap::Subcommand)]
pub enum Command {
    /// Print the resolved config file path.
    Path,
    /// Open the config file in $EDITOR.
    Edit,
    /// Parse the config file and report what is wrong with it.
    Validate,
}

/// Runs one `blubat config` subcommand.
pub fn run(command: &Command, paths: &Paths) -> Result<(), Failure> {
    let path = paths.config_file();

    match command {
        Command::Path => {
            println!("{}", path.display());

            Ok(())
        }
        Command::Edit => edit(path, &editor(|name| std::env::var(name).ok())?),
        Command::Validate => validate(path, &mut io::stdout(), || reading().devices),
    }
}

/// The editor to open the file in, `$EDITOR` or failing that `$VISUAL`.
///
/// Takes the lookup rather than reading the environment, since a test that set
/// a variable would be setting it for every other test running beside it.
fn editor(variable: impl Fn(&str) -> Option<String>) -> Result<String, Failure> {
    ["EDITOR", "VISUAL"]
        .into_iter()
        .filter_map(variable)
        .find(|editor| !editor.trim().is_empty())
        .ok_or_else(|| {
            Failure::Error("set $EDITOR to the editor blubat should open the config in".to_string())
        })
}

/// Opens the config file in the editor and waits for it to close.
///
/// The parent directory is created so the editor has somewhere to save to. The
/// file itself is not: an editor closed without saving leaves no config behind.
fn edit(path: &Path, editor: &str) -> Result<(), Failure> {
    let (program, arguments) = split(editor);

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| Failure::Error(format!("{}: {error}", parent.display())))?;
    }

    Process::new(program)
        .args(arguments)
        .arg(path)
        .status()
        .map_err(|error| Failure::Error(format!("{editor}: {error}")))
        .and_then(|status| {
            status
                .success()
                .then_some(())
                .ok_or_else(|| Failure::Error(format!("{editor} exited with {status}")))
        })
}

/// Splits an editor setting into the program and the arguments before the path.
///
/// Whitespace separated, so `code -w` works; an editor whose own path contains
/// a space needs a wrapper script, which is the usual shell convention.
fn split(editor: &str) -> (&str, Vec<&str>) {
    let mut words = editor.split_whitespace();

    (words.next().unwrap_or(editor), words.collect())
}

/// Parses the config file and reports everything wrong with it.
///
/// No file is a pass, since blubat runs on built-in defaults. A `[[device]]`
/// block matching nothing is a warning rather than a failure: the device is as
/// likely to be switched off as the match is to be a typo. Takes its reading so
/// the check runs without Bluetooth hardware, and only takes one at all when
/// there is a block whose match could be wrong.
fn validate(
    path: &Path,
    out: &mut impl Write,
    devices: impl Fn() -> Vec<Device>,
) -> Result<(), Failure> {
    let Some(config) = Config::read(path)? else {
        writeln!(
            out,
            "{}: no config file, blubat runs on built-in defaults",
            path.display()
        )?;

        return Ok(());
    };

    if !config.devices.is_empty() {
        for pattern in config.unmatched(&devices()) {
            writeln!(
                out,
                "warning: [[device]] match = \"{pattern}\" matches no device blubat can see"
            )?;
        }
    }

    let problems: String = config
        .problems()
        .iter()
        .map(|problem| format!("\n  {problem}"))
        .collect();

    if problems.is_empty() {
        writeln!(out, "{}: ok", path.display())?;

        Ok(())
    } else {
        Err(Failure::Error(format!("{}{problems}", path.display())))
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    use blubat_core::{Address, ChargeState, Levels, Source, Timestamp};

    use super::*;

    static NEXT: AtomicU32 = AtomicU32::new(0);

    /// A directory that removes itself, so a failing test leaves nothing behind.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "blubat-config-tests-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::SeqCst)
            ));
            let _ = fs::remove_dir_all(&path);

            Self(path)
        }

        /// The config file this test would resolve to, written or not.
        fn config_file(&self) -> PathBuf {
            Paths::rooted(&self.0).config_file().to_path_buf()
        }

        fn write(&self, contents: &str) -> PathBuf {
            let path = self.config_file();

            fs::create_dir_all(&self.0).expect("a scratch directory");
            fs::write(&path, contents).expect("a written config");

            path
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn trackpad() -> Device {
        Device {
            address: Address::parse("30-82-16-f2-24-90").expect("valid address"),
            name: "Paul\u{2019}s Magic Trackpad".to_string(),
            kind: None,
            transport: None,
            levels: Levels {
                main: Some(42),
                ..Levels::default()
            },
            charge: ChargeState::Unknown,
            source: Source::IoKit,
            connected: true,
            read_at: Timestamp::from_unix(0),
        }
    }

    /// Runs `validate` against a reading that never touches a real device.
    fn check(path: &Path, devices: Vec<Device>) -> (Result<(), Failure>, String) {
        let mut printed = Vec::new();
        let outcome = validate(path, &mut printed, || devices.clone());

        (outcome, String::from_utf8(printed).expect("utf8 output"))
    }

    #[test]
    fn no_config_file_is_a_pass_that_says_so() {
        let scratch = Scratch::new();

        let (outcome, printed) = check(&scratch.config_file(), Vec::new());

        assert_eq!(outcome, Ok(()));
        assert!(printed.contains("no config file"), "{printed}");
        assert!(!scratch.config_file().exists(), "checking created nothing");
    }

    #[test]
    fn a_usable_file_passes() {
        let scratch = Scratch::new();
        let path = scratch.write("[defaults]\nlow = 25\n\n[notifications]\nsound = \"Ping\"\n");

        let (outcome, printed) = check(&path, Vec::new());

        assert_eq!(outcome, Ok(()));
        assert!(printed.contains("ok"), "{printed}");
    }

    #[test]
    fn a_malformed_file_fails_with_the_line_it_is_on() {
        let scratch = Scratch::new();
        let path = scratch.write("[defaults]\nlow = 20\ncritical = \"ten\"\n");

        let (outcome, printed) = check(&path, Vec::new());

        let message = outcome
            .expect_err("a string threshold is not a number")
            .to_string();
        assert!(message.contains("line 3"), "{message}");
        assert!(message.contains(&path.display().to_string()), "{message}");
        assert!(printed.is_empty(), "{printed}");
    }

    #[test]
    fn an_unknown_key_fails_rather_than_being_ignored() {
        let scratch = Scratch::new();
        let path = scratch.write("[defaults]\nlwo = 20\n");

        assert!(check(&path, Vec::new()).0.is_err());
    }

    #[test]
    fn thresholds_that_cannot_hold_fail_with_every_problem_listed() {
        let scratch = Scratch::new();
        let path = scratch.write("[defaults]\nlow = 20\nhigh = 15\n");

        let message = check(&path, Vec::new())
            .0
            .expect_err("low is above high")
            .to_string();

        assert!(
            message.contains("low (20) must be below high (15)"),
            "{message}"
        );
    }

    #[test]
    fn a_block_matching_nothing_warns_without_failing() {
        let scratch = Scratch::new();
        let path = scratch.write("[[device]]\nmatch = \"trackpad\"\nlow = 25\n");

        let (matched, quiet) = check(&path, vec![trackpad()]);
        let (unmatched, warned) = check(&path, Vec::new());

        assert_eq!(matched, Ok(()));
        assert!(!quiet.contains("warning"), "{quiet}");
        assert_eq!(unmatched, Ok(()), "a device may simply be switched off");
        assert!(warned.contains("warning"), "{warned}");
        assert!(warned.contains("trackpad"), "{warned}");
    }

    #[test]
    fn the_editor_is_the_first_of_the_two_variables_that_names_one() {
        let set = |variables: [(&'static str, &'static str); 2]| {
            move |name: &str| {
                variables
                    .iter()
                    .find(|(key, _)| *key == name)
                    .map(|(_, value)| (*value).to_string())
                    .filter(|value| !value.is_empty())
            }
        };

        assert_eq!(
            editor(set([("EDITOR", "vim"), ("VISUAL", "")])),
            Ok("vim".to_string())
        );
        assert_eq!(
            editor(set([("EDITOR", ""), ("VISUAL", "code -w")])),
            Ok("code -w".to_string())
        );
        assert_eq!(
            editor(set([("EDITOR", "vim"), ("VISUAL", "code -w")])),
            Ok("vim".to_string()),
            "EDITOR is asked first"
        );
        assert_eq!(
            editor(set([("EDITOR", "   "), ("VISUAL", "code -w")])),
            Ok("code -w".to_string()),
            "a variable set to nothing but spaces names no editor"
        );
        assert!(
            editor(|_| None)
                .expect_err("nothing to open it in")
                .to_string()
                .contains("set $EDITOR")
        );
    }

    #[test]
    fn an_editor_setting_splits_into_a_program_and_its_arguments() {
        assert_eq!(split("vim"), ("vim", Vec::new()));
        assert_eq!(split("code -w"), ("code", vec!["-w"]));
        assert_eq!(split("  emacsclient  -nw  "), ("emacsclient", vec!["-nw"]));
    }

    #[test]
    fn editing_makes_room_for_the_file_without_creating_it() {
        let scratch = Scratch::new();
        let path = scratch.config_file();

        assert_eq!(edit(&path, "/usr/bin/true"), Ok(()));
        assert!(path.parent().expect("a parent").is_dir(), "nowhere to save");
        assert!(
            !path.exists(),
            "an editor that saved nothing leaves nothing"
        );
    }

    #[test]
    fn an_editor_that_fails_is_reported() {
        let scratch = Scratch::new();

        let failed = edit(&scratch.config_file(), "/usr/bin/false")
            .expect_err("a non zero exit is a failure");
        let missing = edit(&scratch.config_file(), "/nonexistent-editor")
            .expect_err("an editor that is not there is a failure");

        assert!(failed.to_string().contains("exited"), "{failed}");
        assert!(matches!(missing, Failure::Error(_)));
    }
}
