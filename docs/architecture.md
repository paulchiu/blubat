# architecture

This covers the four data sources and how they tier, connect and disconnect
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

The dashboard's `R` key sends the same nudge by hand: see [`R` in
docs/dashboard.md](dashboard.md#refreshing).

## The daemon's own sources: BMAP and GATT, daemon only

Two kinds of device are unreported through both sources above, and the
background daemon reads each of them itself.

Neither IOKit nor `system_profiler` gives macOS a reliable battery level for
a Bluetooth Classic headset such as a Bose QC. blubat speaks a slice of
Bose's own BMAP protocol over RFCOMM instead, read only, one GET query per
sweep; see [`blubat-core`'s `bmap`
module](../blubat-core/src/bmap.rs) for the wire format and which product ids
are supported.

A third party Bluetooth LE peripheral, an MX Keys or a Keychron among them,
publishes its level through the standard Battery Service (`180F`) and nowhere
else: IOKit's `BatteryPercent` covers Apple HID peripherals only and
`system_profiler` carries a battery field for AirPods and little else, so
such a keyboard sits at `unreported` in blubat while System Settings shows it
at 95%. blubat reads that service over CoreBluetooth, from the peripherals
macOS has already connected (`retrieveConnectedPeripheralsWithServices`) and
never from a scan.

CoreBluetooth identifies a peripheral by a per host UUID rather than by its
Bluetooth address, and the `CoreBluetoothCache` that used to map the two is
not present in `/Library/Preferences/com.apple.Bluetooth.plist` on a current
macOS, so a GATT reading is matched back to a device **by name, exactly**: a
peripheral whose name is exactly one the daemon's own device list already
knows is recorded under that device's address, and one that matches nothing
is skipped in silence. A device another source already has a level for is
never read over GATT at all, so this source can only ever fill a gap and
never displace a direct reading; the one exception is a device GATT itself
last answered for, which is refreshed every sweep like any other.

Only the background daemon may open either link. macOS attributes Bluetooth
access through TCC to the process responsible for it: under launchd the
daemon is responsible for itself and the usage description its own binary
embeds lets TCC prompt for it, but under a terminal the terminal is the
responsible process and TCC aborts the process outright rather than prompt,
whatever blubat's own `Info.plist` says. So the TUI, `list`, `status` and
`wait` never touch IOBluetooth or CoreBluetooth at all: the code that does
lives behind `daemon::run` in the binary crate and nothing else in the
workspace can name it, and `blubat-core` itself carries neither dependency.

The daemon shares what it reads through a file instead of a channel. Each
pass, on the same cadence and timeout as the `system_profiler` slow tier,
runs the BMAP sweep and then the GATT sweep and writes every reading either
took to `readings.toml` under the state directory; every read of a snapshot,
from the daemon's own poll loop, the dashboard and every one-shot command,
merges that file back in as data sources of their own, judged by the same
`read_at` freshness every other source already is. Each sweep folds only the
addresses it answered for, so one failing costs the other nothing. A machine
with no daemon running, or one under an older blubat that never wrote the
file, simply has no daemon data: never an error, and a TUI run without the
daemon behaves exactly as it always has.

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

- `blubat-core`: the device model, all four data sources, the poller, the
  event engine and the config types. Depends on no terminal library, so a
  frontend other than the TUI stays buildable. Its `bmap` module owns the
  BMAP wire format, its `gatt` module the name matching and the Battery Level
  value, and its `readings` module the `readings.toml` handoff the two share,
  but none of them IOBluetooth or CoreBluetooth itself; see
  [above](#the-daemons-own-sources-bmap-and-gatt).
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
  `daemon::bmap`, `daemon::gatt` and the `daemon::sweep` that runs both are
  the one part of it with no counterpart in the dashboard, reachable only
  from `daemon::run`.

## State files

blubat writes one thing into the config file and nothing else: `h` on the
[dashboard](dashboard.md) maintains `[dashboard] hidden`, leaving the rest of
the file, its comments included, exactly as it was. Everything else it
creates is its own state, under `~/.local/state/blubat/`: the event engine's
`state.toml`, the daemon's own `readings.toml` handoff from its BMAP and GATT
sweeps,
the `watches/` directory `blubat wait` may drop into, the `tui.lock` and
`daemon.lock` files its resident modes hold while they run, and the two logs
the daemon writes under launchd. The one file outside both is the LaunchAgent
plist, written to `~/Library/LaunchAgents/` by `daemon install` and by
nothing else.
