//! `blubat config`: where the file is, opening it, checking it, and the
//! commented guide a config file is introduced to.
//!
//! `edit` seeds the full template before opening the editor when there is no
//! file yet, and introduces a file that predates the template to its own
//! defaults the same way, once, before handing either to the editor; see
//! [`annotate`] for what that adds and what it leaves alone. [`save_dashboard`]
//! is the exception: `h` and `i` on the dashboard write `[dashboard] hidden`
//! and `[dashboard] hide_inactive` respectively, each only its own key and
//! nothing else, ever. [`editor`] and the private `edit` below it are also
//! what the dashboard's own `c` opens the file in, reached through
//! [`crate::tui::editor::Cli`] rather than duplicated there.

mod template;

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
        Command::Validate => validate(path, &mut io::stdout(), || reading(paths).devices),
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

/// Introduces a config file that predates the self-documenting template to
/// its own defaults, once, leaving the user's text exactly as it was.
///
/// Additive rather than a rewrite: a marker line and a short pointer go in
/// front, and a guide section for whichever tables, or the device or hook
/// samples, the file does not already have goes on the end. A file already
/// carrying the marker is left alone, whether blubat wrote it or the user
/// kept the line after deleting the sections under it as an opt out, and so
/// is a file `Config::parse` rejects: the load path is what reports a parse
/// error, not this. Every call site runs this ahead of its own load and
/// swallows what it returns, since a config blubat cannot document is still
/// a config it can read.
pub(crate) fn annotate(path: &Path) -> Result<(), String> {
    let original = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.to_string()),
    };

    if original
        .lines()
        .any(|line| line.starts_with(template::MARKER))
    {
        return Ok(());
    }

    let Ok(parsed) = Config::parse(&original) else {
        return Ok(());
    };
    let Ok(document) = original.parse::<DocumentMut>() else {
        return Ok(());
    };

    let composed = compose(&original, &document, &parsed);

    match Config::parse(&composed) {
        Ok(reparsed) if reparsed == parsed => write_atomically(path, &composed),
        _ => Ok(()),
    }
}

/// The annotated file: the marker block, a blank line, the original text
/// exactly as it was, then a guide section for each table, or the device or
/// hook sample, the file does not already have.
fn compose(original: &str, document: &DocumentMut, parsed: &Config) -> String {
    let mut composed = String::from(template::MIGRATED);

    // An empty original (an existing but empty file) has nothing to append,
    // so skip it rather than let append's blank-line separator run for text
    // that turns out to be nothing.
    if !original.is_empty() {
        template::append(&mut composed, original);
    }

    let missing = template::SCALAR_SECTIONS
        .iter()
        .copied()
        .filter(|(table, _)| !document.contains_key(table))
        .map(|(_, section)| section)
        .chain(parsed.devices.is_empty().then_some(template::DEVICE_SAMPLE))
        .chain(parsed.hooks.is_empty().then_some(template::HOOK_SAMPLE));

    for section in missing {
        template::append(&mut composed, section);
    }

    composed
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
/// A file that does not exist yet is seeded with the full template first, so
/// a machine that has never been configured opens the whole schema rather
/// than a blank page; the parent directory that creates is the only one the
/// editor needs. A file that already exists is introduced to its own
/// defaults by [`annotate`] instead, which never rewrites a file it cannot
/// read: an editor that then leaves the file unparsable is a load error
/// exactly as it was before. `pub(crate)` for the same reason [`editor`] is:
/// the dashboard's `c` spawns this rather than a second copy of it.
pub(crate) fn edit(path: &Path, editor: &str) -> Result<(), Failure> {
    let (program, arguments) = split(editor);

    if path.exists() {
        // Best effort: never keep the editor from opening a file annotate
        // cannot read or has already introduced to the guide.
        let _ = annotate(path);
    } else {
        write_atomically(path, &template::full())
            .map_err(|error| Failure::Error(format!("{}: {error}", path.display())))?;
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
    use blubat_core::{
        Address, ChargeState, Dashboard, Defaults, Levels, Notifications, Poll, Rgb, Scheme,
        Source, Thresholds, Timestamp,
    };

    use crate::scratch::Scratch;

    use super::*;

    fn trackpad() -> Device {
        Device {
            address: Address::parse("30-82-16-f2-24-90").expect("valid address"),
            name: "Paul\u{2019}s Magic Trackpad".to_string(),
            kind: None,
            transport: None,
            vendor_id: None,
            product_id: None,
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

    /// `inactive_after` is not one of the two keys blubat ever writes, so a
    /// hand set value has to survive a write naming only `hidden`.
    #[test]
    fn a_hand_set_inactive_after_survives_a_write_to_hidden() {
        let scratch = Scratch::new();
        let path = scratch.write_config("[dashboard]\ninactive_after = \"5m\"\n");

        assert_eq!(
            save_dashboard(&path, Some(&["30-82-16".to_string()]), None),
            Ok(())
        );
        assert_eq!(
            Config::load(&path)
                .expect("parses")
                .dashboard
                .inactive_after,
            std::time::Duration::from_secs(300),
            "a hand edit to inactive_after survives a write that only named hidden"
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
    fn editing_seeds_the_template_when_there_is_no_file_yet() {
        let scratch = Scratch::new();
        let path = scratch.config_file();

        assert_eq!(edit(&path, "/usr/bin/true"), Ok(()));

        assert!(path.exists(), "the editor had something to open");
        assert_eq!(
            Config::load(&path).expect("the template parses"),
            Config::default(),
            "a template only file behaves exactly like no file at all"
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

    /// The template is what a config file is seeded with, so it has to be
    /// exactly as inert as no file at all.
    #[test]
    fn the_template_as_written_is_the_built_in_config() {
        let config = Config::parse(&template::full()).expect("the template parses");

        assert_eq!(config, Config::default());
        assert!(config.problems().is_empty(), "{:?}", config.problems());
    }

    /// Uncommenting every `# key = value` line, and only those, has to
    /// reproduce the numbers the prose above each section claims: this is
    /// what keeps the template honest when a struct gains a key.
    #[test]
    fn uncommenting_every_key_reproduces_the_defaults_it_names() {
        let uncommented = template::full()
            .lines()
            .map(|line| {
                if line.starts_with("##") {
                    line
                } else {
                    line.strip_prefix("# ").unwrap_or(line)
                }
            })
            .collect::<Vec<_>>()
            .join("\n");

        let config = Config::parse(&uncommented).expect("every commented line is valid TOML");

        assert!(config.problems().is_empty(), "{:?}", config.problems());
        assert_eq!(config.poll, Poll::default());
        assert_eq!(config.notifications, Notifications::default());
        assert_eq!(config.dashboard, Dashboard::default());
        assert_eq!(
            config.defaults,
            Defaults {
                low: Some(Thresholds::BUILT_IN.low),
                critical: Some(Thresholds::BUILT_IN.critical),
                high: Some(Thresholds::BUILT_IN.high),
                rearm_margin: Some(Thresholds::BUILT_IN.rearm_margin),
            },
            "commented as the built-in numbers a device without a block falls \
             through to, not the unset default the struct itself has"
        );
        assert_eq!(config.theme.scheme, Scheme::Dark);
        assert_eq!(
            config.theme.accent,
            Some(Rgb::parse("#39c5cf").expect("parses"))
        );
        assert_eq!(
            config.theme.critical,
            Some(Rgb::parse("#f47067").expect("parses"))
        );
        assert_eq!(
            config.theme.low,
            Some(Rgb::parse("#c69026").expect("parses"))
        );
        assert_eq!(
            config.theme.ok,
            Some(Rgb::parse("#57ab5a").expect("parses"))
        );
        assert_eq!(config.theme.charging_glyph.as_deref(), Some("+"));
        assert_eq!(config.devices.len(), 1, "the device sample took effect");
        assert_eq!(config.devices[0].pattern, "trackpad");
        assert_eq!(config.hooks.len(), 1, "the hook sample took effect");
        assert_eq!(config.hooks[0].command, "~/.config/blubat/hooks/nag.sh");
    }

    #[test]
    fn annotating_a_minimal_file_adds_only_the_missing_tables() {
        let scratch = Scratch::new();
        let original = "[dashboard]\nhidden = [\"MX Master\"]\nhide_inactive = true\n";
        let path = scratch.write_config(original);

        assert_eq!(annotate(&path), Ok(()));

        let written = fs::read_to_string(&path).expect("still there");
        assert!(written.contains(original), "{written}");
        assert_eq!(
            Config::parse(&written).expect("parses"),
            Config::parse(original).expect("parses")
        );
        assert_eq!(
            written.matches("[dashboard]").count(),
            1,
            "the table the file already had gains no second header"
        );
        for table in ["[poll]", "[notifications]", "[defaults]", "[theme]"] {
            assert!(written.contains(table), "{written}");
        }
    }

    #[test]
    fn annotating_twice_changes_nothing_the_second_time() {
        let scratch = Scratch::new();
        let path = scratch.write_config("[defaults]\nlow = 25\n");

        assert_eq!(annotate(&path), Ok(()));
        let once = fs::read_to_string(&path).expect("annotated");

        assert_eq!(annotate(&path), Ok(()));
        let twice = fs::read_to_string(&path).expect("still there");

        assert_eq!(once, twice);
    }

    #[test]
    fn a_file_that_already_carries_the_marker_is_left_alone() {
        let scratch = Scratch::new();
        let written = "## blubat configuration, guide v1\nwhatever the user left here\n";
        let path = scratch.write_config(written);

        assert_eq!(annotate(&path), Ok(()));
        assert_eq!(fs::read_to_string(&path).expect("still there"), written);
    }

    #[test]
    fn an_unparsable_file_is_left_for_the_load_path_to_report() {
        let scratch = Scratch::new();
        let written = "[defaults\nlow = 25\n";
        let path = scratch.write_config(written);

        assert_eq!(annotate(&path), Ok(()));
        assert_eq!(fs::read_to_string(&path).expect("still there"), written);
    }

    #[test]
    fn a_missing_file_is_left_for_seeding_to_write() {
        let scratch = Scratch::new();

        assert_eq!(annotate(&scratch.config_file()), Ok(()));
        assert!(!scratch.config_file().exists(), "annotate created nothing");
    }

    /// An existing but empty file (created with `touch`, or truncated) is a
    /// real file, not the missing-file case seeding handles, so it still
    /// takes the single blank line every other migration produces rather
    /// than an extra one where the absent original text would have gone.
    #[test]
    fn an_empty_existing_file_migrates_with_a_single_blank_line() {
        let scratch = Scratch::new();
        let path = scratch.write_config("");

        assert_eq!(annotate(&path), Ok(()));

        let written = fs::read_to_string(&path).expect("still there");
        assert!(
            !written.contains("\n\n\n"),
            "no run of blank lines anywhere in the file: {written}"
        );
        assert!(Config::parse(&written).is_ok(), "{written}");
    }

    #[test]
    fn a_file_without_a_trailing_newline_still_migrates_to_valid_toml() {
        let scratch = Scratch::new();
        let path = scratch.write_config("[defaults]\nlow = 25");

        assert_eq!(annotate(&path), Ok(()));

        let written = fs::read_to_string(&path).expect("still there");
        assert!(written.contains("[defaults]\nlow = 25"), "{written}");
        assert!(Config::parse(&written).is_ok(), "{written}");
    }
}
