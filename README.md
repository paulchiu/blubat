# blubat

A Bluetooth battery monitor for macOS: a single binary that reports the battery
level of every Bluetooth peripheral macOS already knows about, as a one-shot CLI
reading, as JSON for scripts and status bars, and as a live TUI that notifies
and runs hooks on threshold crossings.

macOS has no single source that lists every device battery. Apple HID
peripherals such as the Magic Trackpad report through IOKit and are absent from
`system_profiler`; devices such as the MX Keys and AirPods report through
`system_profiler` and never appear in the HID class. blubat reads both and
merges them, keeping the source and freshness of each reading visible.

The two sources run on their own tiers: the IOKit read on every tick, and the
much more expensive `system_profiler` call on a slower one with its result
cached in between. That call has a timeout (`profiler_timeout`), and a call
that times out or comes back unreadable never fails the poll: blubat keeps the
last good reading, marks it degraded, and says so on stderr or on the dashboard,
so the levels stay visible while they age.

An Apple HID peripheral connecting or disconnecting cuts both tiers' waits
short, through IOKit's own matched and terminated notifications, so a
reconnected trackpad is read at once rather than at the next tick. Devices that
only `system_profiler` sees, AirPods among them, publish no such notification
and are still picked up on the ordinary tick.

## Status

Pre-release. Milestone M0 is complete: both data sources, the merge, and the
one-shot CLI (`list`, `status`, `wait`) that reaches parity with the
`trackpad-battery` shell script blubat takes its inspiration from. M1 is
complete: bare `blubat` opens a live dashboard listing every device with its
level, charge state, trend and freshness, sorted, filtered and narrowed from
the keyboard. M2 is underway: the config file, the threshold event engine, the
desktop notifications and the hooks that run alongside them, all live in the
dashboard and reloadable with `r`. The device detail view and the background
daemon come after it.

blubat reads an optional configuration file and never writes one. The only
files it creates are its own state, under `~/.local/state/blubat/`: the event
engine's `state.toml`, and the `watches/` directory `blubat wait` may drop into.

## Usage

```
blubat                              # the live dashboard: ? lists every key
blubat list [--json] [--all]        # every device that reports a battery
blubat status [--device <match>]    # one device, human readable
              [--json | --number]   # machine readable variants
blubat wait --device <match>        # poll until the level is reached, then
            --until <level>         # post a desktop notification
            [--interval 60s] [--timeout <duration>]
blubat config path                  # print the resolved config path
blubat config edit                  # open it in $EDITOR
blubat config validate              # parse it and report what is wrong
blubat notify-test                  # post a test banner, name the identity it
                                    # was delivered under

  --config <path>                   # read configuration from here instead
```

`--device` takes a substring, matched case insensitively against both the
device name and its Bluetooth address, so `trackpad`, `Magic`, `30-82-16` and
`30:82:16` all select the same device. `status` reports the one device with a
battery that the arguments identify; if more than one qualifies, with or
without `--device`, it names them and asks you to narrow the substring rather
than picking one.

```
$ blubat list
NAME                   ADDRESS            LEVEL  STATE       SOURCE
MX Keys M Mac          de-df-38-f0-46-9b  100%   unknown     system_profiler
Paul's Magic Trackpad  30-82-16-f2-24-90  83%    on battery  iokit

$ blubat status --device trackpad
Paul's Magic Trackpad  83%  on battery

$ blubat wait --device trackpad --until 100 --interval 5m
```

## Dashboard

Bare `blubat` opens the dashboard, or prints the command help and exits 0 when
there is no terminal to draw one on, so a piped `blubat` still answers. The trend
column is a six cell sparkline over the levels read this run, with dots for a
device nothing has been read from yet. Connected devices come first; disconnected
ones sit under a dimmed `inactive` heading with their own count, keeping their
last seen level out of the critical summary. A device no source reports a level
for is listed as `unreported` rather than dropped, and a narrow terminal gives
up columns from the right rather than breaking the table.

```
q      quit                  s  cycle the order: level, name, last seen
j/k    move the selection    /  filter on name or address, esc clears it
enter  detail view, later    h  hide the selected device, H show hidden again
r      reload the config     ?  the full keymap
```

`q` and ctrl+c both leave the dashboard; the keymap overlay takes the keyboard
while it is open, so `?` closes it before anything else responds again.

`r` re-reads `~/.config/blubat/config.toml` in place: thresholds, notification
toggles, hooks, the colour scheme and the charging glyph all take the new
values without a restart. `[poll]` is the exception, since the poller is
already running on the intervals it was started with, so a changed
`foreground_interval` or `profiler_interval` waits for a restart. A file that
will not parse is reported on a line of its own and changes nothing, so the
config that was working a moment ago keeps working and the dashboard never
exits over a typo. The same line carries a hook that went wrong, which is why
hook output goes nowhere near stdout.

A row is painted red below the same `critical` threshold the events are raised
by, so the count on the status line and the banners agree by construction: a
device configured `critical = 40` is red and counted at 39%, which is also the
level that raises `critical_battery` for it.

Hiding lasts for the session: nothing is written anywhere. The charging mark is
ascii by default and becomes the Nerd Font bolt when the environment says a
Nerd Font is in use, which is a guess: set `BLUBAT_NERD_FONT=1` or `=0` to
settle it either way, or `charging_glyph` in `[theme]` to settle it for good.

A disconnected device keeps the level macOS last saw, which carries no
timestamp and can be arbitrarily old. It is labelled `last seen` wherever it is
shown, and `wait` ignores it rather than completing on a stale number.
Durations are written as bare seconds or with an `s`, `m` or `h` suffix.

## Scripting

These are the compatibility surface. They are treated as a contract and will
not change shape within a major version.

### Exit codes

| Code | Meaning |
| ---- | ------- |
| 0 | A usable reading was printed. |
| 1 | An error, including a usage error and a `wait` that hit its timeout. |
| 3 | No matching device has a battery. |

Warnings and errors go to stderr, so nothing contaminates a value read from
stdout. `blubat status --json --device X > level.json` never mixes a diagnostic
into the file, though it writes nothing at all when the device is gone, so
branch on the exit code before reading it back.

### `--number`

`blubat status --number` prints the percentage as a bare integer and nothing
else, for direct substitution:

```sh
level=$(blubat status --device trackpad --number) || exit
[ "$level" -ge 30 ] || echo "charge the trackpad"
```

For a multi-battery device such as AirPods, the number is the lowest present
sub-level, because a device is as charged as its emptiest part.

A bare number has nowhere to carry the `last seen` label, so for a disconnected
device it is the level macOS last saw, which can be arbitrarily old. A script
that needs a fresh reading should use `--json` and check `connected`.

### `--json`

`blubat status --json` emits one object; `blubat list --json` emits an array of
those same objects, in the order the table shows them, and stays an array (`[]`)
when nothing is paired.

Every object names the schema it is written in. `schema_version`, `level`,
`charge`, `source`, `connected` and `read_at` are always written, as `null`
where there is nothing to report, so a script never has to tell an absent value
from a missing key. The descriptive keys (`kind`, `transport`, and each entry
of `levels`) are omitted when they have no value.

```json
{
  "schema_version": 1,
  "level": 83,
  "address": "30-82-16-f2-24-90",
  "name": "Paul's Magic Trackpad",
  "kind": "Magic Trackpad",
  "transport": "Bluetooth",
  "levels": { "main": 83 },
  "charge": "discharging",
  "source": "iokit",
  "connected": true,
  "read_at": "2026-08-01T22:20:31Z"
}
```

| Key | Type | Notes |
| --- | ---- | ----- |
| `schema_version` | integer | `1`. Raised only by a change that breaks a reader, never within a patch release. |
| `level` | integer or null | The one number that stands for the device: the lowest sub-level present, because a device is as charged as its emptiest part. `null` when no source reported a battery. |
| `address` | string | Lowercase hex octets joined by hyphens. The stable identity of a device. |
| `name` | string | |
| `kind` | string, optional | Device category as `system_profiler` names it. |
| `transport` | string, optional | Link the IOKit node reports. |
| `levels` | object | Any of `main`, `left`, `right`, `case`, each a percentage. Absent keys are omitted, so a single-battery device is `{ "main": 83 }` and AirPods are `{ "left": …, "right": …, "case": … }`. |
| `charge` | string | `charging`, `discharging` or `unknown`. Only Apple HID devices report it. The human output prints `discharging` as `on battery`, as the POC does. |
| `source` | string | `iokit` or `system_profiler`. |
| `connected` | boolean | `false` means `levels` is last seen data of unknown age. |
| `read_at` | string | RFC 3339 in UTC, whole seconds. When blubat took the reading, not when the device reported it. |

```sh
blubat list --json | jq -r '.[] | select(.connected) | "\(.name) \(.level)"'
```

## Configuration

TOML at `~/.config/blubat/config.toml`, resolved with the XDG strategy. The
file is optional: blubat runs on built-in defaults, and it never writes one for
you. Machine state (the event engine's armed and fired flags, the one-shot
watches) lives apart from it under `~/.local/state/blubat/`.

Parsing is strict. An unknown key, an unknown event name or a duration that
does not parse is an error naming the line it is on, because a typo that
silently does nothing is worse than one that says so.

```toml
[poll]
foreground_interval = "30s"   # tick while the dashboard or a command runs
daemon_interval     = "120s"  # tick under launchd
profiler_interval   = "5m"    # slow tier, cached in between
profiler_timeout    = "10s"   # ceiling on one system_profiler call
stale_after         = "10m"   # no reading for this long raises `stale`

[notifications]
low      = true
critical = true
charged  = true               # "safe to unplug"
connect  = false              # connect and disconnect are noisy by default
stale    = true
sound    = "Glass"

[defaults]
low          = 20
critical     = 10
high         = 100            # the level that raises `charged`
rearm_margin = 1              # recovery required before an event re-arms

[theme]
scheme   = "dark"             # dark, light or mono
accent   = "#39c5cf"          # per colour overrides on top of the scheme
critical = "#f47067"
low      = "#c69026"
ok       = "#57ab5a"
charging_glyph = "+"          # overrides the Nerd Font guess either way

[dashboard]
hidden = ["MX Master"]        # read but not yet acted on, see below
sort   = "level"              # level, name or last_seen

# Per device overrides. `match` is the same case insensitive substring
# `--device` takes, tested against the name and the address. The first
# block a device matches is the one that applies.
[[device]]
match        = "Soundcore"
low          = 25             # dies fast below 25, warn earlier
high         = 90             # optimised charging never reports 100
rearm_margin = 5              # reports in coarse steps, needs more slack

# Hooks run a shell command with BLUBAT_* set in the environment.
[[hook]]
event    = "low_battery"      # low_battery, critical_battery, charged,
command  = "~/bin/nag"        # connected, disconnected or stale
debounce = "30m"              # a window, or "once" per re-arm cycle

[[hook]]
event   = "disconnected"
match   = "AirPods"           # optional per hook device filter
command = "~/bin/pause-music"
timeout = "10s"
```

Thresholds resolve most specific first: the first `[[device]]` block the
device matches, then `[defaults]`, then what the device's own IOKit node
advertises, then the built-in 20, 10, 100 and 1.

`[poll] foreground_interval` sets the dashboard's tick. Left unwritten, the
dashboard reads every 5s instead: it is on screen and being read as it changes,
and the fast tier is a single digit millisecond IOKit call.

`[dashboard]` parses and validates but nothing acts on it yet. It lands with the
persistent hide, which is the one write blubat will ever make to the file.

`blubat config validate` exits 0 when the file is usable or absent and 1 when
it is not, so it fits a dotfiles check. A `[[device]]` block matching nothing
currently visible is a warning rather than a failure, since the device may
simply be switched off.

## Notifications and hooks

Both subscribe to the same six events (`low_battery`, `critical_battery`,
`charged`, `connected`, `disconnected`, `stale`), and both fire on a threshold
crossing rather than on a level sitting past one. A device already below its
low threshold when blubat starts is recorded rather than announced, and re-arms
only once it has recovered past the threshold by `rearm_margin`, so a level
oscillating around the boundary raises one event instead of forty. That armed
and fired state, and each hook's debounce clock, live in
`~/.local/state/blubat/state.toml` and survive a restart.

`charged` additionally needs a device that is not reporting itself as draining,
since it says a charge has finished: an earbud put back in its case lifts the
device's level without being anything to announce. Devices that report no
charge state at all, which is every one `system_profiler` sees, raise it on the
level alone.

`[notifications]` switches the banners per event and nothing else: an event a
toggle silences still runs its hooks, since the two are separate subscribers.

blubat is an unbundled binary, so it has no notification identity of its own and
macOS attributes its banners to another app: Terminal on the primary path,
Script Editor on the `osascript` fallback taken when that path errors. A muted
identity, or a Focus mode, swallows the banner while the send still reports
success, which is what `blubat notify-test` is for:

```
$ blubat notify-test
test banner delivered by the notification centre as com.apple.Terminal
If no banner appeared, that identity is muted: check Focus and the notification
settings for it.
```

A hook runs under `sh -c`, on its own thread, with its output discarded and
these variables in its environment. Each is always set, and empty where the
reading has no answer. A hook still running when blubat exits is not killed:
the timeout is blubat's to enforce and lasts only as long as blubat does.

| Variable | Value |
| --- | --- |
| `BLUBAT_DEVICE` | Device name, as the dashboard shows it |
| `BLUBAT_DEVICE_ADDRESS` | Bluetooth address, hyphenated |
| `BLUBAT_EVENT` | `low_battery`, `critical_battery`, `charged`, `connected`, `disconnected` or `stale` |
| `BLUBAT_LEVEL` | Level in percent that raised the event |
| `BLUBAT_PREVIOUS_LEVEL` | Last level seen before it |
| `BLUBAT_CHARGING` | `1`, `0`, or `unknown` where no source knows |
| `BLUBAT_SOURCE` | `iokit` or `system_profiler` |
| `BLUBAT_THRESHOLD` | The threshold crossed, empty for the events that watch no level |

```sh
#!/bin/sh
# ~/bin/nag, run on [[hook]] event = "low_battery"
test "$BLUBAT_LEVEL" -lt 10 && say "$BLUBAT_DEVICE needs charging"
```

A hook that outlives its `timeout` is killed, and one that hangs, cannot start
or exits non-zero is reported rather than retried: on the dashboard's own line,
since anything printed underneath would land on top of the frame it just drew.
Nothing a hook does can hold up a poll, a keystroke or another hook.

## Layout

- `blubat-core`: the device model, both data sources, the poller, the event
  engine and the config types. Depends on no terminal library, so a frontend
  other than the TUI stays buildable.
- `blubat`: the binary, holding the CLI, the TUI, the notifier and the hook
  runner over that core. It owns argument parsing, rendering and exit codes,
  and nothing else. The dashboard is one loop over one channel: keypresses,
  readings and finished hooks arrive as events, a pure `update` folds each into
  the next state, and a pure `render` draws it. Everything a reading sets off
  beyond a redraw (stepping the engine, saving its state, posting a banner,
  starting a hook) sits in one effects layer the loop calls, never in `update`.

## Development

Recipes live in the `justfile`:

```
just build
just test
just lint
just fmt
just ci
```

## Requirements

macOS only. Rust 1.95 or newer. No entitlements, no sudo, no helper app bundle.

## License

MIT, see [LICENSE](LICENSE).
