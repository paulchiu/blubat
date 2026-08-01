//! Drawing the dashboard, as a set of small widgets over `App`.
//!
//! Layout only: nothing here decides anything, it just places what the state
//! already says. Every function takes the state borrowed and returns a widget,
//! so a view can be drawn into a test buffer without a terminal.

use blubat_core::Device;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Cell, Clear, Padding, Paragraph, Row, Table, TableState};

use super::app::{App, KEYMAP};
use super::theme;

/// Kept in front of every name so rows stay aligned whatever the gutter holds.
const GUTTER: &str = "  ";

/// The selected row's gutter marker, which colour alone could not carry.
const MARKER: &str = "\u{258e} ";

pub fn render(frame: &mut Frame, app: &App) {
    let screen = frame.area();
    let [status, _spacer, devices, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .horizontal_margin(1)
    .areas(screen);

    render_status(frame, app, status);
    render_devices(frame, app, devices);
    frame.render_widget(keys_footer(app), footer);

    if app.keymap_open {
        render_keymap(frame, screen);
    }
}

/// The one line of context above the table, with anything degraded on the right.
fn render_status(frame: &mut Frame, app: &App, area: Rect) {
    let [left, right] =
        Layout::horizontal([Constraint::Min(0), Constraint::Length(14)]).areas(area);

    frame.render_widget(status_line(app), left);
    frame.render_widget(warning_line(app.warnings().len()), right);
}

fn status_line(app: &App) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            "blubat",
            Style::new().fg(theme::ACCENT).add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!("   {}", summary(app)), Color::DarkGray),
    ])
}

/// How much the dashboard knows, how often it polls and when it polls next.
fn summary(app: &App) -> String {
    let poll = format!("poll {}", seconds(app.interval));

    app.next_poll_in().map_or_else(
        || format!("waiting for the first reading   {poll}"),
        |next| {
            format!(
                "{}   {poll}   next {}",
                counted(app.devices().len(), "device"),
                seconds(next)
            )
        },
    )
}

/// Warnings are counted rather than printed here: the reading is still usable,
/// and the count is the cue that the merge behind it is degraded.
fn warning_line(warnings: usize) -> Line<'static> {
    match warnings {
        0 => Line::default(),
        count => Line::from(Span::styled(counted(count, "warning"), Color::Yellow)).right_aligned(),
    }
}

fn render_devices(frame: &mut Frame, app: &App, area: Rect) {
    if app.devices().is_empty() {
        frame.render_widget(nothing_yet(app), area);
        return;
    }

    // The table owns the scroll offset that follows the selection, and the
    // selection itself lives in `App`, so the state is built fresh each frame.
    let mut state = TableState::new().with_selected(Some(app.selected));

    frame.render_stateful_widget(device_table(app.devices()), area, &mut state);
}

/// The device table: who, how full, and what they are doing.
///
/// Deliberately the minimum that proves the loop draws real readings. The
/// columns the dashboard ships with, and the split between active and inactive
/// devices, land on top of this shape.
fn device_table(devices: &[Device]) -> Table<'_> {
    let header = Row::new([
        Cell::from(format!("{GUTTER}Device")),
        Cell::from(Line::from("Battery").right_aligned()),
        Cell::from("State"),
    ])
    .style(Color::DarkGray);
    let widths = [
        Constraint::Length(30),
        Constraint::Length(7),
        Constraint::Min(12),
    ];

    Table::new(devices.iter().map(device_row), widths)
        .header(header)
        .column_spacing(1)
        .row_highlight_style(Style::new().bg(theme::SELECTION_BG))
        .highlight_symbol(Span::styled(MARKER, Style::new().fg(theme::ACCENT)))
}

fn device_row(device: &Device) -> Row<'_> {
    let level = device.levels.lowest();

    Row::new(vec![
        Cell::from(Line::from(vec![
            Span::raw(GUTTER),
            Span::raw(device.name.as_str()),
        ])),
        Cell::from(
            Line::from(Span::styled(
                theme::percent(level),
                Style::new()
                    .fg(theme::level_color(level))
                    .add_modifier(Modifier::BOLD),
            ))
            .right_aligned(),
        ),
        Cell::from(state(device)),
    ])
}

/// What a device is doing, which for a disconnected one is nothing: its level
/// is the last one macOS saw rather than a live reading.
fn state(device: &Device) -> Span<'static> {
    if device.connected {
        Span::styled(device.charge.to_string(), Color::Gray)
    } else {
        Span::styled("disconnected", Color::DarkGray)
    }
}

/// Stands in for the table before the first reading, and on a bare machine.
fn nothing_yet(app: &App) -> Paragraph<'static> {
    let message = if app.reading.is_some() {
        "no Bluetooth devices reported"
    } else {
        "waiting for the first reading"
    };

    Paragraph::new(message).style(Color::DarkGray)
}

/// The keys live in the current view, which is what makes the footer contextual.
fn keys_footer(app: &App) -> Line<'static> {
    let spans = app
        .keys()
        .iter()
        .flat_map(|binding| {
            [
                Span::styled(binding.keys, Color::Gray),
                Span::styled(format!(" {}  ", binding.label), Color::DarkGray),
            ]
        })
        .collect::<Vec<_>>();

    Line::from(spans)
}

/// The full keymap, centred over the dashboard rather than replacing it.
fn render_keymap(frame: &mut Frame, screen: Rect) {
    let area = centred(screen, 28, KEYMAP.len() as u16 + 2);

    frame.render_widget(Clear, area);
    frame.render_widget(keymap(), area);
}

fn keymap() -> Paragraph<'static> {
    let rows = KEYMAP
        .iter()
        .map(|binding| {
            Line::from(vec![
                Span::styled(format!("{:>5}  ", binding.keys), theme::ACCENT),
                Span::raw(binding.label),
            ])
        })
        .collect::<Vec<_>>();

    Paragraph::new(rows).block(
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
    use blubat_core::{Snapshot, Timestamp};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Cell as Drawn;

    use super::super::app::tests::{app, device, loaded, reading, three_devices};
    use super::super::app::{Event, update};
    use super::*;

    /// What a real terminal of this size would show, one string per row.
    fn drawn(app: &App, width: u16, height: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("a test terminal");

        terminal
            .draw(|frame| render(frame, app))
            .expect("a rendered frame");

        let buffer = terminal.backend().buffer();
        buffer
            .content()
            .chunks(usize::from(buffer.area.width))
            .map(|row| row.iter().map(Drawn::symbol).collect::<String>())
            .collect()
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
    fn the_status_line_says_when_the_next_reading_is_due() {
        let app = loaded();
        let ticked = update(
            app.clone(),
            Event::Tick(Timestamp::from_unix(app.now.unix() + 3)),
        );

        assert!(line_containing(&app, "blubat").contains("3 devices"));
        assert!(line_containing(&app, "blubat").contains("poll 5s"));
        assert!(line_containing(&app, "blubat").contains("next 5s"));
        assert!(
            line_containing(&ticked, "blubat").contains("next 2s"),
            "the countdown moves on a tick alone"
        );
    }

    #[test]
    fn the_footer_carries_the_keys_of_the_view_on_screen() {
        let dashboard = drawn(&loaded(), 100, 30);
        let footer = dashboard.last().expect("a footer row").clone();

        assert!(footer.contains("q quit"), "{footer}");
        assert!(footer.contains("j/k move"), "{footer}");
        assert!(footer.contains("? help"), "{footer}");
    }

    #[test]
    fn the_keymap_overlay_covers_the_dashboard_and_changes_the_footer() {
        let open = update(loaded(), Event::Key('?'));
        let rows = drawn(&open, 100, 30);
        let screen = rows.join("\n");

        assert!(screen.contains("keys"), "the overlay is titled\n{screen}");
        assert!(screen.contains("j/k"), "and lists the keymap\n{screen}");
        assert!(
            rows.last().expect("a footer row").contains("? close"),
            "the footer follows the view"
        );
    }

    #[test]
    fn the_selected_row_is_marked_in_the_gutter() {
        let selected = update(loaded(), Event::Key('j'));

        assert!(
            line_containing(&selected, "MX Keys M Mac").contains(MARKER),
            "the second row is marked"
        );
        assert!(
            !line_containing(&selected, "Magic Trackpad").contains(MARKER),
            "and the first one is not"
        );
    }

    #[test]
    fn an_empty_dashboard_says_which_kind_of_empty_it_is() {
        assert!(screen(&app()).contains("waiting for the first reading"));

        let empty = update(app(), Event::Reading(reading(Vec::new())));
        assert!(screen(&empty).contains("no Bluetooth devices reported"));
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
    fn a_disconnected_device_reads_as_disconnected_rather_than_as_a_state() {
        let mut offline = device("AirPods Pro", "74-15-f5-02-8e-38", Some(45));
        offline.connected = false;

        let app = update(app(), Event::Reading(reading(vec![offline])));

        assert!(line_containing(&app, "AirPods Pro").contains("disconnected"));
    }

    #[test]
    fn no_size_a_terminal_can_be_panics_the_render() {
        let open = update(loaded(), Event::Key('?'));

        for (width, height) in [(1, 1), (20, 3), (40, 10), (100, 30), (200, 60)] {
            drawn(&loaded(), width, height);
            drawn(&open, width, height);
        }
    }
}
