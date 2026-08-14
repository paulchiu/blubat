//! The commented guide a new config file is seeded with, and the sections
//! migration appends to a file that predates it.
//!
//! Every scalar table gets a section: a prose heading, then every key behind
//! a `# ` with its built-in default, so stripping that prefix, leaving `## `
//! prose alone, hands back the built-in config. `[[device]]` and `[[hook]]`
//! get fully commented sample blocks instead, header included, since a live
//! empty array of tables fails to parse.

/// The prefix migration looks for on any line to decide a file already
/// carries the guide, or had it and kept the marker on purpose as an opt out
/// after deleting the sections under it by hand. Shorter than the line
/// blubat itself writes, so a future guide version is still recognised.
pub(crate) const MARKER: &str = "## blubat configuration";

/// The header a brand new file opens with: the marker line and the full
/// prose explaining what is below.
const HEADER: &str = "## blubat configuration, guide v1
##
## Every key blubat reads is below, shown behind a single # with its built-in
## default, so this file behaves exactly like an empty one until a line is
## uncommented. Lines starting with ## are prose and stay comments.
## `blubat config validate` checks the result; r on the dashboard reloads it.
";

/// The shorter header migration prepends ahead of a file that already has
/// content of its own; the guide sections that follow speak for themselves.
pub(crate) const MIGRATED: &str = "## blubat configuration, guide v1
## The commented sections appended below show every key blubat reads with its
## built-in default. `blubat config validate` checks whatever you uncomment.
";

const POLL: &str = "## How often blubat reads, and how long a silence lasts before a device is
## stale. Durations are bare seconds or carry an s, m or h suffix.
[poll]
# foreground_interval = \"30s\"
# daemon_interval = \"120s\"
# profiler_interval = \"5m\"
# profiler_timeout = \"10s\"
# stale_after = \"10m\"
";

const NOTIFICATIONS: &str =
    "## Which events raise a desktop banner. connect covers connect and disconnect
## together, and sound is a macOS sound name as osascript knows them.
[notifications]
# low = true
# critical = true
# charged = true
# connect = false
# stale = true
# sound = \"Glass\"
";

const DEFAULTS: &str = "## Thresholds for every device without a [[device]] block of its own. An
## unset key falls through to what the device advertises, then to the
## built-in numbers shown here.
[defaults]
# low = 20
# critical = 10
# high = 100
# rearm_margin = 1
";

const THEME: &str = "## Colours as #rrggbb hex, each defaulting to the scheme's own colour when
## unset, and the glyph shown while a device charges.
[theme]
# scheme = \"dark\"
# accent = \"#39c5cf\"
# critical = \"#f47067\"
# low = \"#c69026\"
# ok = \"#57ab5a\"
# charging_glyph = \"+\"
";

const DASHBOARD: &str = "## What the dashboard hides and how it sorts: level, name or last_seen.
## h and i maintain hidden and hide_inactive from the dashboard itself.
## inactive_after moves a connected device with no reading this recent into
## the inactive section too.
[dashboard]
# hidden = []
# sort = \"level\"
# hide_inactive = false
# inactive_after = \"60m\"
";

/// Every scalar table's guide section, paired with the key migration checks
/// the parsed document for.
pub(crate) const SCALAR_SECTIONS: &[(&str, &str)] = &[
    ("poll", POLL),
    ("notifications", NOTIFICATIONS),
    ("defaults", DEFAULTS),
    ("theme", THEME),
    ("dashboard", DASHBOARD),
];

/// Fully commented, header included: a live empty `[[device]]` fails to
/// parse, since `match` is required.
pub(crate) const DEVICE_SAMPLE: &str =
    "## Per device overrides, matched case insensitively against the name and the
## address. The first block a device matches wins.
# [[device]]
# match = \"trackpad\"
# low = 15
";

/// Fully commented for the same reason [`DEVICE_SAMPLE`] is.
pub(crate) const HOOK_SAMPLE: &str =
    "## Commands run on events (low_battery, critical_battery, charged, connected,
## disconnected, stale) with the BLUBAT_* variables set. debounce is a window
## or \"once\", and match filters devices the way --device does.
# [[hook]]
# event = \"low_battery\"
# command = \"~/.config/blubat/hooks/nag.sh\"
# debounce = \"30m\"
";

/// Appends `section` to `text`, separated from whatever is already there by
/// exactly one blank line, or by nothing when `text` starts empty.
///
/// Shared between [`full`] and the migration that composes around a file
/// that already has some of these sections, so both settle a missing
/// trailing newline the same way rather than each restating it.
pub(crate) fn append(text: &mut String, section: &str) {
    if !text.is_empty() {
        if !text.ends_with('\n') {
            text.push('\n');
        }
        text.push('\n');
    }
    text.push_str(section);
}

/// The full template a config file that does not exist yet is seeded with.
pub(crate) fn full() -> String {
    let mut text = HEADER.to_string();

    for (_, section) in SCALAR_SECTIONS.iter().copied() {
        append(&mut text, section);
    }
    append(&mut text, DEVICE_SAMPLE);
    append(&mut text, HOOK_SAMPLE);

    text
}
