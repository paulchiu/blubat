# notifications and hooks

This covers the six events, hysteresis and re-arming, notification identity,
and the hook environment. See the [README](../README.md) for installing and a
quick start.

Both subscribe to the same six events (`low_battery`, `critical_battery`,
`charged`, `connected`, `disconnected`, `stale`), and both fire on a threshold
crossing rather than on a level sitting past one. A device already below its
low threshold when blubat starts is recorded rather than announced, and
re-arms only once it has recovered past the threshold by `rearm_margin`, so a
level oscillating around the boundary raises one event instead of forty. That
armed and fired state, and each hook's debounce clock, live in
`~/.local/state/blubat/state.toml` and survive a restart.

`charged` additionally needs a device that is not reporting itself as
draining, since it says a charge has finished: an earbud put back in its case
lifts the device's level without being anything to announce. Devices that
report no charge state at all, which is every one `system_profiler` sees,
raise it on the level alone.

`[notifications]` switches the banners per event and nothing else: an event a
toggle silences still runs its hooks, since the two are separate subscribers.

## Notification identity

blubat is an unbundled binary, so it has no notification identity of its own
and macOS attributes its banners to another app: Terminal on the primary path,
Script Editor on the `osascript` fallback taken when that path errors. A muted
identity, or a Focus mode, swallows the banner while the send still reports
success, which is what `blubat notify-test` is for:

```
$ blubat notify-test
test banner delivered by the notification centre as com.apple.Terminal
If no banner appeared, that identity is muted: check Focus and the notification
settings for it.
```

## Hooks

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
or exits non-zero is reported rather than retried: on the dashboard's own
line, since anything printed underneath would land on top of the frame it
just drew. Nothing a hook does can hold up a poll, a keystroke or another
hook.

See [docs/configuration.md](configuration.md) for the `[notifications]`,
`[defaults]` and `[[hook]]` config that drives all of this.
