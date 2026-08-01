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

Pre-release, not usable yet. Version 0.0.1 is a name reservation and a
workspace skeleton: it builds and prints a placeholder, and reads no devices.

Milestone M0 (the IOKit source, the `system_profiler` source, the merge, and the
CLI surface that reaches parity with the `trackpad-battery` shell script blubat
takes its inspiration from) is in progress. The live TUI, threshold
notifications and the background daemon come after it.

## Layout

- `blubat-core`: the device model, both data sources, the poller, the event
  engine and the config types. Depends on no terminal library, so a frontend
  other than the TUI stays buildable.
- `blubat`: the binary, holding the CLI and (later) the TUI over that core.

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
