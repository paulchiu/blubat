# background daemon

This covers the launchd daemon: install, status, uninstall, restart, and how
it hands over with the dashboard and `wait`. See the [README](../README.md)
for installing and a quick start.

Notifications and hooks only fire while blubat is running. `blubat daemon
install` writes a LaunchAgent at
`~/Library/LaunchAgents/com.paulchiu.blubat.plist` pointing at the binary that
installed it, and bootstraps it into the user's GUI domain, so the same engine
keeps evaluating events with nothing on screen. It polls on `daemon_interval`
rather than the dashboard's faster tick, restarts only when it exits badly,
waits 30 seconds between restarts so a daemon that cannot start cannot spin,
and writes its stdout and stderr to `daemon.log` and `daemon.error.log` under
`~/.local/state/blubat/`.

Nothing installs it for you. blubat never writes that plist on a first run or
an upgrade, and `daemon uninstall` boots the agent out and removes the file
again.

A Homebrew install is pinned to the stable `<prefix>/bin/blubat` shim rather
than the versioned Cellar path underneath it, so `brew upgrade` deleting that
version does not orphan the agent. Anyone who installed before this was added
fixes it once by rerunning `blubat daemon install`.

A Bose headset's battery level, a third party Bluetooth LE peripheral's, and
anything else only macOS itself has a number for, only ever come from this
daemon: the TUI and every one-shot command never touch Bluetooth for any of
them, so a machine with the daemon not running or not yet installed simply
shows them as `unreported`, the same as a device blubat cannot read at all.
Reading them needs [the cache macOS keeps, BMAP over RFCOMM or the Battery
Service over
GATT](architecture.md#the-daemons-own-sources-bluetoothd-bmap-and-gatt),
which needs macOS's Bluetooth permission, and macOS attributes that
permission to whichever process is responsible: under launchd that is this
binary itself, and the `NSBluetoothAlwaysUsageDescription` `build.rs` embeds
in it is what lets TCC create the row for blubat under System Settings →
Privacy & Security → Bluetooth on first sweep rather than aborting the
process. The cache read runs in a short-lived child the sweep spawns, for the
reason [architecture.md
gives](architecture.md#the-daemons-own-sources-bluetoothd-bmap-and-gatt), and
that child reads under the same grant, since responsibility is inherited. That
row has to be granted once, the same as any other app's; running `blubat`
bare in a terminal never asks for it, because the terminal, not blubat, would
be the process TCC held responsible.

The plist names the config file and the state directory the install resolved
rather than leaving the daemon to work them out again: launchd starts an agent
with almost no environment, so a daemon resolving its own would land
somewhere else than the blubat that installed it whenever `XDG_CONFIG_HOME` or
`XDG_STATE_HOME` is set. The daemon reads that config once at startup, so a
config change reaches it on the next `daemon install`, which boots out
whatever was loaded and starts it again.

```
$ blubat daemon install
installed com.paulchiu.blubat
  plist   /Users/paul/Library/LaunchAgents/com.paulchiu.blubat.plist
  running /opt/homebrew/bin/blubat daemon run
  config  /Users/paul/.config/blubat/config.toml
  state   /Users/paul/.local/state/blubat
  logging /Users/paul/.local/state/blubat/daemon.log

$ blubat daemon status
label     com.paulchiu.blubat
plist     /Users/paul/Library/LaunchAgents/com.paulchiu.blubat.plist
loaded    yes
running   yes, pid 4242
live      yes, last beat 2026-09-18T04:18:17Z
ready     yes, last sweep 2026-09-18T04:18:00Z
files     16 descriptors held

$ blubat daemon uninstall
removed com.paulchiu.blubat
```

`daemon status` answers five separate questions in order, since a daemon can
be installed without being loaded, loaded without currently running, and
running without still doing anything: uninstalling one that was never loaded
says so and removes the plist anyway. The last two answers are
[health](#liveness-and-readiness), and they are the only two launchd cannot
give. The `files` line answers nothing; it is a
[measurement](#descriptors) of the daemon, and absent from a report whose
record does not carry one.
`daemon run` is the resident loop itself, which launchd starts and which is
worth running by hand only to watch what the daemon is doing on a terminal.
Both logs are plain text and appended to, so `tail -f
~/.local/state/blubat/daemon.log` follows a daemon already under launchd.

## Liveness and readiness

A process existing is not the same as a process working. A daemon once kept
its loop turning for 28 hours while every sweep it made failed: launchd
reported it running, `daemon status` agreed, `readings.toml` had not been
written since the previous day, and the dashboard's status line said `all ok`.
A monitor that cannot notice it has stopped monitoring is worth fixing on its
own, whatever made the sweeps fail.

So the daemon writes `health.toml` beside its other state on every poll pass,
and again the moment a sweep's readings reach disk, holding two moments and
one measurement: when the loop last came round, when a sweep's readings last
actually reached disk, and how many descriptors the process was holding as it
wrote that down. Writing a landing as it lands rather than at the next pass is what keeps
a daemon that has just started from reading as not ready for a whole
`daemon_interval` after it already is. Everything else reads that file rather
than asking launchd for a pid, because a daemon wedged badly enough to stop
sweeping is wedged badly enough to stop writing here: the record goes stale on
its own, and nothing has to notice on its behalf.

Those two moments answer the two questions separately:

- **live**: the poll loop beat inside the last three `daemon_interval`s.
- **ready**: a sweep saved its readings inside the last three
  `profiler_interval`s.

Three of each, rather than one, because a pass runs late whenever the machine
sleeps or a source is slow. Both windows come from the `[poll]` section in
force, so slowing the daemon down moves what counts as silence with it.

A daemon that is live and not ready is the incident state: the loop is turning
and nothing it produces is worth trusting. `daemon status` says so and points
at `daemon.log`; the [dashboard](dashboard.md) says `daemon not ready`
on its status line and stops claiming `all ok`. One that is not live at all
reads `daemon down` and points at `blubat daemon restart`.

A machine with no daemon installed has never written the file, which is
neither of those states. `daemon status` reports `no heartbeat recorded` with
nothing to fix, and the dashboard shows exactly what it always did: running
blubat without a daemon is a documented way to use it, not a fault. The last
sweep survives a restart, though, so restarting a daemon whose sweeps had
stopped landing does not make it read as ready until one actually lands. A
restart does take up a sweep recorded inside the last `daemon_interval` rather
than repeating it, since a fresh pass would cost every headset a connection to
say what is already on disk.

A file that is there but cannot be made sense of, because it will not open or
because it is not the TOML the daemon writes, is a fourth state and not the
same as any of them. Both answers read `unknown`, on the dashboard's status
line and in `daemon status`, and `all ok` is withheld: the daemon may well be
running, and this machine cannot tell either way. The record is saying nothing
rather than saying the daemon has stopped, which is the one reading that would
be wrong in both directions.

## Descriptors

`health.toml` also records `open_files`, the number of descriptors the process
held on the pass it was written, and `daemon status` reads it back as the
`files` line.

It is there because of how a leak ends. A daemon once accumulated `/dev/null`
descriptors at roughly one a sweep for eighteen days, and nothing noticed
until it reached its per-process limit and every sweep after that failed with
`os error 24`. The count had been climbing the whole time with nowhere to be
seen, so the leak was only ever visible as the failure it eventually caused.

One figure says nothing on its own: a healthy daemon holds a dozen or two.
The series across passes is the instrument. A count that keeps climbing pass
after pass is a leak while the daemon is still working, which is early enough
to do something about.

The count is taken from `/dev/fd`, and the read of that directory is itself
one of the descriptors counted, so the figure runs one high. A record written
by a blubat that predates the count, or one whose count could not be taken,
carries nothing here rather than a zero, and `daemon status` leaves the line
out rather than reporting a process holding none.

## Upgrading

A `brew upgrade` replaces the binary on disk and changes its ad-hoc code
signature, but it does not touch the running agent: launchd is still holding
the old binary's image open and keeps executing it until something stops it.
Killing that process does not fix it either. launchd keeps a lightweight code
requirement for whichever binary it last bootstrapped, the swapped binary no
longer satisfies it, and the agent starts failing to spawn instead of picking
up the new one: `launchctl print` shows it stuck at `spawn scheduled` with
`last exit code = 78 (EX_CONFIG)`, and nothing new ever reaches `daemon.log`.

`blubat daemon restart` is the fix: it boots the agent out and bootstraps it
again from the plist already on disk, which is what makes launchd read the
new binary's signature and refresh the stored requirement. `daemon install`
also does this, but rewrites the plist first; restart does not need to, since
nothing about the plist (the Homebrew shim path, the config, the state
directory) changes on an upgrade. `daemon status` names this fix directly
when it finds the agent loaded but not running.

```
$ blubat daemon restart
restarted com.paulchiu.blubat
  plist   /Users/paul/Library/LaunchAgents/com.paulchiu.blubat.plist
```

Expect a fresh Bluetooth permission prompt on the sweep after an upgrade too.
TCC ties the grant to the binary's identity, a version change is enough to
reset it, and the new binary asks again the same way the very first `daemon
install` did.

## Handing over with the dashboard

Open the [dashboard](dashboard.md) while the daemon is running and the
dashboard takes over: it holds `~/.local/state/blubat/tui.lock` for as long as
it is up, and the daemon checks that file before every banner and every hook,
so an event fires once rather than twice. The dashboard owns the event state
while it holds the lock, and the daemon reads that state back when the lock
goes away, so quitting the dashboard does not set off everything it saw while
it was open. The lock is the kernel's rather than a pid written in a file, so
a dashboard that was killed frees it at once. A second dashboard opened beside
the first draws everything and announces nothing, since the first one up owns
the side effects.

## Handing over `wait`

`blubat wait` hands over the same way. With a daemon running it writes a
one-shot watch into `~/.local/state/blubat/watches/` and returns at once; the
daemon takes the file over on its next poll whether or not a dashboard is up,
posts the same banner when the level arrives, and drops a watch whose deadline
passes or whose device nothing paired matches. With no daemon running, `wait`
polls in the terminal as before.
