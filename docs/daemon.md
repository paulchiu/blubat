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

$ blubat daemon uninstall
removed com.paulchiu.blubat
```

`daemon status` answers the three separate questions in order, since a daemon
can be installed without being loaded and loaded without currently running:
uninstalling one that was never loaded says so and removes the plist anyway.
`daemon run` is the resident loop itself, which launchd starts and which is
worth running by hand only to watch what the daemon is doing on a terminal.
Both logs are plain text and appended to, so `tail -f
~/.local/state/blubat/daemon.log` follows a daemon already under launchd.

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
