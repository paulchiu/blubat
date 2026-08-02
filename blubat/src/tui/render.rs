//! Drawing the dashboard, as a set of small widgets over `App`.
//!
//! Layout only: nothing here decides anything, it just places what the state
//! already says. Every function takes the state borrowed and returns a widget,
//! so a view can be drawn into a test buffer without a terminal.

use blubat_core::{ChargeState, Device, Thresholds};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Cell, Clear, Padding, Paragraph, Row, Table, TableState};

use super::app::{App, KEYMAP, Mode, NOTES, Notice};
use super::columns::{self, Column};
use super::theme::{self, Palette};
use super::view::Rows;

/// Kept in front of every name so rows stay aligned whatever the gutter holds.
const GUTTER: &str = "  ";

/// What separates two pieces of the status line, and the budget they cost.
const GAP: &str = "   ";

/// The selected row's gutter marker, which colour alone could not carry.
const MARKER: &str = "\u{258e} ";

/// A critical device's gutter marker, which sits where the name would start.
const ALERT: &str = "\u{25b2} ";

/// Where the next typed character will land in the filter.
const CURSOR: &str = "\u{2588}";

/// Cells the alert is given, and the width below which it is not drawn at all.
const ALERT_WIDTH: u16 = 14;

/// Draws one frame of `app` into `table`, which carries the scroll offset.
///
/// The offset is the one piece of state a frame leaves behind: ratatui scrolls
/// the table only as far as it takes to bring the selection back into view, so
/// handing it a fresh state each frame would pin the selection to the last
/// visible row.
pub fn render(frame: &mut Frame, app: &App, table: &mut TableState) {
    let screen = frame.area();
    // The notice takes a line only while there is one, so the dashboard keeps
    // the layout it usually has.
    let [status, notice, filter, devices, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(u16::from(app.notice.is_some())),
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .horizontal_margin(1)
    .areas(screen);

    let palette = app.look.palette;
    let rows = app.rows();

    render_status(frame, app, status);

    if let Some(said) = &app.notice {
        frame.render_widget(notice_line(said, palette), notice);
    }

    frame.render_widget(filter_line(app, &rows), filter);
    render_devices(frame, app, &rows, devices, table);
    frame.render_widget(keys_footer(app), footer);

    if app.mode == Mode::Keymap {
        render_keymap(frame, screen, palette);
    }
}

/// What the dashboard has to say about itself, in the colour of how it went.
fn notice_line(notice: &Notice, palette: Palette) -> Line<'static> {
    let colour = if notice.problem {
        palette.alert
    } else {
        palette.accent
    };

    Line::from(Span::styled(notice.text.clone(), colour))
}

/// The one line of context above the table, with the alert count on the right.
///
/// The alert is drawn only where it fits whole: cut short it would read as a
/// different count, which is the one thing on this line that must not be wrong.
fn render_status(frame: &mut Frame, app: &App, area: Rect) {
    let [left, right] =
        Layout::horizontal([Constraint::Min(0), Constraint::Length(ALERT_WIDTH)]).areas(area);

    frame.render_widget(status_line(app, left.width), left);

    if right.width == ALERT_WIDTH {
        frame.render_widget(alert_line(app.critical(), app.look.palette), right);
    }
}

fn status_line(app: &App, width: u16) -> Line<'static> {
    const NAME: &str = "blubat";

    let palette = app.look.palette;
    let warnings = warnings(app.warnings().len(), palette);
    let spent = NAME.len() + GAP.len() + warnings.content.chars().count();
    let room = usize::from(width).saturating_sub(spent);

    Line::from(vec![
        Span::styled(
            NAME,
            Style::new().fg(palette.accent).add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!("{GAP}{}", summary(app, room)), palette.dim),
        warnings,
    ])
}

/// How much the dashboard knows, how it is ordered, and when it reads next.
///
/// Assembled a piece at a time and cut short at `room` rather than truncated,
/// so a narrow terminal loses the last detail whole instead of mid word.
fn summary(app: &App, room: usize) -> String {
    let poll = format!("poll {}", seconds(app.interval));
    let pieces = app.next_poll_in().map_or_else(
        || vec!["waiting for the first reading".to_string(), poll.clone()],
        |next| {
            vec![
                format!("{} active", app.connected().count()),
                format!("sort {}", app.view.sort.label()),
                poll.clone(),
                format!("next {}", seconds(next)),
            ]
        },
    );

    pieces.into_iter().fold(String::new(), |line, piece| {
        let extended = if line.is_empty() {
            piece
        } else {
            format!("{line}{GAP}{piece}")
        };

        if extended.chars().count() <= room {
            extended
        } else {
            line
        }
    })
}

/// Warnings are counted rather than printed: the reading is still usable, and
/// the count is the cue that the merge behind it is degraded.
fn warnings(count: usize, palette: Palette) -> Span<'static> {
    match count {
        0 => Span::raw(""),
        count => Span::styled(format!("{GAP}{}", counted(count, "warning")), palette.low),
    }
}

/// Only connected devices can be critical, so the inactive section never shows here.
fn alert_line(critical: usize, palette: Palette) -> Line<'static> {
    match critical {
        0 => Line::from(Span::styled("all ok", palette.dim)).right_aligned(),
        count => Line::from(Span::styled(
            format!("{ALERT}{count} critical"),
            palette.critical,
        ))
        .right_aligned(),
    }
}

/// The filter, on the line that is otherwise the spacer above the table.
///
/// Present only while it is being typed or narrowing something, so the dense
/// layout keeps its breathing room the rest of the time.
fn filter_line(app: &App, rows: &Rows<'_>) -> Line<'static> {
    let filter = &app.view.filter;
    let typing = app.mode == Mode::Filtering;

    if !typing && !filter.narrows() {
        return Line::default();
    }

    let palette = app.look.palette;
    let cursor = if typing { CURSOR } else { "" };
    let colour = if typing { palette.accent } else { palette.dim };

    Line::from(vec![
        Span::styled(format!("/{}{cursor}", filter.query), colour),
        Span::styled(
            format!("{GAP}{}", counted(rows.len(), "match")),
            palette.dim,
        ),
    ])
}

fn render_devices(
    frame: &mut Frame,
    app: &App,
    rows: &Rows<'_>,
    area: Rect,
    table: &mut TableState,
) {
    if rows.is_empty() {
        frame.render_widget(nothing_to_show(app), area);
        return;
    }

    table.select(Some(table_row(rows, app.selected)));

    frame.render_stateful_widget(device_table(app, rows, area.width), area, table);
}

/// Where the selected device sits among the table's rows.
///
/// The inactive heading is a row of its own, so everything under it is one
/// further down than the selection, which only counts devices.
fn table_row(rows: &Rows<'_>, selected: usize) -> usize {
    if selected < rows.active.len() {
        selected
    } else {
        selected + 1
    }
}

/// The device table: who, how full, and what they are doing.
fn device_table<'a>(app: &'a App, rows: &Rows<'a>, width: u16) -> Table<'a> {
    let palette = app.look.palette;
    let columns = columns::fitting(width);
    let header = Row::new(
        columns
            .iter()
            .map(|column| header_cell(*column))
            .collect::<Vec<_>>(),
    )
    .style(palette.dim);
    let widths = columns
        .iter()
        .map(|column| column.constraint())
        .collect::<Vec<_>>();

    let mut table = rows
        .active
        .iter()
        .map(|device| device_row(app, device, &columns, Section::Active))
        .collect::<Vec<_>>();

    if !rows.inactive.is_empty() {
        table.push(section_row(rows.inactive.len(), palette));
        table.extend(
            rows.inactive
                .iter()
                .map(|device| device_row(app, device, &columns, Section::Inactive)),
        );
    }

    Table::new(table, widths)
        .header(header)
        .column_spacing(1)
        .row_highlight_style(Style::new().bg(palette.selection))
        .highlight_symbol(Span::styled(MARKER, Style::new().fg(palette.accent)))
}

fn header_cell(column: Column) -> Cell<'static> {
    match column {
        Column::Name => Cell::from(format!("{GUTTER}{}", column.header())),
        Column::Level => Cell::from(Line::from(column.header()).right_aligned()),
        other => Cell::from(other.header()),
    }
}

/// Which half of the table a row is in, and what that does to its colours.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Section {
    Active,
    Inactive,
}

impl Section {
    /// `colour` in the live section, and the one dim colour below it.
    ///
    /// Disconnected devices render uniformly dim: their level is what macOS
    /// last persisted, so nothing there competes with a live reading.
    fn tint(self, colour: Color, palette: Palette) -> Color {
        match self {
            Section::Active => colour,
            Section::Inactive => palette.dim,
        }
    }
}

fn device_row<'a>(
    app: &'a App,
    device: &'a Device,
    columns: &[Column],
    section: Section,
) -> Row<'a> {
    let palette = app.look.palette;
    let critical = section == Section::Active
        && theme::is_critical(device.active_level(), app.thresholds(device));
    let cells = columns
        .iter()
        .map(|column| cell(app, device, *column, section, critical))
        .collect::<Vec<_>>();

    Row::new(cells).style(section.tint(palette.text, palette))
}

fn cell<'a>(
    app: &'a App,
    device: &'a Device,
    column: Column,
    section: Section,
    critical: bool,
) -> Cell<'a> {
    let palette = app.look.palette;
    let level = device.levels.lowest();
    let thresholds = app.thresholds(device);

    match column {
        Column::Name => Cell::from(name(device, section, critical, palette)),
        Column::Kind => Cell::from(Span::styled(
            device.kind.as_deref().unwrap_or(theme::UNKNOWN),
            section.tint(palette.dim, palette),
        )),
        Column::Bar => Cell::from(bar(level, section, palette, thresholds)),
        Column::Level => Cell::from(
            Line::from(Span::styled(
                theme::percent(level),
                Style::new()
                    .fg(section.tint(palette.level(level, thresholds), palette))
                    .add_modifier(Modifier::BOLD),
            ))
            .right_aligned(),
        ),
        Column::State => {
            let (text, colour) = state(app, device, critical);

            Cell::from(Span::styled(text, section.tint(colour, palette)))
        }
        Column::Trend => Cell::from(Span::styled(
            theme::sparkline(&recent_levels(app, device)),
            section.tint(
                if critical {
                    palette.critical
                } else {
                    palette.text
                },
                palette,
            ),
        )),
        Column::LastSeen => Cell::from(Span::styled(
            theme::age(app.now.unix().saturating_sub(device.read_at.unix())),
            section.tint(palette.dim, palette),
        )),
    }
}

/// The levels behind one device's sparkline, oldest first.
///
/// Taken from the newest end, since the line only has room for the most recent
/// few and it is the recent ones the trend is about.
fn recent_levels(app: &App, device: &Device) -> Vec<u8> {
    let mut levels: Vec<u8> = app
        .history
        .samples(&device.address)
        .rev()
        .take(theme::SPARK_WIDTH)
        .map(|sample| sample.level)
        .collect();
    levels.reverse();

    levels
}

/// The name, behind the gutter that a critical device puts its mark in.
fn name(device: &Device, section: Section, critical: bool, palette: Palette) -> Line<'_> {
    let marker = if critical {
        Span::styled(ALERT, palette.critical)
    } else {
        Span::raw(GUTTER)
    };
    let colour = if critical {
        palette.alert
    } else {
        section.tint(palette.strong, palette)
    };

    Line::from(vec![marker, Span::styled(device.name.as_str(), colour)])
}

/// The filled run in the level colour, the trough behind it dim.
fn bar(
    level: Option<u8>,
    section: Section,
    palette: Palette,
    thresholds: Thresholds,
) -> Line<'static> {
    let (filled, trough) = theme::battery_bar(level);

    Line::from(vec![
        Span::styled(
            filled,
            section.tint(palette.level(level, thresholds), palette),
        ),
        Span::styled(trough, palette.dim),
    ])
}

/// What a device is doing, which is not always something it is doing.
///
/// A device no source has a level for is unreported rather than absent, and a
/// disconnected one's level is labelled last seen, since macOS keeps reporting
/// it with no timestamp long after the device went away.
fn state(app: &App, device: &Device, critical: bool) -> (String, Color) {
    let palette = app.look.palette;

    if !device.has_battery() {
        ("unreported".to_string(), palette.dim)
    } else if !device.connected {
        ("last seen".to_string(), palette.dim)
    } else if device.charge == ChargeState::Charging {
        (
            format!("{} charging", app.look.glyphs.charging),
            palette.charging,
        )
    } else if critical {
        (device.charge.to_string(), palette.alert)
    } else {
        (device.charge.to_string(), palette.text)
    }
}

/// Announces the disconnected devices, a line clear of the live ones.
fn section_row<'a>(inactive: usize, palette: Palette) -> Row<'a> {
    Row::new(vec![Cell::from(Span::styled(
        format!("inactive ({inactive})"),
        palette.dim,
    ))])
    .top_margin(1)
}

/// Stands in for the table whenever it has no rows, saying which kind of none.
fn nothing_to_show(app: &App) -> Paragraph<'static> {
    let message = if app.reading.is_none() {
        "waiting for the first reading"
    } else if app.devices().is_empty() {
        "no Bluetooth devices reported"
    } else if app.view.filter.narrows() {
        "no device matches the filter"
    } else {
        "every device is hidden; press H to show them"
    };

    Paragraph::new(message).style(app.look.palette.dim)
}

/// The keys live in the current view, which is what makes the footer contextual.
fn keys_footer(app: &App) -> Line<'static> {
    let palette = app.look.palette;
    let spans = app
        .keys()
        .iter()
        .flat_map(|binding| {
            [
                Span::styled(binding.keys, palette.text),
                Span::styled(format!(" {}  ", binding.label), palette.dim),
            ]
        })
        .collect::<Vec<_>>();

    Line::from(spans)
}

/// The full keymap, centred over the dashboard rather than replacing it.
fn render_keymap(frame: &mut Frame, screen: Rect, palette: Palette) {
    let height = KEYMAP.len() + NOTES.len() + 3;
    let area = centred(screen, 68, u16::try_from(height).unwrap_or(u16::MAX));

    frame.render_widget(Clear, area);
    frame.render_widget(keymap(palette), area);
}

fn keymap(palette: Palette) -> Paragraph<'static> {
    let keys = KEYMAP.iter().map(|binding| {
        Line::from(vec![
            Span::styled(format!("{:>5}  ", binding.keys), palette.accent),
            Span::raw(binding.label),
        ])
    });
    let notes = NOTES
        .iter()
        .map(|note| Line::from(Span::styled(*note, palette.dim)));
    let lines = keys
        .chain(std::iter::once(Line::default()))
        .chain(notes)
        .collect::<Vec<_>>();

    Paragraph::new(lines).block(
        Block::bordered()
            .title(" keys ")
            .padding(Padding::horizontal(1)),
    )
}

/// A box of at most `width` by `height` in the middle of `area`.
fn centred(area: Rect, width: u16, height: u16) -> Rect {
    let [_, band, _] = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(height.min(area.height)),
        Constraint::Fill(1),
    ])
    .areas(area);
    let [_, centre, _] = Layout::horizontal([
        Constraint::Fill(1),
        Constraint::Length(width.min(area.width)),
        Constraint::Fill(1),
    ])
    .areas(band);

    centre
}

/// `1 device` and `2 devices`, since both read on the status line.
fn counted(count: usize, noun: &str) -> String {
    let plural = if count == 1 { "" } else { "s" };

    format!("{count} {noun}{plural}")
}

fn seconds(duration: std::time::Duration) -> String {
    format!("{}s", duration.as_secs())
}

#[cfg(test)]
mod tests {
    use blubat_core::{ChargeState, Levels, Snapshot, Timestamp};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::{Buffer, Cell as Drawn};

    use super::super::app::tests::{
        READ_AT, app, config, device, loaded, press, reading, three_devices,
    };
    use super::super::app::{Event, Key, update};
    use super::*;

    /// The same dashboard under a `[theme]` table, which is the only thing that
    /// ever changes what it draws in.
    fn looking(app: App, theme: &str) -> App {
        let theme = config(&format!("[theme]\n{theme}\n")).theme;
        let look = app.look.reloaded(&theme);

        App { look, ..app }
    }

    /// The buffer left behind after drawing each of `apps` in turn.
    ///
    /// One table state throughout, as the loop keeps one, so a test can see
    /// where the previous frame's scroll offset leaves the next one.
    fn buffer_of(apps: &[&App], width: u16, height: u16) -> Buffer {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("a test terminal");
        let mut table = TableState::new();

        for app in apps {
            terminal
                .draw(|frame| render(frame, app, &mut table))
                .expect("a rendered frame");
        }

        terminal.backend().buffer().clone()
    }

    /// A buffer as one string per row.
    ///
    /// Trailing blanks are cut: they are invisible on screen and unreadable in
    /// an expected frame, and every difference that shows still shows.
    fn rows_of(buffer: &Buffer) -> Vec<String> {
        buffer
            .content()
            .chunks(usize::from(buffer.area.width))
            .map(|row| {
                row.iter()
                    .map(Drawn::symbol)
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    /// What a real terminal of this size would show, one string per row.
    fn drawn(app: &App, width: u16, height: u16) -> Vec<String> {
        rows_of(&buffer_of(&[app], width, height))
    }

    /// The cell drawing the first character of `needle`, for asserting the
    /// colours and weights a frame compared as text cannot carry.
    fn cell_of<'a>(buffer: &'a Buffer, needle: &str) -> &'a Drawn {
        let symbols: Vec<&str> = buffer.content().iter().map(Drawn::symbol).collect();
        let wanted: Vec<String> = needle
            .chars()
            .map(|character| character.to_string())
            .collect();
        let start = symbols
            .windows(wanted.len())
            .position(|run| run.iter().zip(&wanted).all(|(drawn, want)| drawn == want))
            .unwrap_or_else(|| panic!("`{needle}` is nowhere on screen"));

        &buffer.content()[start]
    }

    /// The whole screen as one string, for asserting a line is somewhere on it.
    fn screen(app: &App) -> String {
        drawn(app, 100, 30).join("\n")
    }

    fn line_containing(app: &App, needle: &str) -> String {
        drawn(app, 100, 30)
            .into_iter()
            .find(|line| line.contains(needle))
            .unwrap_or_else(|| panic!("no line contains `{needle}`"))
    }

    fn typed(name: &str, kind: &str, address: &str, level: Option<u8>) -> Device {
        Device {
            kind: Some(kind.to_string()),
            ..device(name, address, level)
        }
    }

    /// The dashboard with one of everything: a charging device, a critical one,
    /// an ordinary one, and an inactive section holding a last seen level and a
    /// device no source has ever had a level for.
    fn dashboard() -> App {
        let devices = vec![
            Device {
                charge: ChargeState::Charging,
                ..typed("Magic Trackpad", "trackpad", "30-82-16-f2-24-90", Some(23))
            },
            typed("MX Keys M Mac", "keyboard", "de-df-38-f0-46-9b", Some(67)),
            typed("Soundcore Liberty", "audio", "d0-03-4b-0b-e6-4e", Some(8)),
            Device {
                connected: false,
                read_at: Timestamp::from_unix(READ_AT.unix() - 10_800),
                ..typed("AirPods Pro", "audio", "74-15-f5-02-8e-38", Some(45))
            },
            Device {
                connected: false,
                levels: Levels::default(),
                read_at: Timestamp::from_unix(READ_AT.unix() - 172_800),
                ..typed("MX Master 3S", "mouse", "aa-bb-cc-dd-ee-ff", None)
            },
        ];

        // Five earlier readings a minute apart, each five points fuller, so the
        // sparkline has a full run to draw and every frame below is reproducible.
        let earlier = |step: u8| Snapshot {
            read_at: minutes_before(step),
            devices: devices
                .iter()
                .map(|device| Device {
                    levels: Levels {
                        main: device
                            .levels
                            .main
                            .map(|level| level.saturating_add(5 * step)),
                        ..device.levels
                    },
                    read_at: if device.connected {
                        minutes_before(step)
                    } else {
                        device.read_at
                    },
                    ..device.clone()
                })
                .collect(),
            warnings: Vec::new(),
        };

        let app = (1..=5).rev().fold(app(), |app, step| {
            update(app, Event::Reading(earlier(step)))
        });

        update(app, Event::Reading(reading(devices)))
    }

    fn minutes_before(minutes: u8) -> Timestamp {
        Timestamp::from_unix(READ_AT.unix() - i64::from(minutes) * 60)
    }

    /// More devices than a test terminal can show at once, each one named
    /// distinctly enough to say which of them is on screen.
    fn crowd(count: u8) -> App {
        let devices = (0..count)
            .map(|n| {
                device(
                    &format!("Device {n:02}"),
                    &format!("aa-bb-cc-dd-ee-{n:02x}"),
                    Some(50),
                )
            })
            .collect();

        update(app(), Event::Reading(reading(devices)))
    }

    #[test]
    fn the_dashboard_draws_the_frame_it_is_specified_to_draw() {
        let expected = " blubat   3 active   sort level   poll 5s   next 5s                                    ▲ 1 critical

     Device                   Type         Battery         % State       Trend  Last seen
 ▎ ▲ Soundcore Liberty        audio        █░░░░░░░░░░░   8% on battery  █▇▅▄▂▁ now
     Magic Trackpad           trackpad     ███░░░░░░░░░  23% + charging  █▇▅▄▂▁ now
     MX Keys M Mac            keyboard     ████████░░░░  67% on battery  █▇▅▄▂▁ now

   inactive (2)
     AirPods Pro              audio        █████░░░░░░░  45% last seen   ······ 3h ago
     MX Master 3S             mouse        ░░░░░░░░░░░░   -- unreported  ······ 2d ago



















 q quit  j/k move  enter detail  s sort  / filter  h hide  H show hidden  r reload  ? help";
        assert_frame(&dashboard(), 100, 30, expected);
    }

    #[test]
    fn a_narrow_terminal_drops_columns_rather_than_the_reading() {
        let expected = " blubat   3 active   sort level   poll 5s      ▲ 1 critical

     Device                   Battery         % State
 ▎ ▲ Soundcore Liberty        █░░░░░░░░░░░   8% on battery
     Magic Trackpad           ███░░░░░░░░░  23% + charging
     MX Keys M Mac            ████████░░░░  67% on battery

   inactive (2)
     AirPods Pro              █████░░░░░░░  45% last seen
     MX Master 3S             ░░░░░░░░░░░░   -- unreported









 q quit  j/k move  enter detail  s sort  / filter  h hide";
        assert_frame(&dashboard(), 60, 20, expected);
    }

    /// Fails with the frame that was drawn, since that is what has to be read
    /// to decide whether a change was the point or a regression.
    fn assert_frame(app: &App, width: u16, height: u16, expected: &str) {
        let drawn = drawn(app, width, height).join("\n");

        assert_eq!(drawn, expected, "\nas drawn at {width}x{height}:\n{drawn}");
    }

    #[test]
    fn the_dashboard_shows_the_devices_it_was_handed() {
        let screen = screen(&loaded());

        assert!(screen.contains("blubat"), "{screen}");
        for name in ["Magic Trackpad", "MX Keys M Mac", "Soundcore Liberty"] {
            assert!(screen.contains(name), "{name} is missing from\n{screen}");
        }
        assert!(screen.contains("85%") && screen.contains("42%"), "{screen}");
        assert!(screen.contains("--"), "an unread level is still a row");
        assert!(screen.contains("on battery"), "{screen}");
    }

    #[test]
    fn the_status_line_says_how_it_is_ordered_and_when_it_reads_next() {
        let app = loaded();
        let ticked = update(
            app.clone(),
            Event::Tick(Timestamp::from_unix(app.now.unix() + 3)),
        );

        assert!(line_containing(&app, "blubat").contains("3 active"));
        assert!(line_containing(&app, "blubat").contains("sort level"));
        assert!(line_containing(&app, "blubat").contains("poll 5s"));
        assert!(line_containing(&app, "blubat").contains("next 5s"));
        assert!(
            line_containing(&ticked, "blubat").contains("next 2s"),
            "the countdown moves on a tick alone"
        );
        assert!(
            line_containing(&press(app, "s"), "blubat").contains("sort name"),
            "and follows the order the table is in"
        );
    }

    #[test]
    fn the_footer_carries_the_keys_of_the_view_on_screen() {
        let dashboard = drawn(&loaded(), 100, 30);
        let footer = dashboard.last().expect("a footer row").clone();

        for key in [
            "q quit",
            "j/k move",
            "enter detail",
            "s sort",
            "/ filter",
            "h hide",
            "H show hidden",
            "r reload",
            "? help",
        ] {
            assert!(footer.contains(key), "{key} is missing from `{footer}`");
        }
    }

    /// The overlay covers what is under it, which only a whole frame can say:
    /// the box, its border, the keys it lists and the dashboard rows it hides
    /// are one assertion rather than four substrings on a screen that already
    /// contains them.
    #[test]
    fn the_keymap_overlay_covers_the_dashboard_and_says_what_is_still_to_come() {
        let expected = " blubat   3 active   sort level   poll 5s   next 5s                                    ▲ 1 critical

     Device                   Type         Battery         % State       Trend  Last seen
 ▎ ▲ Soundcore Liberty        audio        █░░░░░░░░░░░   8% on battery  █▇▅▄▂▁ now
     Magic Trackpad           trackpad     ███░░░░░░░░░  23% + charging  █▇▅▄▂▁ now
     MX Keys M Mac            keyboard     ████████░░░░  67% on battery  █▇▅▄▂▁ now

   inactive (2)
     AirPods Pro┌ keys ────────────────────────────────────────────────────────────┐go
     MX Master 3│     q  quit                                                      │go
                │   j/k  move                                                      │
                │ enter  detail                                                    │
                │     s  sort                                                      │
                │     /  filter                                                    │
                │     h  hide                                                      │
                │     H  show hidden                                               │
                │     r  reload                                                    │
                │     ?  help                                                      │
                │                                                                  │
                │ enter opens the detail view, which arrives later.                │
                │ h hides for this session only; a lasting hide arrives later.     │
                │ r re-reads the config file; one it cannot read changes nothing.  │
                └──────────────────────────────────────────────────────────────────┘






 ? close  q quit";
        assert_frame(
            &update(dashboard(), Event::Key(Key::Char('?'))),
            100,
            30,
            expected,
        );
    }

    #[test]
    fn the_selected_row_is_marked_in_the_gutter() {
        let selected = update(loaded(), Event::Key(Key::Char('j')));

        assert!(
            line_containing(&selected, "Magic Trackpad").contains(MARKER),
            "the second row is marked"
        );
        assert!(
            !line_containing(&selected, "MX Keys M Mac").contains(MARKER),
            "and the first one is not"
        );
    }

    #[test]
    fn the_selection_marker_skips_the_inactive_heading() {
        let app = press(dashboard(), "jjj");

        assert!(line_containing(&app, "AirPods Pro").contains(MARKER));
        assert!(!line_containing(&app, "inactive (2)").contains(MARKER));
    }

    #[test]
    fn an_empty_dashboard_says_which_kind_of_empty_it_is() {
        assert!(screen(&app()).contains("waiting for the first reading"));

        let empty = update(app(), Event::Reading(reading(Vec::new())));
        assert!(screen(&empty).contains("no Bluetooth devices reported"));

        let filtered = press(loaded(), "/nothing here");
        assert!(screen(&filtered).contains("no device matches the filter"));

        let all_hidden = press(loaded(), "hhh");
        assert!(screen(&all_hidden).contains("every device is hidden"));
    }

    #[test]
    fn a_degraded_reading_is_counted_on_the_status_line() {
        let degraded = update(
            app(),
            Event::Reading(Snapshot {
                warnings: vec!["system_profiler exited with 1".to_string()],
                ..three_devices()
            }),
        );

        assert!(line_containing(&degraded, "blubat").contains("1 warning"));
    }

    #[test]
    fn a_disconnected_device_sits_under_a_counted_inactive_heading() {
        let rows = drawn(&dashboard(), 100, 30);
        let heading = rows
            .iter()
            .position(|line| line.contains("inactive (2)"))
            .expect("an inactive heading");
        let airpods = rows
            .iter()
            .position(|line| line.contains("AirPods Pro"))
            .expect("a disconnected device");

        assert!(heading < airpods, "the heading introduces the section");
        assert!(
            rows[..heading].iter().any(|line| line.contains("MX Keys")),
            "and the live devices come first"
        );
    }

    #[test]
    fn a_device_no_source_has_a_level_for_is_shown_as_unreported() {
        assert!(line_containing(&dashboard(), "MX Master 3S").contains("unreported"));
        assert!(line_containing(&dashboard(), "AirPods Pro").contains("last seen"));
    }

    #[test]
    fn only_a_live_low_reading_is_marked_critical() {
        let screen = screen(&dashboard());

        assert!(line_containing(&dashboard(), "Soundcore").contains(ALERT));
        assert!(screen.contains(&format!("{ALERT}1 critical")));
        assert!(
            !line_containing(&dashboard(), "AirPods Pro").contains(ALERT),
            "a disconnected 45% is history rather than an alert"
        );
    }

    #[test]
    fn a_dashboard_with_nothing_low_says_so() {
        assert!(screen(&loaded()).contains("all ok"));
    }

    #[test]
    fn the_filter_is_drawn_while_it_is_typed_and_while_it_narrows() {
        let typing = press(loaded(), "/key");
        let kept = update(typing.clone(), Event::Key(Key::Enter));

        assert!(line_containing(&typing, "/key").contains(CURSOR));
        assert!(line_containing(&typing, "/key").contains("1 match"));
        assert!(!line_containing(&kept, "/key").contains(CURSOR));
        assert!(
            drawn(&loaded(), 100, 30)[1].is_empty(),
            "and nowhere to be seen otherwise"
        );
    }

    #[test]
    fn the_footer_follows_the_keys_the_filter_binds() {
        let typing = press(loaded(), "/key");
        let footer = drawn(&typing, 100, 30).last().expect("a footer").clone();

        assert!(footer.contains("esc clear"), "{footer}");
        assert!(footer.contains("enter keep"), "{footer}");
    }

    #[test]
    fn the_charging_glyph_is_the_one_the_dashboard_was_given() {
        let ascii = dashboard();
        let nerd_font = looking(dashboard(), "charging_glyph = \"\u{f0e7}\"");

        assert!(line_containing(&ascii, "Magic Trackpad").contains("+ charging"));
        assert!(
            line_containing(&nerd_font, "Magic Trackpad").contains("\u{f0e7} charging"),
            "a Nerd Font terminal gets the bolt"
        );
    }

    #[test]
    fn no_size_a_terminal_can_be_panics_the_render() {
        let open = update(loaded(), Event::Key(Key::Char('?')));
        let states = [
            app(),
            loaded(),
            open.clone(),
            press(dashboard(), "/a"),
            dashboard(),
        ];

        for app in &states {
            // Densest under the minimum width, which is the band the guarantee
            // is about: a column can be half a cell wide and a line can have
            // nowhere at all to put itself.
            for width in 1..=40 {
                for height in 1..=8 {
                    drawn(app, width, height);
                }
            }
            for width in 1..=120 {
                for height in [1, 3, 30] {
                    drawn(app, width, height);
                }
            }
        }
        for (width, height) in [(1, 1), (5, 5), (1, 30), (20, 3), (40, 10), (200, 60)] {
            drawn(&dashboard(), width, height);
            drawn(&open, width, height);
        }
    }

    #[test]
    fn the_alert_is_drawn_whole_or_not_at_all() {
        let whole = format!("{ALERT}1 critical");

        for width in 1..=120 {
            let screen = drawn(&dashboard(), width, 30).join("\n");

            assert!(
                screen.contains(&whole) || !screen.contains("critical"),
                "a cut alert reads as a different count, at {width}:\n{screen}"
            );
        }
    }

    #[test]
    fn a_table_taller_than_the_terminal_scrolls_with_the_selection() {
        let top = crowd(40);
        let bottom = press(top.clone(), &"j".repeat(39));
        let up = press(bottom.clone(), "k");

        let at_the_end = rows_of(&buffer_of(&[&top, &bottom], 100, 14)).join("\n");
        assert!(
            at_the_end.contains("Device 39"),
            "the table scrolls to the selection\n{at_the_end}"
        );
        assert!(!at_the_end.contains("Device 00"), "{at_the_end}");

        let rows = rows_of(&buffer_of(&[&top, &bottom, &up], 100, 14));
        let marked = rows
            .iter()
            .find(|line| line.contains(MARKER))
            .expect("a marked row");

        assert!(marked.contains("Device 38"), "{marked}");
        assert!(
            rows.iter().any(|line| line.contains("Device 39")),
            "and moving up does not drag the rows above it along\n{}",
            rows.join("\n")
        );
    }

    #[test]
    fn a_dashboard_with_nothing_connected_selects_through_the_inactive_section() {
        let asleep = update(
            app(),
            Event::Reading(reading(
                [
                    ("Magic Trackpad", "30-82-16-f2-24-90"),
                    ("MX Keys M Mac", "de-df-38-f0-46-9b"),
                ]
                .into_iter()
                .map(|(name, address)| Device {
                    connected: false,
                    ..device(name, address, Some(50))
                })
                .collect(),
            )),
        );

        assert!(line_containing(&asleep, "blubat").contains("0 active"));
        assert!(!line_containing(&asleep, "inactive (2)").contains(MARKER));
        assert!(line_containing(&asleep, "Magic Trackpad").contains(MARKER));
        assert!(line_containing(&press(asleep, "j"), "MX Keys M Mac").contains(MARKER));
    }

    /// Colour and weight carry as much of this layout as the glyphs do: the
    /// dimmed inactive section, the level scale, the selection tint and the
    /// critical red are all invisible to a frame compared as text.
    #[test]
    fn the_palette_reaches_the_buffer_it_is_drawn_into() {
        let buffer = buffer_of(&[&press(dashboard(), "j")], 100, 30);
        let cell = |needle| cell_of(&buffer, needle);
        let dark = Palette::DARK;

        assert_eq!(cell("blubat").fg, dark.accent);
        assert!(cell("blubat").modifier.contains(Modifier::BOLD));

        assert_eq!(cell("Soundcore").fg, dark.alert, "a critical name");
        assert_eq!(cell("8%").fg, dark.critical, "and the level under it");
        assert!(cell("8%").modifier.contains(Modifier::BOLD));
        assert_eq!(cell("67%").fg, dark.ok);

        assert_eq!(
            cell("Magic Trackpad").bg,
            dark.selection,
            "the selected row is tinted rather than inverted"
        );
        assert_eq!(cell("MX Keys").bg, Color::Reset);

        assert_eq!(
            cell("AirPods Pro").fg,
            dark.dim,
            "the inactive section is dim throughout"
        );
        assert_eq!(cell("45%").fg, dark.dim);
    }

    /// The whole point of `[theme]`: a scheme and its overrides have to reach
    /// the cells rather than a palette nothing draws with.
    #[test]
    fn the_configured_scheme_and_its_overrides_reach_the_cells() {
        let themed = looking(
            press(dashboard(), "j"),
            "scheme = \"light\"\naccent = \"#39c5cf\"\ncritical = \"#f47067\"",
        );
        let buffer = buffer_of(&[&themed], 100, 30);
        let cell = |needle| cell_of(&buffer, needle);
        let overridden = Color::Rgb(0xf4, 0x70, 0x67);

        assert_eq!(
            cell("blubat").fg,
            Color::Rgb(0x39, 0xc5, 0xcf),
            "the accent"
        );
        assert_eq!(cell("8%").fg, overridden, "the bottom band of the scale");
        assert_eq!(
            cell("Soundcore").fg,
            overridden,
            "and the brighter variant paired with it"
        );
        assert_eq!(
            cell("67%").fg,
            Palette::LIGHT.ok,
            "unwritten, so the scheme's"
        );
        assert_eq!(cell("AirPods Pro").fg, Palette::LIGHT.dim);
        assert_eq!(cell("Magic Trackpad").bg, Palette::LIGHT.selection);
    }

    #[test]
    fn a_notice_takes_a_line_of_its_own_only_while_there_is_one() {
        let quiet = drawn(&loaded(), 100, 30);
        let said = update(
            loaded(),
            Event::Note(Notice::problem("config.toml: expected `=` at line 3")),
        );

        assert!(quiet[1].is_empty(), "no line is spent on nothing to say");
        assert!(
            line_containing(&said, "expected `=`").contains("config.toml"),
            "and the problem is on screen rather than on stderr"
        );
        assert_eq!(
            cell_of(&buffer_of(&[&said], 100, 30), "config.toml").fg,
            Palette::DARK.alert,
            "in the colour of how it went"
        );
        assert_eq!(
            cell_of(
                &buffer_of(
                    &[&update(
                        loaded(),
                        Event::Note(Notice::said("config reloaded"))
                    )],
                    100,
                    30
                ),
                "config reloaded"
            )
            .fg,
            Palette::DARK.accent
        );
    }

    #[test]
    fn every_terminal_wide_enough_keeps_the_name_and_the_level() {
        for width in 20..=120 {
            let screen = drawn(&dashboard(), width, 30).join("\n");

            assert!(screen.contains("8%"), "at {width}:\n{screen}");
        }
    }
}
