# dashboard

This covers the live dashboard: its keys, the detail view, hiding devices, the
theme and charging glyph, and what a reload or a refresh does. See the
[README](../README.md) for installing and a quick start.

Bare `blubat` opens the dashboard, or prints the command help and exits 0 when
there is no terminal to draw one on, so a piped `blubat` still answers. The
trend column is a six cell sparkline over the levels read this run, with dots
for a device nothing has been read from yet. Connected devices come first;
disconnected ones sit under a dimmed `disconnected` heading with their own
count, keeping their last seen level out of the critical summary. A device no
source reports a level for is listed as `unreported` rather than dropped,
which is where a Bose headset and a third party Bluetooth LE keyboard otherwise sit:
both read instead once the [background daemon](daemon.md) is running, out of
the levels macOS itself has cached, over BMAP or over the Battery Service
rather than IOKit or `system_profiler`, and show up here as `bluetoothd`,
`bmap` and `gatt` in the source column like any other reading. A narrow
terminal gives up columns from the right rather than breaking the table.

```
q      quit                  s  open the sort menu: level, name, last seen
j/k    move the selection    /  filter on name or address, esc clears it
enter  the detail view       h  hide the selected device, H show hidden again
i      hide the disconnected section, i again shows it
r      reload the config     c  edit the config file, reloading it on return
R      refresh both sources  ?  the full keymap, titled with the version blubat is running
```

`q` and ctrl+c both leave the dashboard; the keymap overlay and the sort menu
both take the keyboard while they are open, so `esc` closes whichever is up
before anything else responds again. The device table itself sits in its own
border, in the same bordered language as the detail view's panels.

`s` opens a menu of the columns the table can be sorted by rather than cycling
through them: `l` sorts by level, `n` by name, `t` by last seen. Each opens
its column at the direction that makes sense of it first: emptiest level
first, name A to Z, freshest reading first. Choosing the same column again
flips the direction instead of reapplying it, and the header of the sorted
column carries an arrow saying which way it is currently read. `esc` or `s`
again backs out of the menu without changing anything.

`i` drops the disconnected section off the table for the rest of the run and
brings it back with the same key; the footer reads `hide disconnected` or
`show disconnected` depending on which it would do next. Like `h`, it writes
back to the config file: `[dashboard] hide_inactive` is what the dashboard
opens showing (the key name predates the connected/disconnected wording), so
the choice survives a restart the same way a hide does.

## Detail view

`enter` opens the detail view over the selected device, named on a plain line
at the top rather than inside a frame of its own: the panels below it carry
their own borders, so the view is not a box around a box. It answers the
questions one table row has no room for, all of which are about time: a chart
of the levels read this run against the threshold that would raise an event,
the charge or drain rate behind it, an estimate to full or to empty where the
level is actually moving, the thresholds the device is judged by, and the
events blubat has raised for it. A multi-battery device lists each of its
batteries under the one level every threshold is applied to. The history is in
memory and per run, so the chart starts empty after a restart and fills as
blubat polls.

`j` and `k` move to the next and previous device without leaving the view, over
the same row list the dashboard shows: hidden rows only where `H` is showing
them, and the disconnected section absent where `i` has hidden it. `esc` and
`enter` both back out to the dashboard with the selection on whichever device
that left it on. It leaves nothing else live, so there is no way to act on a
device from a view of another one:

```
esc/enter  back to the dashboard      j/k  next/previous device      q  quit
```

## Editing the config file

`c` suspends the dashboard, opens `~/.config/blubat/config.toml` in `$EDITOR`
(or failing that `$VISUAL`), and reloads the file the same way `r` does once
the editor closes: the reload's own rules apply, so a file the editor leaves
unparsable is reported and changes nothing. A file that does not exist yet
opens as the full commented template rather than a blank page; see
[docs/configuration.md](configuration.md#the-self-documenting-file) for what
that means for a file that already exists. Nothing set in either variable is a
notice on the dashboard rather than a crash, the same message `blubat config
edit` gives on the command line.

## Reloading

`r` re-reads `~/.config/blubat/config.toml` in place: thresholds, notification
toggles, hooks, the colour scheme and the charging glyph all take the new
values without a restart. `[poll]` takes effect live too: a changed
`foreground_interval`, `profiler_interval` or `profiler_timeout` reaches the
running poller rather than waiting for a restart, though the fast tier is at
most one of its own ticks away from noticing and the slow tier picks the
change up once the interval it is already waiting out ends. A file that will
not parse is reported on a line of its own and changes nothing, so the config
that was working a moment ago keeps working and the dashboard never exits over
a typo. The same line carries a hook that went wrong, which is why hook output
goes nowhere near stdout.

A row is painted red below the same `critical` threshold the events are raised
by, so the count on the status line and the banners agree by construction: a
device configured `critical = 40` is red and counted at 39%, which is also the
level that raises `critical_battery` for it.

## Refreshing

`R` forces an immediate read of both device sources: IOKit answers at once
and `system_profiler` is asked for an early read too, the same one a device
connecting or disconnecting already triggers. That source keeps a five second
floor between early reads, so holding `R` down does not turn into a stream of
`system_profiler` calls. It changes nothing about the config; that is `r`'s
job.

## Hiding

`h` hides for good. It writes the device's address into `[dashboard] hidden`
in the config file, alongside `hide_inactive` in the one table blubat ever
writes there, so the next dashboard and the next machine reading that dotfile
open without it. `H` shows hidden devices again and a second `h` brings one
back, dropping every match that was hiding it, whether blubat wrote it or a
person did. Hiding is blubat's own view of a device and nothing more: the
device stays paired, macOS still knows it, and blubat never unpairs anything.

Once `H` has brought hidden devices onto the table, each one carries a dim
marker beside its name (an eye-off glyph on a Nerd Font terminal, `[h]`
otherwise) so a hidden row is never mistaken for a shown one.

## Theme and glyph

The charging mark is ascii by default and becomes the Nerd Font bolt when the
environment says a Nerd Font is in use, which is a guess: set
`BLUBAT_NERD_FONT=1` or `=0` to settle it either way, or `charging_glyph` in
`[theme]` to settle it for good.

A disconnected device keeps the level macOS last saw, which carries no
timestamp and can be arbitrarily old. It is labelled `last seen` wherever it
is shown, and `wait` ignores it rather than completing on a stale number.
Durations are written as bare seconds or with an `s`, `m` or `h` suffix.
