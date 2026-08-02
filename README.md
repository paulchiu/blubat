# blubat

A Bluetooth battery monitor for macOS: a single binary that reports the battery
level of every Bluetooth peripheral macOS already knows about, as a one-shot CLI
reading, as JSON for scripts and status bars, and (later) as a live TUI.

macOS has no single source that lists every device battery. Apple HID
peripherals such as the Magic Trackpad report through IOKit and are absent from
`system_profiler`; devices such as the MX Keys and AirPods report through
`system_profiler` and never appear in the HID class. blubat reads both and
merges them, keeping the source and freshness of each reading visible.

## Status

Pre-release. Milestone M0 is complete: both data sources, the merge, and the
one-shot CLI (`list`, `status`, `wait`) that reaches parity with the
`trackpad-battery` shell script blubat takes its inspiration from. M1 is
underway: bare `blubat` opens a live dashboard listing every device with its
level, charge state, trend and freshness, sorted, filtered and narrowed from
the keyboard. The device detail view, threshold notifications and the
background daemon come after it.

blubat reads an optional configuration file and never writes one. The only
files it creates are its own state: `blubat wait` may create
`~/.local/state/blubat/watches/`.

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
?      the full keymap
```

`q` and ctrl+c both leave the dashboard; the keymap overlay takes the keyboard
while it is open, so `?` closes it before anything else responds again.

Hiding lasts for the session: nothing is written anywhere. The charging mark is
ascii by default and becomes the Nerd Font bolt when the environment says a
Nerd Font is in use, which is a guess: set `BLUBAT_NERD_FONT=1` or `=0` to
settle it either way.

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
when nothing is paired. Keys with no value are omitted rather than emitted as
`null`.

```json
{
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
| `address` | string | Lowercase hex octets joined by hyphens. The stable identity of a device. |
| `name` | string | |
| `kind` | string, optional | Device category as `system_profiler` names it. |
| `transport` | string, optional | Link the IOKit node reports. |
| `levels` | object | Any of `main`, `left`, `right`, `case`, each a percentage. Absent keys are omitted. |
| `charge` | string | `charging`, `discharging` or `unknown`. Only Apple HID devices report it. The human output prints `discharging` as `on battery`, as the POC does. |
| `source` | string | `iokit` or `system_profiler`. |
| `connected` | boolean | `false` means `levels` is last seen data of unknown age. |
| `read_at` | string | RFC 3339 in UTC, whole seconds. When blubat took the reading, not when the device reported it. |

```sh
blubat list --json | jq -r '.[] | select(.connected) | "\(.name) \(.levels.main)"'
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

[dashboard]
hidden = ["MX Master"]
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

`blubat config validate` exits 0 when the file is usable or absent and 1 when
it is not, so it fits a dotfiles check. A `[[device]]` block matching nothing
currently visible is a warning rather than a failure, since the device may
simply be switched off.

## Notifications and hooks

Both subscribe to the same events, and both fire on a threshold crossing rather
than on a level sitting past one.

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
reading has no answer.

| Variable | Value |
| --- | --- |
| `BLUBAT_DEVICE` | Device name, as the dashboard shows it |
| `BLUBAT_DEVICE_ADDRESS` | Bluetooth address, hyphenated |
| `BLUBAT_EVENT` | `low_battery`, `critical_battery`, `charged`, `connected`, `disconnected` or `stale` |
| `BLUBAT_LEVEL` | Level in percent that raised the event |
| `BLUBAT_PREVIOUS_LEVEL` | Last level seen before it |
| `BLUBAT_CHARGING` | `true`, `false`, or empty where no source knows |
| `BLUBAT_SOURCE` | `iokit` or `system_profiler` |
| `BLUBAT_THRESHOLD` | The threshold crossed, empty for the events that watch no level |

```sh
#!/bin/sh
# ~/bin/nag, run on [[hook]] event = "low_battery"
test "$BLUBAT_LEVEL" -lt 10 && say "$BLUBAT_DEVICE needs charging"
```

A hook that outlives its `timeout` is killed, and one that hangs, cannot start
or exits non-zero is reported rather than retried. Nothing a hook does can hold
up a poll, a keystroke or another hook.

## Layout

- `blubat-core`: the device model, both data sources, the poller, the event
  engine and the config types. Depends on no terminal library, so a frontend
  other than the TUI stays buildable.
- `blubat`: the binary, holding the CLI and the TUI over that core. It owns
  argument parsing, rendering and exit codes, and nothing else. The dashboard
  is one loop over one channel: keypresses and readings arrive as events, a
  pure `update` folds each into the next state, and a pure `render` draws it.

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
