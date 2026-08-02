# architecture

This covers the two data sources and how they tier, connect and disconnect
nudges, the crate layout, and blubat's own state files. See the
[README](../README.md) for installing and a quick start.

## Two sources, two tiers

Apple HID peripherals such as the Magic Trackpad report through IOKit and are
absent from `system_profiler`; devices such as the MX Keys and AirPods report
through `system_profiler` and never appear in the HID class. blubat reads
both and merges them, keeping the source and freshness of each reading
visible.

The two sources run on their own tiers: the IOKit read on every tick, and the
much more expensive `system_profiler` call on a slower one with its result
cached in between. That call has a timeout (`profiler_timeout`), and a call
that times out or comes back unreadable never fails the poll: blubat keeps the
last good reading, marks it degraded, and says so on stderr or on the
dashboard, so the levels stay visible while they age.

## Nudges

An Apple HID peripheral connecting or disconnecting cuts both tiers' waits
short, through IOKit's own matched and terminated notifications, so a
reconnected trackpad is read at once rather than at the next tick. Devices
that only `system_profiler` sees, AirPods among them, publish no such
notification and are still picked up on the ordinary tick.

The nudge reaches the slow tier immediately but its answer is delivered with
the next fast tick, since every reading blubat hands out is a merge of both
sources. A connect or disconnect is therefore seen at once for anything IOKit
reports, and within one `foreground_interval` or `daemon_interval` for
anything only `system_profiler` knows about.

## Status

Milestone M0 is complete: both data sources, the merge, and the one-shot CLI
(`list`, `status`, `wait`) that reaches parity with the `trackpad-battery`
shell script blubat takes its inspiration from. M1 is complete: bare `blubat`
opens a live dashboard listing every device with its level, charge state,
trend and freshness, sorted, filtered and narrowed from the keyboard. M2 is
complete: the config file, the threshold event engine, the desktop
notifications and the hooks that run alongside them, all live in the
dashboard and reloadable with `r`. M3 is complete: the background daemon
under launchd, the device detail view `enter` opens over the selected device,
hiding that survives a restart, and a Homebrew tap built by the release
pipeline.

## Layout

- `blubat-core`: the device model, both data sources, the poller, the event
  engine and the config types. Depends on no terminal library, so a frontend
  other than the TUI stays buildable.
- `blubat`: the binary, holding the CLI, the TUI, the daemon, the notifier,
  the hook runner and the launchd plumbing over that core. It owns argument
  parsing, rendering and exit codes, and nothing else. The dashboard is one
  loop over one channel: keypresses, readings and finished hooks arrive as
  events, a pure `update` folds each into the next state, and a pure `render`
  draws it. Everything a reading sets off beyond a redraw (stepping the
  engine, saving its state, posting a banner, starting a hook) sits in one
  effects layer the loop calls, never in `update`. The daemon drives that
  same layer with no view attached, which is what makes resident mode the
  dashboard minus one component rather than a second implementation of it.

## State files

blubat writes one thing into the config file and nothing else: `h` on the
[dashboard](dashboard.md) maintains `[dashboard] hidden`, leaving the rest of
the file, its comments included, exactly as it was. Everything else it
creates is its own state, under `~/.local/state/blubat/`: the event engine's
`state.toml`, the `watches/` directory `blubat wait` may drop into, the
`tui.lock` and `daemon.lock` files its resident modes hold while they run,
and the two logs the daemon writes under launchd. The one file outside both
is the LaunchAgent plist, written to `~/Library/LaunchAgents/` by `daemon
install` and by nothing else.
