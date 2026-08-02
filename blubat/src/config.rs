//! `blubat config`: where the file is, opening it, checking it, and the two
//! lines the dashboard writes into it.
//!
//! `edit` hands the file to the editor and waits, and whether a file exists
//! afterwards is the editor's decision, not blubat's, so a machine that has
//! never been configured stays that way. [`save_dashboard`] is the exception:
//! `h` and `i` on the dashboard write `[dashboard] hidden` and `[dashboard]
//! hide_inactive` respectively, each only its own key and nothing else, ever.
//! [`editor`] and the private `edit` below it are also what the dashboard's
//! own `c` opens the file in, reached through [`crate::tui::editor::Cli`]
//! rather than duplicated there.

use std::ffi::OsString;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command as Process;

use blubat_core::{Config, Device, Paths};
use toml_edit::{Array, DocumentMut, Item, Table, value};

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

/// Writes `[dashboard] hidden`, `[dashboard] hide_inactive`, or both, leaving
/// the rest of the file, and whichever of the two is not given, exactly as it
/// was.
///
/// The one table blubat ever writes back, from either of the two keys that
/// maintain it: `h` and `i` both call this rather than each keeping a write of
/// its own. Each names only the field it changed, so a write from one key
/// never carries the other's possibly stale in-memory value over a change the
/// file gained since this dashboard last read it, whether that was a hand
/// edit or a second blubat's own write. A surgical edit rather than a
/// re-serialisation, since the comments, the blank lines and the order of
/// everything else in the file are the user's. A file that is not there yet is
/// created holding whichever of the two keys was given.
pub fn save_dashboard(
    path: &Path,
    hidden: Option<&[String]>,
    hide_inactive: Option<bool>,
) -> Result<(), String> {
    document(path)
        .and_then(|mut document| {
            set_dashboard(&mut document, hidden, hide_inactive)?;

            write_atomically(path, &document.to_string())
        })
        .map_err(|problem| format!("{}: {problem}", path.display()))
}

/// The file as it stands, or an empty document where there is no file yet.
fn document(path: &Path) -> Result<DocumentMut, String> {
    match fs::read_to_string(path) {
        Ok(written) => written
            .parse()
            .map_err(|error: toml_edit::TomlError| format!("{error}, so nothing was written")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(DocumentMut::new()),
        Err(error) => Err(error.to_string()),
    }
}

fn set_dashboard(
    document: &mut DocumentMut,
    hidden: Option<&[String]>,
    hide_inactive: Option<bool>,
) -> Result<(), String> {
    document
        .entry("dashboard")
        .or_insert(Item::Table(Table::new()))
        .as_table_like_mut()
        .ok_or_else(|| String::from("[dashboard] is not a table"))
        .map(|dashboard| {
            if let Some(hidden) = hidden {
                dashboard.insert(
                    "hidden",
                    value(hidden.iter().map(String::as_str).collect::<Array>()),
                );
            }
            if let Some(hide_inactive) = hide_inactive {
                dashboard.insert("hide_inactive", value(hide_inactive));
            }
        })
}

/// Writes through a partial file and a rename, so nothing reads half a config.
///
/// A rename that failed takes the partial file with it: leaving one beside the
/// user's config would be blubat writing a second file into a directory it is
/// only ever meant to maintain one table in.
fn write_atomically(path: &Path, contents: &str) -> Result<(), String> {
    let mut partial = OsString::from(path);
    partial.push(".partial");
    let partial = PathBuf::from(partial);

    let written = path
        .parent()
        .map_or(Ok(()), fs::create_dir_all)
        .and_then(|()| fs::write(&partial, contents))
        .and_then(|()| fs::rename(&partial, path));

    if written.is_err() {
        let _ = fs::remove_file(&partial);
    }

    written.map_err(|error| error.to_string())
}

/// The editor to open the file in, `$EDITOR` or failing that `$VISUAL`.
///
/// Takes the lookup rather than reading the environment, since a test that set
/// a variable would be setting it for every other test running beside it.
/// `pub(crate)` so the dashboard's `c` resolves the same way `blubat config
/// edit` does, rather than guessing at $EDITOR a second time.
pub(crate) fn editor(variable: impl Fn(&str) -> Option<String>) -> Result<String, Failure> {
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
/// `pub(crate)` for the same reason [`editor`] is: the dashboard's `c` spawns
/// this rather than a second copy of it.
pub(crate) fn edit(path: &Path, editor: &str) -> Result<(), Failure> {
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
    use blubat_core::{Address, ChargeState, Levels, Source, Timestamp};

    use crate::scratch::Scratch;

    use super::*;

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
        let path =
            scratch.write_config("[defaults]\nlow = 25\n\n[notifications]\nsound = \"Ping\"\n");

        let (outcome, printed) = check(&path, Vec::new());

        assert_eq!(outcome, Ok(()));
        assert!(printed.contains("ok"), "{printed}");
    }

    #[test]
    fn a_malformed_file_fails_with_the_line_it_is_on() {
        let scratch = Scratch::new();
        let path = scratch.write_config("[defaults]\nlow = 20\ncritical = \"ten\"\n");

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
        let path = scratch.write_config("[defaults]\nlwo = 20\n");

        assert!(check(&path, Vec::new()).0.is_err());
    }

    #[test]
    fn thresholds_that_cannot_hold_fail_with_every_problem_listed() {
        let scratch = Scratch::new();
        let path = scratch.write_config("[defaults]\nlow = 20\nhigh = 15\n");

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
        let path = scratch.write_config("[[device]]\nmatch = \"trackpad\"\nlow = 25\n");

        let (matched, quiet) = check(&path, vec![trackpad()]);
        let (unmatched, warned) = check(&path, Vec::new());

        assert_eq!(matched, Ok(()));
        assert!(!quiet.contains("warning"), "{quiet}");
        assert_eq!(unmatched, Ok(()), "a device may simply be switched off");
        assert!(warned.contains("warning"), "{warned}");
        assert!(warned.contains("trackpad"), "{warned}");
    }

    /// One realistic file: a comment, arrays of tables, a table blubat's own
    /// `Config` knows nothing about, and a trailing comment on the very line
    /// being rewritten. Asserted whole, since the claim is that everything
    /// outside `hidden` and `hide_inactive` is exactly as the user left it.
    #[test]
    fn hiding_a_device_writes_the_table_and_nothing_else_in_the_file() {
        let scratch = Scratch::new();
        let written = "# my thresholds\n[defaults]\nlow = 25\n\n\
             [[device]]\nmatch = \"trackpad\"\nlow = 30\n\n\
             [[hook]]\nevent = \"low_battery\"\ncommand = \"nag\"  # shouts\n\n\
             [[hook]]\nevent = \"charged\"\ncommand = \"unplug\"\n\n\
             [experimental]\nnothing_blubat_knows = true\n\n\
             [dashboard]\nhidden = [\"MX Master\"] # for now\nsort = \"name\"\nhide_inactive = false\n";
        let path = scratch.write_config(written);

        assert_eq!(
            save_dashboard(
                &path,
                Some(&["MX Master".to_string(), "30-82-16".to_string()]),
                Some(true)
            ),
            Ok(())
        );
        assert_eq!(
            fs::read_to_string(&path).expect("a written config"),
            written
                .replace(
                    "hidden = [\"MX Master\"] # for now",
                    "hidden = [\"MX Master\", \"30-82-16\"]"
                )
                .replace("hide_inactive = false", "hide_inactive = true"),
            "the comments, the order and every other table survive the write"
        );
    }

    /// `i` names only `hide_inactive`, so the key is added to a table that
    /// predates it without the write carrying `hidden` along, and without
    /// disturbing whatever `hidden` the table already holds.
    #[test]
    fn hide_inactive_is_added_to_a_table_that_predates_it() {
        let scratch = Scratch::new();
        let written = "[dashboard]\nhidden = [\"MX Master\"]\nsort = \"name\"\n";
        let path = scratch.write_config(written);

        assert_eq!(save_dashboard(&path, None, Some(true)), Ok(()));

        let loaded = Config::load(&path).expect("blubat reads back what it wrote");
        assert!(
            loaded.dashboard.hide_inactive,
            "hide_inactive round trips even though this file never had it"
        );
        assert_eq!(
            loaded.dashboard.hidden,
            ["MX Master".to_string()],
            "hidden is untouched: this write named hide_inactive alone"
        );
    }

    /// The bug the shared write almost reintroduced: a write naming one field
    /// must never carry the other's in-memory copy over a value the file
    /// gained since this dashboard last read it, whichever field is which.
    #[test]
    fn writing_one_field_leaves_the_other_on_disk_untouched() {
        let hiding = Scratch::new();
        let path = hiding.write_config("[dashboard]\nhide_inactive = true\n");
        assert_eq!(
            save_dashboard(&path, Some(&["30-82-16".to_string()]), None),
            Ok(())
        );
        assert!(
            Config::load(&path).expect("parses").dashboard.hide_inactive,
            "a hand edit to hide_inactive survives a write that only named hidden"
        );

        let toggling = Scratch::new();
        let path = toggling.write_config("[dashboard]\nhidden = [\"30-82-16\"]\n");
        assert_eq!(save_dashboard(&path, None, Some(true)), Ok(()));
        assert_eq!(
            Config::load(&path).expect("parses").dashboard.hidden,
            ["30-82-16".to_string()],
            "a hand edit to hidden survives a write that only named hide_inactive"
        );
    }

    /// The one place blubat writes to a file it does not own, so a `[dashboard]`
    /// that is not a table is reported and left exactly as it was, the same
    /// guarantee a file that will not parse gets.
    #[test]
    fn a_dashboard_that_is_not_a_table_is_reported_rather_than_replaced() {
        let scratch = Scratch::new();
        let path = scratch.write_config("dashboard = 5\n");

        let problem = save_dashboard(&path, Some(&["30-82-16".to_string()]), Some(false))
            .expect_err("there is nowhere to put the list");

        assert!(problem.contains("[dashboard] is not a table"), "{problem}");
        assert_eq!(
            fs::read_to_string(&path).expect("still there"),
            "dashboard = 5\n"
        );
    }

    #[test]
    fn hiding_the_first_device_creates_a_file_holding_that_table_alone() {
        let scratch = Scratch::new();
        let path = scratch.config_file();

        assert_eq!(
            save_dashboard(&path, Some(&["30-82-16".to_string()]), None),
            Ok(())
        );
        assert_eq!(
            fs::read_to_string(&path).expect("a written config"),
            "[dashboard]\nhidden = [\"30-82-16\"]\n"
        );
        assert_eq!(
            Config::load(&path).expect("blubat reads back what it wrote"),
            Config {
                dashboard: blubat_core::Dashboard {
                    hidden: vec!["30-82-16".to_string()],
                    ..blubat_core::Dashboard::default()
                },
                ..Config::default()
            }
        );
    }

    #[test]
    fn showing_the_last_device_again_leaves_the_list_empty_rather_than_absent() {
        let scratch = Scratch::new();
        let path = scratch.write_config("[dashboard]\nhidden = [\"30-82-16\"]\n");

        assert_eq!(save_dashboard(&path, Some(&[]), None), Ok(()));
        assert_eq!(
            fs::read_to_string(&path).expect("a written config"),
            "[dashboard]\nhidden = []\n"
        );
    }

    #[test]
    fn a_file_that_will_not_parse_is_reported_rather_than_overwritten() {
        let scratch = Scratch::new();
        let path = scratch.write_config("[defaults\nlow = 25\n");

        let problem = save_dashboard(&path, Some(&["30-82-16".to_string()]), None)
            .expect_err("an unclosed table header is not TOML");

        assert!(problem.contains(&path.display().to_string()), "{problem}");
        assert_eq!(
            fs::read_to_string(&path).expect("still there"),
            "[defaults\nlow = 25\n",
            "a file blubat cannot read is a file it will not rewrite"
        );
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
