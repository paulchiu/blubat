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
`trackpad-battery` shell script blubat takes its inspiration from. The live
TUI, threshold notifications and the background daemon come after it, so bare
`blubat` prints help rather than opening a dashboard for now.

blubat reads no configuration file and writes nothing, with one exception:
`blubat wait` may create `~/.local/state/blubat/watches/`.

## Usage

```
blubat list [--json] [--all]        # every device that reports a battery
blubat status [--device <match>]    # one device, human readable
              [--json | --number]   # machine readable variants
blubat wait --device <match>        # poll until the level is reached, then
            --until <level>         # post a desktop notification
            [--interval 60s] [--timeout <duration>]
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

## Layout

- `blubat-core`: the device model, both data sources, the poller, the event
  engine and the config types. Depends on no terminal library, so a frontend
  other than the TUI stays buildable.
- `blubat`: the binary, holding the CLI and (later) the TUI over that core. It
  owns argument parsing, rendering and exit codes, and nothing else.

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
