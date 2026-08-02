# blubat

blubat is a single binary that reports the battery level of every Bluetooth
peripheral macOS already knows about, as a one-shot CLI reading, JSON for
scripts and status bars, or a live TUI. macOS has no single source for this:
Apple HID peripherals such as the Magic Trackpad report through IOKit and are
absent from `system_profiler`, while devices such as the MX Keys and AirPods
report through `system_profiler` and never appear in the HID class. blubat
reads both, merges them, and watches the result for threshold crossings it can
notify on and run hooks against.

![blubat dashboard](docs/assets/demo.gif)

## Installing

```sh
brew install paulchiu/tap/blubat
```

From a checkout instead:

```sh
cargo install --path blubat
```

## Usage

```
blubat                              # the live dashboard: ? lists every key
blubat list [--json] [--all]        # every device that reports a battery
blubat status [--device <match>]    # one device, human readable
              [--json | --number]   # machine readable variants
blubat wait --device <match>        # hand a one-shot watch to a running daemon
            --until <level>         # and return, or poll here if none is
            [--interval 60s] [--timeout <duration>]  # running, then notify
blubat daemon install               # write the LaunchAgent and start it
```

```
$ blubat list
NAME                   ADDRESS            LEVEL  STATE       SOURCE
MX Keys M Mac          de-df-38-f0-46-9b  100%   unknown     system_profiler
Paul's Magic Trackpad  30-82-16-f2-24-90  83%    on battery  iokit

$ blubat status --device trackpad
Paul's Magic Trackpad  83%  on battery

$ blubat wait --device trackpad --until 100 --interval 5m
```

Bare `blubat` opens the dashboard. The keys worth knowing on sight:

```
q      quit                  s  cycle the order: level, name, last seen
j/k    move the selection    /  filter on name or address, esc clears it
enter  the detail view       h  hide the selected device, H show hidden again
r      reload the config     ?  the full keymap
```

The rest, including `config`, `daemon` and `notify-test`, is in
[docs](#documentation).

## Documentation

- [docs/dashboard.md](docs/dashboard.md): the dashboard, its keys, the detail
  view, hiding devices, the theme and glyph, and what reloads on `r`.
- [docs/scripting.md](docs/scripting.md): the machine readable contract, exit
  codes, `--number`, and the `--json` schema.
- [docs/configuration.md](docs/configuration.md): the full TOML schema,
  threshold resolution, and `config validate`.
- [docs/notifications-and-hooks.md](docs/notifications-and-hooks.md): the
  events, hysteresis and re-arming, notification identity, and the hook
  environment.
- [docs/daemon.md](docs/daemon.md): the background daemon, install, status
  and uninstall, and how it hands off with the dashboard and `wait`.
- [docs/architecture.md](docs/architecture.md): the two data sources and their
  tiers, nudges, the crate layout, and blubat's state files.
- [docs/releasing.md](docs/releasing.md): the release pipeline, its two
  workflows, its secrets, and cargo-dist.

## Developing

Recipes live in the `justfile`:

```
just build
just test
just lint
just fmt
just ci
```

`just ci` mirrors the pipeline; [docs/releasing.md](docs/releasing.md) has
what it checks. A pull request needs exactly one of the `major`, `minor`,
`patch` or `norelease` labels before it can merge.

## Requirements

macOS only. Rust 1.95 or newer. No entitlements, no sudo, no helper app
bundle.

## Licence

MIT, see [LICENSE](LICENSE).
