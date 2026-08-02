//! The CLI contract, exercised as a real process.
//!
//! Everything here runs without Bluetooth hardware: argument errors, the help
//! surface, and the no-matching-device path, which is the honest outcome on a
//! CI runner with nothing paired.

use std::fs;
use std::path::PathBuf;
use std::process::Output;
use std::sync::atomic::{AtomicU32, Ordering};

use assert_cmd::Command;

/// A substring no device on any machine can match.
const NO_SUCH_DEVICE: &str = "zzz-no-such-device-zzz";

fn blubat(args: &[&str]) -> Output {
    Command::cargo_bin("blubat")
        .expect("the binary builds")
        .args(args)
        .output()
        .expect("the binary runs")
}

fn code(output: &Output) -> i32 {
    output.status.code().expect("an exit code")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// A bare `blubat` opens the dashboard, which needs a screen to draw on. Piped
/// into a test or a script there is none, so it offers what it can do in text
/// instead of taking over a terminal it does not have, and exits clean: a first
/// run that produces help is not a failure.
#[test]
fn a_bare_invocation_with_nowhere_to_draw_offers_the_commands() {
    let output = blubat(&[]);

    assert_eq!(code(&output), 0);
    assert!(stdout(&output).contains("Usage: blubat"), "{output:?}");
    assert!(stdout(&output).contains("blubat list"), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
}

#[test]
fn help_still_lists_every_subcommand() {
    let printed = stdout(&blubat(&["--help"]));

    assert!(printed.contains("Usage: blubat"), "{printed}");
    for subcommand in ["list", "status", "wait", "config", "notify-test"] {
        assert!(printed.contains(subcommand), "{subcommand} is missing");
    }
}

/// `notify-test` itself is not run here: it posts a real banner. Its help is,
/// because the diagnosis it exists for is the text rather than the banner.
#[test]
fn notify_test_documents_what_a_silent_success_means() {
    let printed = stdout(&blubat(&["notify-test", "--help"]));

    assert!(printed.contains("identity"), "{printed}");
    assert!(printed.contains("muted"), "{printed}");
}

#[test]
fn help_and_version_exit_clean() {
    for args in [&["--help"][..], &["--version"], &["status", "--help"]] {
        assert_eq!(code(&blubat(args)), 0, "{args:?}");
    }
}

#[test]
fn no_matching_device_is_exit_three_with_a_clear_line_on_stderr() {
    let output = blubat(&["status", "--device", NO_SUCH_DEVICE]);

    assert_eq!(code(&output), 3);
    assert!(
        output.stdout.is_empty(),
        "nothing a script could read as a level"
    );
    assert!(stderr(&output).contains(NO_SUCH_DEVICE), "{output:?}");
}

#[test]
fn no_matching_device_is_exit_three_for_every_output_format() {
    for format in ["--json", "--number"] {
        let output = blubat(&["status", "--device", NO_SUCH_DEVICE, format]);

        assert_eq!(code(&output), 3, "{format}");
        assert!(output.stdout.is_empty(), "{format}");
    }
}

/// Runs on a CI machine with nothing paired and on a desk with several, so it
/// asserts the contract both have in common rather than any device's reading.
#[test]
fn list_json_is_an_array_whatever_the_machine_has_paired() {
    let output = blubat(&["list", "--json"]);
    let printed = stdout(&output);

    assert!(matches!(code(&output), 0 | 3), "{output:?}");
    assert!(
        serde_json::from_str::<serde_json::Value>(&printed).is_ok_and(|json| json.is_array()),
        "{printed}"
    );
}

#[test]
fn incompatible_output_flags_are_an_error_exit() {
    let output = blubat(&["status", "--json", "--number"]);

    assert_eq!(code(&output), 1);
    assert!(
        stderr(&output).contains("cannot be used with"),
        "{output:?}"
    );
}

#[test]
fn an_unknown_subcommand_is_an_error_exit() {
    assert_eq!(code(&blubat(&["dashboard"])), 1);
}

#[test]
fn wait_requires_a_device_and_a_target() {
    let scratch = Scratch::new();

    assert_eq!(code(&blubat(&["--config", &scratch.path(), "wait"])), 1);
    assert_eq!(
        code(&blubat(&[
            "--config",
            &scratch.path(),
            "wait",
            "--device",
            "trackpad"
        ])),
        1
    );
}

#[test]
fn wait_rejects_a_target_no_device_could_reach() {
    let scratch = Scratch::new();
    let output = blubat(&[
        "--config",
        &scratch.path(),
        "wait",
        "--device",
        "trackpad",
        "--until",
        "101",
    ]);

    assert_eq!(code(&output), 1);
    assert!(
        stderr(&output).contains("percentage between 0 and 100"),
        "{output:?}"
    );
}

#[test]
fn wait_rejects_an_interval_it_cannot_measure() {
    let scratch = Scratch::new();
    let output = blubat(&[
        "--config",
        &scratch.path(),
        "wait",
        "--device",
        "trackpad",
        "--until",
        "100",
        "--interval",
        "soon",
    ]);

    assert_eq!(code(&output), 1);
    assert!(stderr(&output).contains("is not a duration"), "{output:?}");
}

/// The two configs a wait can be run under: none at all, and one naming a
/// notification sound, which is the only key `wait` reads.
#[test]
fn wait_gives_up_when_its_timeout_expires() {
    let scratch = Scratch::new();

    for config in [
        scratch.file(),
        scratch.written("[notifications]\nsound = \"Ping\"\n"),
    ] {
        let output = blubat(&[
            "--config",
            &config.display().to_string(),
            "wait",
            "--device",
            NO_SUCH_DEVICE,
            "--until",
            "100",
            "--interval",
            "0s",
            "--timeout",
            "0s",
        ]);

        assert_eq!(code(&output), 1, "{output:?}");
        assert!(stderr(&output).contains("gave up waiting"), "{output:?}");
    }
}

/// A config file in a scratch directory that removes itself.
///
/// Every config command is pointed at one of these with `--config`, so the
/// tests never read or write the config file of whoever is running them.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);

        let path = std::env::temp_dir().join(format!(
            "blubat-cli-tests-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = fs::remove_dir_all(&path);

        Self(path)
    }

    fn file(&self) -> PathBuf {
        self.0.join("config.toml")
    }

    fn written(&self, contents: &str) -> PathBuf {
        fs::create_dir_all(&self.0).expect("a scratch directory");
        fs::write(self.file(), contents).expect("a written config");

        self.file()
    }

    fn path(&self) -> String {
        self.file().display().to_string()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn config_path_prints_the_file_it_would_read() {
    let resolved = stdout(&blubat(&["config", "path"]));

    assert!(
        resolved.trim().ends_with("blubat/config.toml"),
        "{resolved}"
    );
    assert!(resolved.starts_with('/'), "{resolved}");
}

#[test]
fn an_explicit_config_replaces_the_resolved_one() {
    let scratch = Scratch::new();
    let output = blubat(&["--config", &scratch.path(), "config", "path"]);

    assert_eq!(code(&output), 0);
    assert_eq!(stdout(&output).trim(), scratch.path());
}

#[test]
fn validating_a_missing_file_passes_because_defaults_are_a_config() {
    let scratch = Scratch::new();
    let output = blubat(&["--config", &scratch.path(), "config", "validate"]);

    assert_eq!(code(&output), 0);
    assert!(stdout(&output).contains("no config file"), "{output:?}");
    assert!(!scratch.file().exists(), "validating created a file");
}

#[test]
fn validating_a_usable_file_passes() {
    let scratch = Scratch::new();
    let path = scratch.written("[defaults]\nlow = 25\n[notifications]\nsound = \"Ping\"\n");

    let output = blubat(&[
        "--config",
        &path.display().to_string(),
        "config",
        "validate",
    ]);

    assert_eq!(code(&output), 0);
    assert!(stdout(&output).contains("ok"), "{output:?}");
}

#[test]
fn validating_a_broken_file_fails_with_the_line_it_is_on() {
    let scratch = Scratch::new();
    let path = scratch.written("[defaults]\nlow = 20\ncritical = \"ten\"\n");

    let output = blubat(&[
        "--config",
        &path.display().to_string(),
        "config",
        "validate",
    ]);

    assert_eq!(code(&output), 1);
    assert!(stderr(&output).contains("line 3"), "{output:?}");
}

#[test]
fn validating_thresholds_that_cannot_hold_fails() {
    let scratch = Scratch::new();
    let path = scratch.written("[defaults]\nlow = 20\nhigh = 15\n");

    let output = blubat(&[
        "--config",
        &path.display().to_string(),
        "config",
        "validate",
    ]);

    assert_eq!(code(&output), 1);
    assert!(stderr(&output).contains("must be below high"), "{output:?}");
}
