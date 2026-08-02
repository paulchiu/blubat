//! The CLI contract, exercised as a real process.
//!
//! Everything here runs without Bluetooth hardware: argument errors, the help
//! surface, and the no-matching-device path, which is the honest outcome on a
//! CI runner with nothing paired.

use std::process::Output;

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
    for subcommand in ["list", "status", "wait"] {
        assert!(printed.contains(subcommand), "{subcommand} is missing");
    }
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
    assert_eq!(code(&blubat(&["wait"])), 1);
    assert_eq!(code(&blubat(&["wait", "--device", "trackpad"])), 1);
}

#[test]
fn wait_rejects_a_target_no_device_could_reach() {
    let output = blubat(&["wait", "--device", "trackpad", "--until", "101"]);

    assert_eq!(code(&output), 1);
    assert!(
        stderr(&output).contains("percentage between 0 and 100"),
        "{output:?}"
    );
}

#[test]
fn wait_rejects_an_interval_it_cannot_measure() {
    let output = blubat(&[
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

#[test]
fn wait_gives_up_when_its_timeout_expires() {
    let output = blubat(&[
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

    assert_eq!(code(&output), 1);
    assert!(stderr(&output).contains("gave up waiting"), "{output:?}");
}
