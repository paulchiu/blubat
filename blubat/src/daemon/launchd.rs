//! The LaunchAgent: the plist blubat writes for it, and the `launchctl` calls
//! around that file.
//!
//! Installing is always something the user asks for. blubat never writes this
//! plist on a first run, on an upgrade, or as a side effect of anything else,
//! so a machine that has not run `blubat daemon install` has no resident blubat
//! on it and nothing to find.
//!
//! `launchctl` is reached through a trait and the plist path is handed in, so
//! every test here runs against a recorder and a scratch directory: nothing in
//! this repository can bootstrap an agent or write to `~/Library`.

use std::fs;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::Duration;

use blubat_core::Paths;

use crate::Failure;

/// The agent's label, which is also the name of its file.
const LABEL: &str = "com.paulchiu.blubat";

/// How long launchd leaves between restarts.
///
/// The agent restarts only when the daemon exits badly, and this is what stops
/// one that fails on startup from being restarted as fast as it can fail.
const THROTTLE: u32 = 30;

/// How many times an install asks launchd to bootstrap the agent.
const ATTEMPTS: u32 = 3;

/// How long it leaves launchd between those attempts.
const SETTLE: Duration = Duration::from_millis(250);

/// The PATH the daemon runs with.
///
/// launchd inherits almost nothing, and blubat shells out to `system_profiler`
/// in `/usr/sbin` and `osascript` in `/usr/bin`.
const PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";

/// One finished `launchctl` call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ran {
    pub code: i32,
    /// Everything it printed, both streams together: `launchctl` answers on
    /// stdout and complains on stderr, and a caller here wants whichever came.
    pub output: String,
}

impl Ran {
    fn worked(&self) -> bool {
        self.code == 0
    }
}

/// Somewhere `launchctl` runs, which a test fills with a recorder.
pub trait Launchctl {
    /// Runs `launchctl` with these arguments and waits for it.
    fn run(&self, arguments: &[&str]) -> Result<Ran, String>;
}

/// The real one.
#[derive(Clone, Copy, Debug, Default)]
pub struct Cli;

impl Launchctl for Cli {
    fn run(&self, arguments: &[&str]) -> Result<Ran, String> {
        Command::new("launchctl")
            .args(arguments)
            .output()
            .map_err(|error| format!("launchctl: {error}"))
            .map(|output| Ran {
                code: output.status.code().unwrap_or(-1),
                output: format!(
                    "{}{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                ),
            })
    }
}

/// Everything in the plist that is particular to one machine.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Agent {
    /// The binary launchd runs, absolute: launchd resolves nothing, having
    /// neither a PATH nor a working directory to resolve a name against.
    program: PathBuf,
    /// The files the daemon reads and writes, named rather than left to be
    /// worked out again. launchd starts the agent with almost no environment,
    /// so a daemon resolving its own paths would land somewhere else than the
    /// blubat that installed it whenever the XDG variables are set, and the two
    /// would then hold different locks and write different state.
    config: PathBuf,
    state: PathBuf,
    home: PathBuf,
    out: PathBuf,
    error: PathBuf,
}

impl Agent {
    /// The agent for the blubat that is running and the files it is using.
    fn resolve(paths: &Paths) -> Result<Self, Failure> {
        Ok(Self {
            program: executable()?,
            config: paths.config_file().to_path_buf(),
            state: paths.state_dir().to_path_buf(),
            home: home()?,
            out: paths.log_file(),
            error: paths.error_log_file(),
        })
    }

    /// The plist launchd reads, which is a pure function of the machine.
    fn plist(&self) -> String {
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
             <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
             \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
             <plist version=\"1.0\">\n\
             <dict>\n\
             \t<key>Label</key>\n\
             \t<string>{LABEL}</string>\n\
             \t<key>ProgramArguments</key>\n\
             \t<array>\n\
             \t\t<string>{program}</string>\n\
             \t\t<string>--config</string>\n\
             \t\t<string>{config}</string>\n\
             \t\t<string>--state-dir</string>\n\
             \t\t<string>{state}</string>\n\
             \t\t<string>daemon</string>\n\
             \t\t<string>run</string>\n\
             \t</array>\n\
             \t<key>EnvironmentVariables</key>\n\
             \t<dict>\n\
             \t\t<key>HOME</key>\n\
             \t\t<string>{home}</string>\n\
             \t\t<key>PATH</key>\n\
             \t\t<string>{PATH}</string>\n\
             \t</dict>\n\
             \t<key>StandardOutPath</key>\n\
             \t<string>{out}</string>\n\
             \t<key>StandardErrorPath</key>\n\
             \t<string>{error}</string>\n\
             \t<key>RunAtLoad</key>\n\
             \t<true/>\n\
             \t<key>KeepAlive</key>\n\
             \t<dict>\n\
             \t\t<key>SuccessfulExit</key>\n\
             \t\t<false/>\n\
             \t</dict>\n\
             \t<key>ThrottleInterval</key>\n\
             \t<integer>{THROTTLE}</integer>\n\
             </dict>\n\
             </plist>\n",
            program = escaped(&self.program),
            config = escaped(&self.config),
            state = escaped(&self.state),
            home = escaped(&self.home),
            out = escaped(&self.out),
            error = escaped(&self.error),
        )
    }
}

/// Where the plist goes, which is the one path of blubat's that is not XDG.
pub fn plist_file() -> Result<PathBuf, Failure> {
    home().map(|home| {
        home.join("Library")
            .join("LaunchAgents")
            .join(format!("{LABEL}.plist"))
    })
}

/// `blubat daemon install`: write the plist and start the agent.
///
/// Whatever was loaded under this label is booted out first, so installing over
/// an older blubat is one command rather than an uninstall and an install.
pub fn install(
    launchctl: &dyn Launchctl,
    paths: &Paths,
    plist: &Path,
    out: &mut impl Write,
) -> Result<(), Failure> {
    let agent = Agent::resolve(paths)?;

    write_plist(plist, &agent.plist())?;
    let _ = launchctl.run(&["bootout", &service()]);
    bootstrap(launchctl, plist, SETTLE)?;

    writeln!(out, "installed {LABEL}")?;
    writeln!(out, "  plist   {}", plist.display())?;
    writeln!(out, "  running {} daemon run", agent.program.display())?;
    writeln!(out, "  config  {}", agent.config.display())?;
    writeln!(out, "  state   {}", agent.state.display())?;
    writeln!(out, "  logging {}", agent.out.display())?;

    Ok(())
}

/// Bootstraps the agent, giving launchd time to finish with the last one.
///
/// `bootout` returns before launchd has torn the old job down, so a bootstrap
/// issued straight after it is refused for a service that is on its way out
/// rather than for anything wrong with the plist. Retrying is what makes
/// installing over a running blubat one command rather than a race.
fn bootstrap(launchctl: &dyn Launchctl, plist: &Path, settle: Duration) -> Result<(), Failure> {
    let target = plist.to_string_lossy().into_owned();
    let mut refused = None;

    for attempt in 0..ATTEMPTS {
        if attempt > 0 {
            thread::sleep(settle);
        }

        let ran = launchctl
            .run(&["bootstrap", &domain(), &target])
            .map_err(Failure::Error)?;

        match succeeded(&ran, "bootstrap") {
            Ok(()) => return Ok(()),
            Err(failure) => refused = Some(failure),
        }
    }

    Err(refused.unwrap_or_else(|| Failure::Error("launchctl bootstrap was never run".to_string())))
}

/// `blubat daemon uninstall`: stop the agent and remove its plist.
///
/// An agent that was not loaded is not a failure: the point of the command is
/// that nothing is left afterwards, which is already true of that one.
pub fn uninstall(
    launchctl: &dyn Launchctl,
    plist: &Path,
    out: &mut impl Write,
) -> Result<(), Failure> {
    let stopped = launchctl
        .run(&["bootout", &service()])
        .map_err(Failure::Error)?;

    removed(plist)?;
    writeln!(out, "removed {LABEL}")?;

    if !stopped.worked() {
        writeln!(out, "  it was not loaded")?;
    }

    Ok(())
}

/// `blubat daemon status`: whether the agent is installed, loaded and running.
pub fn status(
    launchctl: &dyn Launchctl,
    plist: &Path,
    out: &mut impl Write,
) -> Result<(), Failure> {
    let printed = launchctl
        .run(&["print", &service()])
        .map_err(Failure::Error)?;

    for line in describe(plist, plist.exists(), &printed) {
        writeln!(out, "{line}")?;
    }

    Ok(())
}

/// What the plist on disk and `launchctl print` say between them.
///
/// Loaded and running are separate answers: an agent can be bootstrapped and
/// still be between restarts, and a plist can sit on disk with nothing loaded
/// from it after a boot that never ran it.
fn describe(plist: &Path, installed: bool, printed: &Ran) -> Vec<String> {
    let loaded = printed.worked();
    let pid = loaded.then(|| field(&printed.output, "pid")).flatten();

    vec![
        format!("label     {LABEL}"),
        if installed {
            format!("plist     {}", plist.display())
        } else {
            format!("plist     not installed ({})", plist.display())
        },
        format!("loaded    {}", if loaded { "yes" } else { "no" }),
        pid.map_or_else(
            || "running   no".to_string(),
            |pid| format!("running   yes, pid {pid}"),
        ),
    ]
}

/// One `key = value` field out of a `launchctl print` report.
fn field(printed: &str, name: &str) -> Option<String> {
    printed
        .lines()
        .filter_map(|line| line.split_once('='))
        .find(|(key, _)| key.trim() == name)
        .map(|(_, value)| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// The user's GUI domain, which is where a LaunchAgent belongs.
///
/// `bootstrap` and `bootout` name a domain and a service in it; `load` and
/// `unload` have been deprecated since OS X 10.10.
fn domain() -> String {
    // SAFETY: `getuid` reads no memory blubat owns and cannot fail.
    format!("gui/{}", unsafe { libc::getuid() })
}

fn service() -> String {
    format!("{}/{LABEL}", domain())
}

fn home() -> Result<PathBuf, Failure> {
    std::env::home_dir()
        .filter(|home| !home.as_os_str().is_empty())
        .ok_or_else(|| {
            Failure::Error("no home directory to install the LaunchAgent in".to_string())
        })
}

/// The blubat launchd should run, which is the one asking to be installed.
fn executable() -> Result<PathBuf, Failure> {
    std::env::current_exe()
        .map(|path| path.canonicalize().unwrap_or(path))
        .map(|path| stable(&path, |shim| shim.exists()))
        .map_err(|error| Failure::Error(format!("cannot find this blubat: {error}")))
}

/// The path launchd should be given for a resolved executable, pinned to the
/// Homebrew prefix shim when the resolved path is a Cellar install.
///
/// `current_exe` resolves fully, so a Homebrew install lands on the versioned
/// Cellar path rather than the shim in `<prefix>/bin` a user actually invoked.
/// `brew upgrade` deletes that versioned directory outright, so a plist naming
/// it starts failing with a missing binary on every upgrade until the user
/// reruns `daemon install`. The shim is what brew re-links on every upgrade, so
/// naming that instead is what survives one. `exists` is asked rather than
/// assumed so a Cellar layout with no shim (a broken or partial install) is
/// left recording the path that is actually there.
fn stable(resolved: &Path, exists: impl Fn(&Path) -> bool) -> PathBuf {
    cellar_shim(resolved)
        .filter(|shim| exists(shim))
        .unwrap_or_else(|| resolved.to_path_buf())
}

/// The Homebrew prefix shim for a path shaped like a Cellar install:
/// `<prefix>/Cellar/<formula>/<version>/bin/<name>` becomes
/// `<prefix>/bin/<name>`. Anything not shaped exactly like that, including a
/// path that merely mentions "Cellar" somewhere in it, answers `None`.
fn cellar_shim(resolved: &Path) -> Option<PathBuf> {
    let name = resolved.file_name()?;
    let bin = resolved.parent()?;
    let version = bin.parent()?;
    let formula = version.parent()?;
    let cellar = formula.parent()?;

    (bin.file_name()? == "bin" && cellar.file_name()? == "Cellar")
        .then(|| cellar.parent().map(|prefix| prefix.join("bin").join(name)))
        .flatten()
}

fn write_plist(path: &Path, contents: &str) -> Result<(), Failure> {
    path.parent()
        .map_or(Ok(()), fs::create_dir_all)
        .and_then(|()| fs::write(path, contents))
        .map_err(|error| Failure::Error(format!("{}: {error}", path.display())))
}

/// Removes the plist, counting one that was never there as removed.
fn removed(plist: &Path) -> Result<(), Failure> {
    match fs::remove_file(plist) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(Failure::Error(format!("{}: {error}", plist.display()))),
    }
}

/// The failure a `launchctl` call that did not work is, named by what it was.
fn succeeded(ran: &Ran, what: &str) -> Result<(), Failure> {
    if ran.worked() {
        return Ok(());
    }

    Err(Failure::Error(format!(
        "launchctl {what} exited with {}: {}",
        ran.code,
        ran.output.trim()
    )))
}

/// A path as XML text, which is the only escaping a plist of paths needs.
fn escaped(path: &Path) -> String {
    path.to_string_lossy()
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use crate::scratch::Scratch;

    use super::*;

    /// Records every call instead of making it, so no test loads an agent.
    #[derive(Debug, Default)]
    struct Recorder {
        calls: Mutex<Vec<Vec<String>>>,
        /// What successive calls answer with, falling back to a plain success.
        answers: Mutex<Vec<Ran>>,
    }

    impl Recorder {
        fn new() -> Self {
            Self::default()
        }

        /// A recorder whose calls answer with these, oldest first.
        fn answering(answers: Vec<Ran>) -> Self {
            Self {
                answers: Mutex::new(answers),
                ..Self::default()
            }
        }

        fn calls(&self) -> Vec<Vec<String>> {
            self.calls.lock().expect("an unpoisoned recorder").clone()
        }

        /// The verb of each call, which is what it asked launchd to do.
        fn verbs(&self) -> Vec<String> {
            self.calls()
                .into_iter()
                .filter_map(|call| call.first().cloned())
                .collect()
        }
    }

    impl Launchctl for Recorder {
        fn run(&self, arguments: &[&str]) -> Result<Ran, String> {
            self.calls
                .lock()
                .expect("an unpoisoned recorder")
                .push(arguments.iter().map(|part| (*part).to_string()).collect());

            let mut answers = self.answers.lock().expect("an unpoisoned recorder");

            Ok(if answers.is_empty() {
                worked("")
            } else {
                answers.remove(0)
            })
        }
    }

    /// Where the plist goes, under a directory that is not `~/Library`.
    fn plist_in(scratch: &Scratch) -> PathBuf {
        scratch.join("LaunchAgents").join(format!("{LABEL}.plist"))
    }

    fn worked(output: &str) -> Ran {
        Ran {
            code: 0,
            output: output.to_string(),
        }
    }

    fn failed(output: &str) -> Ran {
        Ran {
            code: 113,
            output: output.to_string(),
        }
    }

    fn agent() -> Agent {
        Agent {
            program: PathBuf::from("/opt/homebrew/bin/blubat"),
            config: PathBuf::from("/Users/blubat/.config/blubat/config.toml"),
            state: PathBuf::from("/Users/blubat/.local/state/blubat"),
            home: PathBuf::from("/Users/blubat"),
            out: PathBuf::from("/Users/blubat/.local/state/blubat/daemon.log"),
            error: PathBuf::from("/Users/blubat/.local/state/blubat/daemon.error.log"),
        }
    }

    /// The plist as launchd reads it, kept as one literal so a change to what
    /// blubat asks launchd for has to be made deliberately.
    const PLIST: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>com.paulchiu.blubat</string>
	<key>ProgramArguments</key>
	<array>
		<string>/opt/homebrew/bin/blubat</string>
		<string>--config</string>
		<string>/Users/blubat/.config/blubat/config.toml</string>
		<string>--state-dir</string>
		<string>/Users/blubat/.local/state/blubat</string>
		<string>daemon</string>
		<string>run</string>
	</array>
	<key>EnvironmentVariables</key>
	<dict>
		<key>HOME</key>
		<string>/Users/blubat</string>
		<key>PATH</key>
		<string>/usr/bin:/bin:/usr/sbin:/sbin</string>
	</dict>
	<key>StandardOutPath</key>
	<string>/Users/blubat/.local/state/blubat/daemon.log</string>
	<key>StandardErrorPath</key>
	<string>/Users/blubat/.local/state/blubat/daemon.error.log</string>
	<key>RunAtLoad</key>
	<true/>
	<key>KeepAlive</key>
	<dict>
		<key>SuccessfulExit</key>
		<false/>
	</dict>
	<key>ThrottleInterval</key>
	<integer>30</integer>
</dict>
</plist>
"#;

    #[test]
    fn the_plist_is_the_documented_one() {
        assert_eq!(agent().plist(), PLIST);
    }

    #[test]
    fn the_agent_restarts_only_on_a_bad_exit_and_cannot_spin() {
        let plist = agent().plist();

        assert!(
            plist.contains(
                "<key>KeepAlive</key>\n\t<dict>\n\t\t<key>SuccessfulExit</key>\n\t\t<false/>"
            ),
            "{plist}"
        );
        assert!(
            plist.contains("<key>ThrottleInterval</key>\n\t<integer>30</integer>"),
            "{plist}"
        );
    }

    #[test]
    fn a_path_with_xml_in_it_is_escaped_rather_than_breaking_the_plist() {
        let plist = Agent {
            home: PathBuf::from("/Users/a&b<c>"),
            ..agent()
        }
        .plist();

        assert!(
            plist.contains("<string>/Users/a&amp;b&lt;c&gt;</string>"),
            "{plist}"
        );
    }

    #[test]
    fn installing_writes_the_plist_and_replaces_whatever_was_loaded_before_it() {
        let scratch = Scratch::new();
        let plist = plist_in(&scratch);
        let launchctl = Recorder::new();
        let mut out = Vec::new();

        install(&launchctl, &scratch.paths(), &plist, &mut out).expect("installs");

        assert!(
            fs::read_to_string(&plist)
                .expect("a written plist")
                .contains(LABEL)
        );
        assert_eq!(launchctl.verbs(), ["bootout", "bootstrap"]);
        assert_eq!(
            launchctl.calls()[1],
            [
                "bootstrap".to_string(),
                domain(),
                plist.to_string_lossy().into_owned()
            ]
        );
        assert!(String::from_utf8_lossy(&out).contains(LABEL));
    }

    /// The daemon has to read the config and write the state the blubat that
    /// installed it is using, and launchd hands it nothing to work that out from.
    #[test]
    fn the_agent_names_the_very_files_the_install_was_run_against() {
        let scratch = Scratch::new();
        let paths = scratch.paths();
        let plist = plist_in(&scratch);

        install(&Recorder::new(), &paths, &plist, &mut Vec::new()).expect("installs");

        let written = fs::read_to_string(&plist).expect("a written plist");
        for named in [
            paths.config_file().to_path_buf(),
            paths.state_dir().to_path_buf(),
            paths.log_file(),
            paths.error_log_file(),
        ] {
            assert!(
                written.contains(&format!("<string>{}</string>", named.display())),
                "{} is not in {written}",
                named.display()
            );
        }
    }

    #[test]
    fn a_bootstrap_launchd_was_not_ready_for_is_asked_again() {
        let scratch = Scratch::new();
        let launchctl = Recorder::answering(vec![failed("Bootstrap failed: 5: I/O error")]);

        bootstrap(&launchctl, &plist_in(&scratch), Duration::ZERO)
            .expect("the second attempt found launchd done tearing the old job down");

        assert_eq!(launchctl.verbs(), ["bootstrap", "bootstrap"]);
    }

    #[test]
    fn a_bootstrap_launchd_keeps_refusing_is_an_error_naming_what_it_said() {
        let scratch = Scratch::new();
        let launchctl = Recorder::answering(vec![
            failed("Bootstrap failed: 5: Input/output error"),
            failed("Bootstrap failed: 5: Input/output error"),
            failed("Bootstrap failed: 5: Input/output error"),
        ]);

        let failure = bootstrap(&launchctl, &plist_in(&scratch), Duration::ZERO)
            .expect_err("it was refused every time");

        assert_eq!(failure.code(), 1);
        assert_eq!(launchctl.verbs().len(), ATTEMPTS as usize);
        assert!(
            failure.to_string().contains("Bootstrap failed"),
            "{failure}"
        );
    }

    #[test]
    fn uninstalling_boots_the_agent_out_and_removes_the_plist() {
        let scratch = Scratch::new();
        let plist = plist_in(&scratch);
        let launchctl = Recorder::new();
        install(&launchctl, &scratch.paths(), &plist, &mut Vec::new()).expect("installs");
        let mut out = Vec::new();

        uninstall(&launchctl, &plist, &mut out).expect("uninstalls");

        assert_eq!(launchctl.verbs(), ["bootout", "bootstrap", "bootout"]);
        assert!(!plist.exists());
        assert!(!String::from_utf8_lossy(&out).contains("not loaded"));
    }

    #[test]
    fn uninstalling_what_was_never_installed_says_so_rather_than_failing() {
        let scratch = Scratch::new();
        let launchctl = Recorder::answering(vec![failed("Could not find service")]);
        let mut out = Vec::new();

        uninstall(&launchctl, &plist_in(&scratch), &mut out).expect("nothing is left either way");

        assert!(String::from_utf8_lossy(&out).contains("it was not loaded"));
    }

    #[test]
    fn a_report_names_the_pid_when_the_agent_is_running() {
        let report = worked(
            "com.paulchiu.blubat = {\n\tactive count = 1\n\tstate = running\n\tpid = 4242\n}",
        );

        let lines = describe(Path::new("/Users/blubat/plist"), true, &report);

        assert_eq!(lines[0], "label     com.paulchiu.blubat");
        assert_eq!(lines[1], "plist     /Users/blubat/plist");
        assert_eq!(lines[2], "loaded    yes");
        assert_eq!(lines[3], "running   yes, pid 4242");
    }

    #[test]
    fn an_agent_loaded_but_between_restarts_is_loaded_and_not_running() {
        let report = worked("com.paulchiu.blubat = {\n\tstate = waiting\n}");

        let lines = describe(Path::new("/Users/blubat/plist"), true, &report);

        assert_eq!(lines[2], "loaded    yes");
        assert_eq!(lines[3], "running   no");
    }

    #[test]
    fn nothing_installed_reads_as_nothing_installed_rather_than_a_failure() {
        let scratch = Scratch::new();
        let launchctl = Recorder::answering(vec![failed("Could not find service in domain")]);
        let mut out = Vec::new();

        status(&launchctl, &plist_in(&scratch), &mut out).expect("a report either way");

        let report = String::from_utf8_lossy(&out);
        assert!(report.contains("not installed"), "{report}");
        assert!(report.contains("loaded    no"), "{report}");
        assert!(report.contains("running   no"), "{report}");
    }

    #[test]
    fn a_field_is_read_out_of_the_report_or_absent() {
        let report = "\tstate = running\n\tpid = 4242\n\tpath = \n";

        assert_eq!(field(report, "pid"), Some("4242".to_string()));
        assert_eq!(field(report, "state"), Some("running".to_string()));
        assert_eq!(field(report, "path"), None, "an empty value is no answer");
        assert_eq!(field(report, "program"), None);
    }

    #[test]
    fn the_service_target_is_the_label_in_this_users_gui_domain() {
        assert!(domain().starts_with("gui/"));
        assert_eq!(service(), format!("{}/{LABEL}", domain()));
    }

    #[test]
    fn a_plist_that_is_not_there_is_already_removed() {
        let scratch = Scratch::new();

        assert!(removed(&plist_in(&scratch)).is_ok());
    }

    #[test]
    fn a_cellar_path_maps_to_the_prefix_shim_when_the_shim_exists() {
        let resolved = Path::new("/opt/homebrew/Cellar/blubat/0.7.0/bin/blubat");

        let stable = stable(resolved, |shim| {
            shim == Path::new("/opt/homebrew/bin/blubat")
        });

        assert_eq!(stable, Path::new("/opt/homebrew/bin/blubat"));
    }

    #[test]
    fn a_cellar_path_stays_itself_when_the_shim_does_not_exist() {
        let resolved = Path::new("/opt/homebrew/Cellar/blubat/0.7.0/bin/blubat");

        let stable = stable(resolved, |_| false);

        assert_eq!(stable, resolved);
    }

    #[test]
    fn a_non_cellar_path_is_untouched() {
        let resolved = Path::new("/Users/blubat/.cargo/bin/blubat");

        let stable = stable(resolved, |_| true);

        assert_eq!(stable, resolved);
    }

    #[test]
    fn a_path_that_merely_contains_cellar_but_not_the_full_layout_is_untouched() {
        let resolved = Path::new("/Users/blubat/projects/Cellar/target/blubat");

        let stable = stable(resolved, |_| true);

        assert_eq!(stable, resolved);
    }
}
