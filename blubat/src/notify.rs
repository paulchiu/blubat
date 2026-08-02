//! Desktop banners: the primary path, the fallback, and the borrowed identity
//! both of them deliver under.
//!
//! An unbundled binary has no notification identity of its own, so macOS
//! attributes a banner to whichever app blubat borrows: the notification centre
//! path takes Terminal's bundle identifier, and `osascript` is attributed to
//! Script Editor. That is why a send can succeed and show nothing, and why
//! [`run`] exists to name the identity that was used.

// TODO: remove once the poll loop calls `announce`; only tests reach it so far.
#![allow(dead_code)]

use std::fmt;
use std::io::{self, Write};
use std::process::{Command, Stdio};
use std::sync::OnceLock;

use blubat_core::{Config, Event, Notifications, Paths, Raised};

use crate::Failure;

/// The app whose notification identity the notification centre path borrows.
const BORROWED_APP: &str = "Terminal";

/// The identity macOS attributes an `osascript` banner to.
const OSASCRIPT_IDENTITY: &str = "com.apple.ScriptEditor2";

/// What the notifier is asked to put on screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Banner {
    pub title: String,
    pub body: String,
    /// A macOS sound name, absent for a silent banner.
    pub sound: Option<String>,
}

impl Banner {
    /// A banner with the configured sound, silent when that setting is blank.
    pub fn new(title: impl Into<String>, body: impl Into<String>, sound: &str) -> Self {
        Self {
            title: title.into(),
            body: body.into(),
            sound: (!sound.trim().is_empty()).then(|| sound.trim().to_string()),
        }
    }

    /// The banner one raised event puts on screen.
    pub fn of(raised: &Raised, sound: &str) -> Self {
        Self::new(raised.device.clone(), describe(raised), sound)
    }
}

/// Which path delivered a banner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
    /// The notification centre, through `mac-notification-sys`.
    Native,
    /// The `osascript` fallback, taken when the native path errors.
    Osascript,
}

/// A delivered banner and the app identity it went out under.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Delivery {
    pub route: Route,
    pub identity: String,
}

impl fmt::Display for Delivery {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let path = match self.route {
            Route::Native => "the notification centre",
            Route::Osascript => "osascript",
        };

        write!(f, "delivered by {path} as {}", self.identity)
    }
}

/// Somewhere a banner can be posted, which a test fills with a recorder.
pub trait Notifier: Send + Sync {
    /// Posts a banner, reporting the path and identity it went out under.
    fn post(&self, banner: &Banner) -> Result<Delivery, String>;
}

/// The macOS notifier: the notification centre, falling back to `osascript`.
#[derive(Clone, Copy, Debug, Default)]
pub struct Desktop;

impl Notifier for Desktop {
    fn post(&self, banner: &Banner) -> Result<Delivery, String> {
        deliver(banner, native, osascript)
    }
}

/// Posts the banner one event asks for, or nothing when the config silences it.
pub fn announce(
    raised: &Raised,
    notifications: &Notifications,
    notifier: &dyn Notifier,
) -> Option<Result<Delivery, String>> {
    notifications
        .enabled(raised.event)
        .then(|| notifier.post(&Banner::of(raised, &notifications.sound)))
}

/// `blubat notify-test`: post one banner and name the identity it went out as.
pub fn run(paths: &Paths) -> Result<(), Failure> {
    let config = Config::load(paths.config_file())?;

    report(&Desktop, &config.notifications.sound, &mut io::stdout())
}

/// The reporting half, which a test drives with a notifier that posts nothing.
fn report(notifier: &dyn Notifier, sound: &str, out: &mut impl Write) -> Result<(), Failure> {
    let banner = Banner::new("blubat", "Test notification from blubat.", sound);
    let delivery = notifier.post(&banner).map_err(Failure::Error)?;

    writeln!(out, "test banner {delivery}")?;
    writeln!(
        out,
        "If no banner appeared, that identity is muted: check Focus and the \
         notification settings for it."
    )?;

    Ok(())
}

/// The sentence a raised event says.
fn describe(raised: &Raised) -> String {
    let at = raised
        .level
        .map_or(String::new(), |level| format!(" at {level}%"));

    match raised.event {
        Event::LowBattery => format!("Battery low{at}"),
        Event::CriticalBattery => format!("Battery critically low{at}"),
        Event::Charged => format!("Charged{at}, safe to unplug"),
        Event::Connected => format!("Connected{at}"),
        Event::Disconnected => format!("Disconnected{at}"),
        Event::Stale => String::from("No battery reading recently"),
    }
}

/// Tries the primary path, falling back, and keeps both problems if both fail.
///
/// Takes both paths as arguments so the choice between them is testable without
/// a notification centre to post to.
fn deliver(
    banner: &Banner,
    native: impl Fn(&Banner) -> Result<String, String>,
    fallback: impl Fn(&Banner) -> Result<String, String>,
) -> Result<Delivery, String> {
    native(banner)
        .map(|identity| Delivery {
            route: Route::Native,
            identity,
        })
        .or_else(|primary| {
            fallback(banner)
                .map(|identity| Delivery {
                    route: Route::Osascript,
                    identity,
                })
                .map_err(|fallback| {
                    format!("notification centre: {primary}; osascript: {fallback}")
                })
        })
}

/// Posts through the notification centre under the borrowed identity.
fn native(banner: &Banner) -> Result<String, String> {
    let identity = identity()?;
    let mut options = mac_notification_sys::Notification::new();
    options.maybe_sound(banner.sound.as_deref());

    mac_notification_sys::send_notification(&banner.title, None, &banner.body, Some(&options))
        .map(|_| identity)
        .map_err(|error| error.to_string())
}

/// The bundle identity blubat borrows, resolved and registered once per process.
///
/// `set_application` takes the first call and rejects every later one, so the
/// answer is memoised: the second banner of a session must not fail on that.
fn identity() -> Result<String, String> {
    static IDENTITY: OnceLock<Result<String, String>> = OnceLock::new();

    IDENTITY
        .get_or_init(|| {
            let bundle = mac_notification_sys::get_bundle_identifier_or_default(BORROWED_APP);

            mac_notification_sys::set_application(&bundle)
                .map(|()| bundle)
                .map_err(|error| error.to_string())
        })
        .clone()
}

/// Posts through `osascript`, whose output goes nowhere the dashboard draws.
fn osascript(banner: &Banner) -> Result<String, String> {
    Command::new("osascript")
        .arg("-e")
        .arg(script(banner))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|error| error.to_string())
        .and_then(|status| {
            status
                .success()
                .then(|| OSASCRIPT_IDENTITY.to_string())
                .ok_or_else(|| format!("exited with {status}"))
        })
}

/// The AppleScript one banner compiles to.
fn script(banner: &Banner) -> String {
    let sound = banner.sound.as_deref().map_or(String::new(), |sound| {
        format!(" sound name {}", literal(sound))
    });

    format!(
        "display notification {} with title {}{sound}",
        literal(&banner.body),
        literal(&banner.title)
    )
}

/// An AppleScript string literal, with the two characters that can escape one.
fn literal(text: &str) -> String {
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

/// A notifier that records what it was asked to post, for the modules that
/// wire the real one up.
#[cfg(test)]
pub mod fake {
    use std::sync::Mutex;

    use super::{Banner, Delivery, Notifier, Route};

    /// Records every banner instead of posting it.
    #[derive(Debug, Default)]
    pub struct Recorder {
        posted: Mutex<Vec<Banner>>,
        /// The problem every post fails with, absent when they all succeed.
        problem: Option<String>,
    }

    impl Recorder {
        pub fn new() -> Self {
            Self::default()
        }

        /// A notifier nothing gets through, which is the logged path.
        pub fn failing(problem: &str) -> Self {
            Self {
                problem: Some(problem.to_string()),
                ..Self::default()
            }
        }

        /// The banners posted so far, in order.
        pub fn posted(&self) -> Vec<Banner> {
            self.posted.lock().expect("an unpoisoned recorder").clone()
        }
    }

    impl Notifier for Recorder {
        fn post(&self, banner: &Banner) -> Result<Delivery, String> {
            self.posted
                .lock()
                .expect("an unpoisoned recorder")
                .push(banner.clone());

            match &self.problem {
                Some(problem) => Err(problem.clone()),
                None => Ok(Delivery {
                    route: Route::Native,
                    identity: String::from("com.example.recorder"),
                }),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use blubat_core::{Address, ChargeState, Source, Timestamp};

    use super::fake::Recorder;
    use super::*;

    fn raised(event: Event, level: Option<u8>) -> Raised {
        Raised {
            event,
            device: String::from("Paul\u{2019}s Magic Trackpad"),
            address: Address::parse("30-82-16-f2-24-90").expect("valid address"),
            level,
            previous: Some(30),
            charge: ChargeState::Discharging,
            source: Source::IoKit,
            threshold: Some(20),
            cycle: 0,
            at: Timestamp::from_unix(1_785_643_199),
        }
    }

    fn works(identity: &str) -> impl Fn(&Banner) -> Result<String, String> {
        let identity = identity.to_string();

        move |_| Ok(identity.clone())
    }

    fn breaks(problem: &str) -> impl Fn(&Banner) -> Result<String, String> {
        let problem = problem.to_string();

        move |_| Err(problem.clone())
    }

    #[test]
    fn a_blank_sound_setting_is_a_silent_banner() {
        assert_eq!(
            Banner::new("t", "b", "Glass").sound.as_deref(),
            Some("Glass")
        );
        assert_eq!(Banner::new("t", "b", "  ").sound, None);
        assert_eq!(Banner::new("t", "b", "").sound, None);
    }

    #[test]
    fn every_event_says_something_naming_the_device() {
        for event in Event::ALL {
            let banner = Banner::of(&raised(event, Some(18)), "Glass");

            assert_eq!(banner.title, "Paul\u{2019}s Magic Trackpad");
            assert!(!banner.body.is_empty(), "{event} says nothing");
        }

        assert_eq!(
            Banner::of(&raised(Event::LowBattery, Some(18)), "Glass").body,
            "Battery low at 18%"
        );
        assert_eq!(
            Banner::of(&raised(Event::Charged, Some(100)), "Glass").body,
            "Charged at 100%, safe to unplug"
        );
        assert_eq!(
            Banner::of(&raised(Event::Disconnected, None), "Glass").body,
            "Disconnected",
            "a device with no live level says nothing about one"
        );
    }

    #[test]
    fn the_native_path_is_taken_when_it_works() {
        let delivery = deliver(
            &Banner::new("blubat", "body", "Glass"),
            works("com.apple.Terminal"),
            breaks("never reached"),
        )
        .expect("delivered");

        assert_eq!(delivery.route, Route::Native);
        assert_eq!(delivery.identity, "com.apple.Terminal");
        assert_eq!(
            delivery.to_string(),
            "delivered by the notification centre as com.apple.Terminal"
        );
    }

    #[test]
    fn the_fallback_is_taken_when_the_native_path_errors() {
        let delivery = deliver(
            &Banner::new("blubat", "body", "Glass"),
            breaks("no bundle"),
            works(OSASCRIPT_IDENTITY),
        )
        .expect("delivered");

        assert_eq!(delivery.route, Route::Osascript);
        assert_eq!(
            delivery.to_string(),
            format!("delivered by osascript as {OSASCRIPT_IDENTITY}")
        );
    }

    #[test]
    fn both_paths_failing_reports_both_problems() {
        let problem = deliver(
            &Banner::new("blubat", "body", "Glass"),
            breaks("no bundle"),
            breaks("no osascript"),
        )
        .expect_err("nothing was delivered");

        assert!(problem.contains("no bundle"), "{problem}");
        assert!(problem.contains("no osascript"), "{problem}");
    }

    #[test]
    fn a_script_survives_the_quotes_a_device_name_may_carry() {
        assert_eq!(literal("plain"), "\"plain\"");
        assert_eq!(
            literal("a \"quoted\" back\\slash"),
            "\"a \\\"quoted\\\" back\\\\slash\""
        );
        assert_eq!(
            script(&Banner::new("blubat", "Ed\"s \"Mouse\" is low", "Glass")),
            "display notification \"Ed\\\"s \\\"Mouse\\\" is low\" with title \"blubat\" \
             sound name \"Glass\""
        );
    }

    #[test]
    fn a_silent_banner_asks_for_no_sound_at_all() {
        let quiet = script(&Banner::new("blubat", "body", ""));

        assert!(!quiet.contains("sound name"), "{quiet}");
    }

    #[test]
    fn an_event_the_config_silences_posts_nothing() {
        let recorder = Recorder::new();
        let notifications = Notifications::default();

        assert!(
            announce(
                &raised(Event::Connected, Some(80)),
                &notifications,
                &recorder
            )
            .is_none(),
            "link events are off by default"
        );
        assert!(recorder.posted().is_empty());
    }

    #[test]
    fn an_enabled_event_posts_the_banner_with_the_configured_sound() {
        let recorder = Recorder::new();
        let notifications = Notifications {
            sound: String::from("Ping"),
            ..Notifications::default()
        };

        let delivery = announce(
            &raised(Event::LowBattery, Some(18)),
            &notifications,
            &recorder,
        )
        .expect("the event is enabled")
        .expect("the recorder accepts it");

        assert_eq!(delivery.route, Route::Native);
        assert_eq!(recorder.posted().len(), 1);
        assert_eq!(recorder.posted()[0].sound.as_deref(), Some("Ping"));
        assert_eq!(recorder.posted()[0].body, "Battery low at 18%");
    }

    #[test]
    fn the_test_banner_names_the_identity_and_what_silence_means() {
        let mut printed = Vec::new();

        report(&Recorder::new(), "Glass", &mut printed).expect("delivered");

        let printed = String::from_utf8(printed).expect("utf8 output");
        assert!(printed.contains("com.example.recorder"), "{printed}");
        assert!(printed.contains("muted"), "{printed}");
    }

    #[test]
    fn a_test_banner_that_cannot_be_delivered_is_an_error_exit() {
        let mut printed = Vec::new();

        let failure = report(
            &Recorder::failing("no notification centre"),
            "Glass",
            &mut printed,
        )
        .expect_err("nothing was delivered");

        assert_eq!(failure.code(), 1);
        assert!(
            failure.to_string().contains("no notification centre"),
            "{failure}"
        );
        assert!(printed.is_empty(), "nothing to report");
    }
}
