# configuration

This covers the config file: its full schema, how thresholds resolve, the
`config` subcommands, and the global flags that override where blubat reads
and writes. See the [README](../README.md) for installing and a quick start.

TOML at `~/.config/blubat/config.toml`, resolved with the XDG strategy. The
file is optional: blubat runs on built-in defaults, and `[dashboard] hidden`
and `[dashboard] hide_inactive` are the only things it ever writes into one.
Machine state (the event engine's armed and fired flags, the one-shot
watches) lives apart from it under `~/.local/state/blubat/`; see
[docs/architecture.md](architecture.md) for what lives there.

Parsing is strict. An unknown key, an unknown event name or a duration that
does not parse is an error naming the line it is on, because a typo that
silently does nothing is worse than one that says so.

```toml
[poll]
foreground_interval = "30s"   # tick while the dashboard or a command runs
daemon_interval     = "120s"  # tick under launchd
profiler_interval   = "5m"    # slow tier, cached in between
profiler_timeout    = "10s"   # ceiling on one system_profiler call
stale_after         = "10m"   # no reading for this long raises `stale`

[notifications]
low      = true
critical = true
charged  = true               # "safe to unplug"
connect  = false              # connect and disconnect are noisy by default
stale    = true
sound    = "Glass"

[defaults]
low          = 20
critical     = 10
high         = 100            # the level that raises `charged`
rearm_margin = 1              # recovery required before an event re-arms

[theme]
scheme   = "dark"             # dark, light or mono
accent   = "#39c5cf"          # per colour overrides on top of the scheme
critical = "#f47067"
low      = "#c69026"
ok       = "#57ab5a"
charging_glyph = "+"          # overrides the Nerd Font guess either way

[dashboard]
hidden        = ["MX Master"] # matches, as --device takes them; `h` writes here
sort          = "level"       # level, name or last_seen
hide_inactive = false         # whether the disconnected section opens shown; `i` writes here

# Per device overrides. `match` is the same case insensitive substring
# `--device` takes, tested against the name and the address. The first
# block a device matches is the one that applies.
[[device]]
match        = "Soundcore"
low          = 25             # dies fast below 25, warn earlier
high         = 90             # optimised charging never reports 100
rearm_margin = 5              # reports in coarse steps, needs more slack

# Hooks run a shell command with BLUBAT_* set in the environment.
[[hook]]
event    = "low_battery"      # low_battery, critical_battery, charged,
command  = "~/bin/nag"        # connected, disconnected or stale
debounce = "30m"              # a window, or "once" per re-arm cycle

[[hook]]
event   = "disconnected"
match   = "AirPods"           # optional per hook device filter
command = "~/bin/pause-music"
timeout = "10s"
```

Thresholds resolve most specific first: the first `[[device]]` block the
device matches, then `[defaults]`, then what the device's own IOKit node
advertises, then the built-in 20, 10, 100 and 1.

`[poll] foreground_interval` sets the dashboard's tick. Left unwritten, the
dashboard reads every 5s instead: it is on screen and being read as it
changes, and the fast tier is a single digit millisecond IOKit call.

`[dashboard] hidden` and `[dashboard] hide_inactive` are maintained from the
dashboard as well as by hand, and both live in the one table: `h` appends the
selected device's address and `h` over a shown-again device removes whatever
was hiding it, and `i` flips whether the dashboard opens with the disconnected
section shown. Each key writes only the field it changed, so pressing `h`
never carries `hide_inactive` back over a value the file gained since this
dashboard last read it, and pressing `i` never carries `hidden` back either.
The edit is surgical, so a hand written file keeps its comments, its blank
lines and the order of everything in it. `r` on the
[dashboard](dashboard.md), and `c`'s reload once its editor closes, re-read
both along with the rest of the file, which is what settles a hand edit made
while the dashboard is open.

## The `config` subcommand

```
blubat config path      # print the resolved config path
blubat config edit      # open it in $EDITOR
blubat config validate  # parse it and report what is wrong
```

`blubat config validate` exits 0 when the file is usable or absent and 1 when
it is not, so it fits a dotfiles check. A `[[device]]` block matching nothing
currently visible is a warning rather than a failure, since the device may
simply be switched off.

The dashboard's own `c` opens the same editor `blubat config edit` does: the
same `$EDITOR`/`$VISUAL` resolution, and the same message when neither is set.

## The self-documenting file

`blubat config edit` and the dashboard's `c` seed the full commented template
above the first time either opens a file that does not exist yet, so a
machine that has never been configured opens the whole schema instead of a
blank page: every key behind a `#` with its built-in default, which parses to
exactly the same config as no file at all. A file that predates the template
is introduced to it once instead, whether it is opened by `edit`, opened by
`c`, or just loaded at startup: a marker line and a short pointer go in
front, and a guide section for whichever tables the file does not already
have goes on the end, leaving the user's own text exactly as it was. The
marker line, `## blubat configuration, guide v1`, is what a later run looks
for to skip a file already introduced; keeping that line after deleting the
sections under it by hand opts a file out for good. Like every other write
blubat makes to this file, a file `Config::parse` rejects is never touched:
it is left for `blubat config validate` and the load path to report on.

## Global flags

Every command accepts `--config <path>` and `--state-dir <path>` to read
configuration and keep blubat's own files somewhere other than the resolved
locations for that one invocation. The installed daemon does not resolve
these itself; see [docs/daemon.md](daemon.md) for why.
