//! The CLI contract, exercised as a real process.
//!
//! Everything here runs without Bluetooth hardware: argument errors, the help
//! surface, and the no-matching-device path, which is the honest outcome on a
//! CI runner with nothing paired.
//!
//! Anything that reads or writes a file is pointed at a scratch directory with
//! both `--config` and `--state-dir`, so a machine with a daemon installed and
//! a machine with none run these the same way, and nothing here can drop a
//! watch file into the state directory of whoever is running them.

use std::fs::{self, File, OpenOptions};
use std::os::fd::AsRawFd;
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

/// The same, with every file blubat touches inside `scratch`.
fn blubat_in(scratch: &Scratch, args: &[&str]) -> Output {
    let rooted = [
        "--config",
        &scratch.path(),
        "--state-dir",
        &scratch.state_path(),
    ];

    blubat(&[&rooted[..], args].concat())
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
    for subcommand in ["list", "status", "wait", "config", "daemon", "notify-test"] {
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

/// The daemon is never started by a test: these run its argument surface only,
/// because everything past it either holds the terminal forever or asks
/// launchd to load an agent on the machine running them.
#[test]
fn daemon_lists_the_four_things_it_can_be_asked_to_do() {
    let printed = stdout(&blubat(&["daemon", "--help"]));

    for subcommand in ["run", "install", "uninstall", "status"] {
        assert!(printed.contains(subcommand), "{subcommand} is missing");
    }
}

/// `cached-levels` is the sweep's own helper rather than something to run, and
/// running it here would ask the terminal for Bluetooth and be aborted for it.
#[test]
fn the_helper_the_sweep_spawns_is_kept_out_of_the_daemons_help() {
    let printed = stdout(&blubat(&["daemon", "--help"]));

    assert!(!printed.contains("cached-levels"), "{printed}");
}

#[test]
fn wait_says_it_may_hand_the_wait_to_a_daemon() {
    let printed = stdout(&blubat(&["wait", "--help"]));

    assert!(printed.contains("daemon"), "{printed}");
    assert!(printed.contains("one-shot watch"), "{printed}");
}

#[test]
fn an_unknown_daemon_subcommand_is_an_error_exit() {
    assert_eq!(code(&blubat(&["daemon", "start"])), 1);
}

#[test]
fn an_unknown_subcommand_is_an_error_exit() {
    assert_eq!(code(&blubat(&["dashboard"])), 1);
}

#[test]
fn wait_requires_a_device_and_a_target() {
    let scratch = Scratch::new();

    assert_eq!(code(&blubat_in(&scratch, &["wait"])), 1);
    assert_eq!(
        code(&blubat_in(&scratch, &["wait", "--device", "trackpad"])),
        1
    );
}

#[test]
fn wait_rejects_a_target_no_device_could_reach() {
    let scratch = Scratch::new();
    let output = blubat_in(
        &scratch,
        &["wait", "--device", "trackpad", "--until", "101"],
    );

    assert_eq!(code(&output), 1);
    assert!(
        stderr(&output).contains("percentage between 0 and 100"),
        "{output:?}"
    );
}

#[test]
fn wait_rejects_an_interval_it_cannot_measure() {
    let scratch = Scratch::new();
    let output = blubat_in(
        &scratch,
        &[
            "wait",
            "--device",
            "trackpad",
            "--until",
            "100",
            "--interval",
            "soon",
        ],
    );

    assert_eq!(code(&output), 1);
    assert!(stderr(&output).contains("is not a duration"), "{output:?}");
}

/// The arguments of a wait that runs here rather than being handed over.
const IMPATIENT: [&str; 8] = [
    "wait",
    "--device",
    NO_SUCH_DEVICE,
    "--until",
    "100",
    "--interval",
    "0s",
    "--timeout",
];

/// The two configs a wait can be run under: none at all, and one naming a
/// notification sound, which is the only key `wait` reads.
#[test]
fn wait_gives_up_when_its_timeout_expires() {
    for contents in ["", "[notifications]\nsound = \"Ping\"\n"] {
        let scratch = Scratch::new();
        if !contents.is_empty() {
            scratch.written(contents);
        }

        let output = blubat_in(&scratch, &[&IMPATIENT[..], &["0s"]].concat());

        assert_eq!(code(&output), 1, "{output:?}");
        assert!(stderr(&output).contains("gave up waiting"), "{output:?}");
    }
}

/// The handoff, both ways round. Which one a wait takes turns on one lock file,
/// and getting that wrong either parks the wait or ignores a running daemon.
#[test]
fn a_wait_is_handed_to_a_daemon_that_is_running_and_kept_here_when_none_is() {
    let scratch = Scratch::new();
    let watches = scratch.state().join("watches");

    let alone = blubat_in(&scratch, &[&IMPATIENT[..], &["0s"]].concat());
    assert_eq!(code(&alone), 1, "{alone:?}");
    assert!(!watches.exists(), "nothing was registered for nobody");

    let _daemon = scratch.daemon_lock();
    let handed = blubat_in(&scratch, &[&IMPATIENT[..], &["10m"]].concat());

    assert_eq!(code(&handed), 0, "{handed:?}");
    assert!(stdout(&handed).contains("registered as"), "{handed:?}");
    assert_eq!(
        fs::read_dir(&watches).expect("a watch directory").count(),
        1,
        "one watch, waiting for the daemon to pick it up"
    );
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

    /// Where blubat keeps its own files under this directory.
    fn state(&self) -> PathBuf {
        self.0.join("state")
    }

    fn state_path(&self) -> String {
        self.state().display().to_string()
    }

    /// A daemon lock this scratch's blubat sees as a running daemon, held for
    /// as long as the returned file is open.
    #[allow(unsafe_code)]
    fn daemon_lock(&self) -> File {
        fs::create_dir_all(self.state()).expect("a state directory");

        let lock = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(self.state().join("daemon.lock"))
            .expect("a lock file");

        // SAFETY: `flock` is given a descriptor this test owns and reads no
        // memory it owns.
        assert_eq!(
            unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
            0,
            "nothing else is holding a lock in a fresh scratch directory"
        );

        lock
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
