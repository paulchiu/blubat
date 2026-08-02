# scripting

This covers the machine readable contract: matching a device, exit codes,
`--number`, and the `--json` schema. See the [README](../README.md) for
installing and a quick start.

These are the compatibility surface. They are treated as a contract and will
not change shape within a major version.

## Matching a device

`--device` takes a substring, matched case insensitively against both the
device name and its Bluetooth address, so `trackpad`, `Magic`, `30-82-16` and
`30:82:16` all select the same device. `status` reports the one device with a
battery that the arguments identify; if more than one qualifies, with or
without `--device`, it names them and asks you to narrow the substring rather
than picking one.

## Exit codes

| Code | Meaning |
| ---- | ------- |
| 0 | A usable reading was printed. |
| 1 | An error, including a usage error and a `wait` that hit its timeout. |
| 3 | No matching device has a battery. |

Warnings and errors go to stderr, so nothing contaminates a value read from
stdout. `blubat status --json --device X > level.json` never mixes a
diagnostic into the file, though it writes nothing at all when the device is
gone, so branch on the exit code before reading it back.

## `--number`

`blubat status --number` prints the percentage as a bare integer and nothing
else, for direct substitution:

```sh
level=$(blubat status --device trackpad --number) || exit
[ "$level" -ge 30 ] || echo "charge the trackpad"
```

For a multi-battery device such as AirPods, the number is the lowest present
sub-level, because a device is as charged as its emptiest part.

A bare number has nowhere to carry the `last seen` label, so for a
disconnected device it is the level macOS last saw, which can be arbitrarily
old. A script that needs a fresh reading should use `--json` and check
`connected`.

## `--json`

`blubat status --json` emits one object; `blubat list --json` emits an array
of those same objects, in the order the table shows them, and stays an array
(`[]`) when nothing is paired.

Every object names the schema it is written in. `schema_version`, `level`,
`charge`, `source`, `connected` and `read_at` are always written, as `null`
where there is nothing to report, so a script never has to tell an absent
value from a missing key. The descriptive keys (`kind`, `transport`, and each
entry of `levels`) are omitted when they have no value.

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
