//! One device on its own: how full it is, where it is heading, what it is
//! judged by, and what it has raised.
//!
//! The dashboard answers which device needs attention; this view answers the
//! questions a single row has no room for, all of which are about time. Layout
//! only, as in [`super::render`]: everything drawn here is already in the state.

use std::fmt;

use blubat_core::{Device, Direction, Event, Levels, Raised, Thresholds, Trend};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols::Marker;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Axis, Block, BorderType, Chart, Dataset, GraphType, Paragraph};

use super::app::App;
use super::render;
use super::theme::{self, Palette};

/// Seconds in the hour the chart's axis and the rates are both stated in.
const SECONDS_PER_HOUR: f64 = 3_600.0;

/// Cells the stats panel takes, which is what its longest row needs.
const STATS_WIDTH: u16 = 33;

/// Rows the event log takes, borders included.
const EVENTS_HEIGHT: u16 = 7;

/// What separates two pieces of the line under the battery bar.
const SEPARATOR: &str = "  \u{b7}  ";

/// Tenths of the power panel the battery bar takes, and the most it may take.
const BAR_SHARE: usize = 4;
const BAR_CEILING: usize = 48;

/// Draws `device` over the whole screen, in place of the dashboard.
pub fn render(frame: &mut Frame, app: &App, device: &Device, area: Rect) {
    let palette = app.look.palette;
    let outer = panel(&format!(" blubat | {} ", device.name), palette);
    let inner = outer.inner(area);
    frame.render_widget(outer, area);

    let [power, middle, events, footer] = Layout::vertical([
        Constraint::Length(power_height(device.levels)),
        Constraint::Min(0),
        Constraint::Length(EVENTS_HEIGHT),
        Constraint::Length(1),
    ])
    .areas(inner);
    let [chart, stats] =
        Layout::horizontal([Constraint::Min(0), Constraint::Length(STATS_WIDTH)]).areas(middle);

    render_power(frame, app, device, power);
    render_chart(frame, app, device, chart);
    render_stats(frame, app, device, stats);
    render_events(frame, app, device, events);
    frame.render_widget(render::keys_footer(app), footer);
}

/// A rounded panel with an accented title, which is this view's chrome.
///
/// The dashboard is deliberately borderless for density; this view is
/// genuinely several panels, so it keeps the frames around them.
fn panel(title: &str, palette: Palette) -> Block<'static> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(palette.dim))
        .title(Span::styled(title.to_string(), palette.accent))
}

/// The rows the power panel needs: the bar, the line under it, and one row per
/// battery where the device reports more than one.
fn power_height(levels: Levels) -> u16 {
    let parts = if levels.multi_battery() {
        levels.present().count()
    } else {
        0
    };

    4 + u16::try_from(parts).unwrap_or(0)
}

/// How full the device is, what it is doing, and when it was last heard from.
fn render_power(frame: &mut Frame, app: &App, device: &Device, area: Rect) {
    let block = panel(" power ", app.look.palette);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let lines: Vec<Line<'static>> = std::iter::once(level_line(app, device, inner.width))
        .chain(sub_levels(app, device))
        .chain([reading_line(app, device)])
        .collect();

    frame.render_widget(Paragraph::new(lines), inner);
}

/// The hero line: the level that stands for the device, and what it means.
fn level_line(app: &App, device: &Device, width: u16) -> Line<'static> {
    let palette = app.look.palette;
    let level = device.levels.lowest();
    let colour = palette.level(level, app.thresholds(device));
    let (filled, trough) = theme::bar(level, bar_width(width));

    Line::from(vec![
        Span::styled(
            format!("{:>4}  ", theme::percent(level)),
            Style::new().fg(colour).add_modifier(Modifier::BOLD),
        ),
        Span::styled(filled, colour),
        Span::styled(trough, palette.dim),
        Span::styled(format!("  {}", doing(app, device)), palette.text),
    ])
}

/// One row per battery, for a device that reports more than one.
///
/// The line above already carries the level of a single battery device, and
/// carries the emptiest of a multi battery one, which is the number every
/// threshold is applied to.
fn sub_levels<'a>(app: &'a App, device: &'a Device) -> impl Iterator<Item = Line<'static>> + 'a {
    let palette = app.look.palette;
    let thresholds = app.thresholds(device);
    let multi = device.levels.multi_battery();

    device
        .levels
        .present()
        .filter(move |_| multi)
        .map(move |(part, level)| {
            let level = Some(level);
            let (filled, trough) = theme::bar(level, theme::BAR_WIDTH);

            Line::from(vec![
                Span::styled(format!("  {:<8}", part.to_string()), palette.dim),
                Span::styled(
                    format!("{:>4}  ", theme::percent(level)),
                    palette.level(level, thresholds),
                ),
                Span::styled(filled, palette.level(level, thresholds)),
                Span::styled(trough, palette.dim),
            ])
        })
}

/// Where the reading came from, whether the link is up, and how old it is.
fn reading_line(app: &App, device: &Device) -> Line<'static> {
    let palette = app.look.palette;
    let link = if device.connected {
        "connected"
    } else {
        "last seen"
    };
    let said = format!(
        "{link}{SEPARATOR}{}{SEPARATOR}last reading {}",
        device.source,
        theme::age(app.now.unix().saturating_sub(device.read_at.unix()))
    );
    let stale = if app.is_stale(device) {
        Span::styled(format!("{SEPARATOR}stale"), palette.low)
    } else {
        Span::raw("")
    };

    Line::from(vec![Span::styled(said, palette.dim), stale])
}

/// What the device is doing, and what its rate says that will come to.
///
/// The estimate is the whole reason this view exists: a level says how full a
/// battery is and a rate says how fast it is moving, but only the two together
/// answer whether to go and find the cable.
fn doing(app: &App, device: &Device) -> String {
    let state = if device.connected {
        device.charge.to_string()
    } else {
        "last seen level".to_string()
    };

    match estimate(app, device) {
        Some(estimate) => format!("{state}, est. {estimate}"),
        None => state,
    }
}

/// Where the current rate is taking the device, and how long it has to go.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Estimate {
    /// Seconds to the charged threshold at the rate it is charging.
    ToFull(i64),
    /// Seconds to empty at the rate it is draining.
    ToEmpty(i64),
}

impl fmt::Display for Estimate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (seconds, end) = match *self {
            Estimate::ToFull(seconds) => (seconds, "full"),
            Estimate::ToEmpty(seconds) => (seconds, "empty"),
        };

        write!(f, "{} to {end}", theme::span(seconds))
    }
}

impl Estimate {
    /// What `trend` says is left of `level`, absent where nothing can be said.
    ///
    /// A flat level is going nowhere, so it has no estimate rather than an
    /// endless one, and a device with no trend behind it has nothing to
    /// extrapolate from at all.
    fn of(level: Option<u8>, trend: Option<Trend>, thresholds: Thresholds) -> Option<Self> {
        let (level, trend) = (f64::from(level?), trend?);
        let seconds = |points: f64| (points.max(0.0) / trend.rate.abs() * SECONDS_PER_HOUR) as i64;

        match trend.direction {
            Direction::Rising => Some(Self::ToFull(seconds(f64::from(thresholds.high) - level))),
            Direction::Falling => Some(Self::ToEmpty(seconds(level))),
            Direction::Flat => None,
        }
    }

    /// How long it has to go, without the end it is going to.
    fn left(self) -> String {
        let (Self::ToFull(seconds) | Self::ToEmpty(seconds)) = self;

        theme::span(seconds)
    }
}

/// The estimate for one device, which only a live reading can carry.
///
/// A disconnected device's level is whatever macOS last persisted, so
/// extrapolating from it would put a countdown on a number that stopped moving
/// when the link went down.
fn estimate(app: &App, device: &Device) -> Option<Estimate> {
    Estimate::of(
        device.active_level(),
        app.history.trend(&device.address),
        app.thresholds(device),
    )
}

/// The level over time, against the threshold that would raise an event.
///
/// A chart needs two moments to draw a line between, so a run that has sampled
/// this device once or not at all says so rather than drawing a point.
fn render_chart(frame: &mut Frame, app: &App, device: &Device, area: Rect) {
    let palette = app.look.palette;
    let levels = points(app, device);
    let Some(oldest) = levels
        .first()
        .map(|(hours, _)| *hours)
        .filter(|hours| *hours < 0.0)
    else {
        let block = panel(" battery ", palette);

        frame.render_widget(
            Paragraph::new("no history yet: the chart fills as blubat polls")
                .style(palette.dim)
                .block(block),
            area,
        );
        return;
    };

    let low = f64::from(app.thresholds(device).low);
    let threshold = [(oldest, low), (0.0, low)];
    let axis = Style::new().fg(palette.dim);
    let chart = Chart::new(vec![
        Dataset::default()
            .marker(Marker::Dot)
            .graph_type(GraphType::Line)
            .style(axis)
            .data(&threshold),
        Dataset::default()
            .marker(Marker::Braille)
            .graph_type(GraphType::Line)
            .style(Style::new().fg(palette.accent))
            .data(&levels),
    ])
    .block(panel(
        &format!(
            " battery, last {} ",
            theme::span((-oldest * SECONDS_PER_HOUR) as i64)
        ),
        palette,
    ))
    .legend_position(None)
    .x_axis(Axis::default().style(axis).bounds([oldest, 0.0]).labels([
        format!("{} ago", theme::span((-oldest * SECONDS_PER_HOUR) as i64)),
        "now".to_string(),
    ]))
    .y_axis(
        Axis::default()
            .style(axis)
            .bounds([0.0, 100.0])
            .labels(["0", "50", "100"]),
    );

    frame.render_widget(chart, area);
}

/// One device's samples as hours before now against level, oldest first.
///
/// Relative hours rather than stamps, so the axis reads as a span and the
/// present sits at the origin however long the run has been going.
fn points(app: &App, device: &Device) -> Vec<(f64, f64)> {
    app.history
        .samples(&device.address)
        .map(|sample| {
            (
                (sample.at.unix() - app.now.unix()) as f64 / SECONDS_PER_HOUR,
                f64::from(sample.level),
            )
        })
        .collect()
}

/// The numbers the chart cannot show: the rate, the thresholds and the device.
fn render_stats(frame: &mut Frame, app: &App, device: &Device, area: Rect) {
    let palette = app.look.palette;
    let block = panel(" stats ", palette);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let width = usize::from(inner.width);
    let thresholds = app.thresholds(device);
    let trend = app.history.trend(&device.address);
    let estimate = estimate(app, device);
    let row = |label, value: String, colour| stat(width, label, &value, colour, palette);
    let lines = vec![
        row(rate_label(trend), rate(trend), palette.text),
        row("trend", direction(trend), trend_colour(trend, palette)),
        row(
            estimate_label(estimate),
            estimate.map_or_else(|| theme::UNKNOWN.to_string(), Estimate::left),
            palette.text,
        ),
        Line::default(),
        row("low", theme::percent(Some(thresholds.low)), palette.low),
        row(
            "critical",
            theme::percent(Some(thresholds.critical)),
            palette.critical,
        ),
        row(
            "charged at",
            theme::percent(Some(thresholds.high)),
            palette.ok,
        ),
        Line::default(),
        row("address", device.address.as_str().to_string(), palette.text),
        row("source", device.source.to_string(), palette.text),
        row(
            "type",
            device.kind.clone().unwrap_or_else(|| theme::UNKNOWN.into()),
            palette.text,
        ),
        row(
            "samples",
            app.history.samples(&device.address).count().to_string(),
            palette.text,
        ),
    ];

    frame.render_widget(Paragraph::new(lines), inner);
}

/// One `label      value` row, padded so every value ends on the right edge.
fn stat(width: usize, label: &str, value: &str, colour: Color, palette: Palette) -> Line<'static> {
    let spent = label.chars().count() + value.chars().count();
    let gap = width.saturating_sub(spent).max(1);

    Line::from(vec![
        Span::styled(label.to_string(), palette.dim),
        Span::raw(" ".repeat(gap)),
        Span::styled(value.to_string(), colour),
    ])
}

/// A rate is named by which way it is going, since that is what makes it news.
fn rate_label(trend: Option<Trend>) -> &'static str {
    match trend.map(|trend| trend.direction) {
        Some(Direction::Rising) => "charge rate",
        Some(Direction::Falling) => "drain rate",
        _ => "rate",
    }
}

fn rate(trend: Option<Trend>) -> String {
    trend.map_or_else(
        || theme::UNKNOWN.to_string(),
        |trend| theme::rate(trend.rate),
    )
}

fn direction(trend: Option<Trend>) -> String {
    trend.map_or_else(
        || theme::UNKNOWN.to_string(),
        |trend| trend.direction.to_string(),
    )
}

fn trend_colour(trend: Option<Trend>, palette: Palette) -> Color {
    match trend.map(|trend| trend.direction) {
        Some(Direction::Rising) => palette.charging,
        Some(Direction::Falling) => palette.low,
        Some(Direction::Flat) => palette.text,
        None => palette.dim,
    }
}

/// The stats row an estimate goes in, named by the end it is heading for.
fn estimate_label(estimate: Option<Estimate>) -> &'static str {
    match estimate {
        Some(Estimate::ToFull(_)) => "to full",
        Some(Estimate::ToEmpty(_)) => "to empty",
        None => "estimate",
    }
}

/// What blubat has raised for this device, newest first.
fn render_events(frame: &mut Frame, app: &App, device: &Device, area: Rect) {
    let palette = app.look.palette;
    let block = panel(" recent events ", palette);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let mut lines: Vec<Line<'static>> = app
        .journal
        .recent(&device.address)
        .map(|raised| event_line(app, raised))
        .collect();

    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "nothing raised for this device yet",
            palette.dim,
        )));
    }

    frame.render_widget(Paragraph::new(lines), inner);
}

fn event_line(app: &App, raised: &Raised) -> Line<'static> {
    let palette = app.look.palette;
    let level = theme::percent(raised.level);
    let threshold = raised
        .threshold
        .map(|threshold| format!(", threshold {threshold}%"))
        .unwrap_or_default();

    Line::from(vec![
        Span::styled(
            format!(
                "{:<10}",
                theme::age(app.now.unix().saturating_sub(raised.at.unix()))
            ),
            palette.dim,
        ),
        Span::styled(
            format!("{:<18}", raised.event.to_string()),
            event_colour(raised.event, palette),
        ),
        Span::styled(format!("at {level}{threshold}"), palette.text),
    ])
}

/// An event carries the colour of the band it belongs to, so the log reads in
/// the same language as the table it was opened from.
fn event_colour(event: Event, palette: Palette) -> Color {
    match event {
        Event::LowBattery | Event::Stale => palette.low,
        Event::CriticalBattery => palette.critical,
        Event::Charged | Event::Connected => palette.ok,
        Event::Disconnected => palette.dim,
    }
}

/// Cells the battery bar is drawn in, leaving room for the state beside it.
///
/// A share of the panel rather than a fixed width, so a wide terminal spends
/// the room it has on the bar and a narrow one still has somewhere to put the
/// estimate.
fn bar_width(width: u16) -> usize {
    (usize::from(width) * BAR_SHARE / 10).min(BAR_CEILING)
}

#[cfg(test)]
mod tests {
    use blubat_core::Direction;

    use super::*;

    const HOUR: f64 = 3_600.0;

    fn trend(rate: f64) -> Option<Trend> {
        let direction = match rate {
            rate if rate > 0.0 => Direction::Rising,
            rate if rate < 0.0 => Direction::Falling,
            _ => Direction::Flat,
        };

        Some(Trend { rate, direction })
    }

    fn levels(present: [Option<u8>; 4]) -> Levels {
        Levels {
            main: present[0],
            left: present[1],
            right: present[2],
            case: present[3],
        }
    }

    fn estimate(level: Option<u8>, rate: f64) -> Option<Estimate> {
        Estimate::of(level, trend(rate), Thresholds::BUILT_IN)
    }

    #[test]
    fn a_climbing_level_is_counted_up_to_the_charged_threshold() {
        assert_eq!(estimate(Some(23), 10.0), Some(Estimate::ToFull(27_720)));
        assert_eq!(
            estimate(Some(23), 10.0).map(Estimate::left),
            Some("7h 42m".to_string())
        );
        assert_eq!(
            estimate(Some(23), 10.0).map(|estimate| estimate.to_string()),
            Some("7h 42m to full".to_string())
        );
    }

    #[test]
    fn a_dropping_level_is_counted_down_to_empty() {
        assert_eq!(estimate(Some(23), -4.0), Some(Estimate::ToEmpty(20_700)));
        assert_eq!(
            estimate(Some(23), -4.0).map(|estimate| estimate.to_string()),
            Some("5h 45m to empty".to_string())
        );
    }

    #[test]
    fn a_level_going_nowhere_has_nothing_to_estimate() {
        assert_eq!(estimate(Some(23), 0.0), None, "a flat trend");
        assert_eq!(
            Estimate::of(Some(23), None, Thresholds::BUILT_IN),
            None,
            "and one reading, which is no trend at all"
        );
        assert_eq!(estimate(None, -4.0), None, "as is nothing to count from");
    }

    #[test]
    fn a_level_already_past_the_threshold_it_climbs_to_is_there_now() {
        assert_eq!(estimate(Some(100), 10.0), Some(Estimate::ToFull(0)));
        assert_eq!(
            estimate(Some(100), 10.0).map(Estimate::left),
            Some("0m".to_string()),
            "rather than a wait that runs backwards"
        );
    }

    #[test]
    fn the_charged_threshold_the_device_is_judged_by_is_the_one_counted_to() {
        let unplug_at_80 = Thresholds {
            high: 80,
            ..Thresholds::BUILT_IN
        };

        assert_eq!(
            Estimate::of(Some(20), trend(10.0), unplug_at_80),
            Some(Estimate::ToFull(6 * HOUR as i64))
        );
    }

    #[test]
    fn the_power_panel_grows_a_row_for_every_battery_beyond_the_first() {
        let airpods = levels([None, Some(100), Some(97), Some(68)]);

        assert_eq!(power_height(levels([Some(42), None, None, None])), 4);
        assert_eq!(
            power_height(Levels::default()),
            4,
            "nothing read is one row"
        );
        assert_eq!(power_height(airpods), 7);
        assert_eq!(power_height(levels([Some(42), Some(9), None, None])), 6);
    }

    #[test]
    fn the_bar_takes_a_share_of_the_panel_up_to_a_ceiling() {
        assert_eq!(bar_width(0), 0);
        assert_eq!(bar_width(30), 12);
        assert_eq!(bar_width(96), 38);
        assert_eq!(bar_width(400), BAR_CEILING, "a wide terminal stops here");
    }

    #[test]
    fn a_rate_is_labelled_by_the_direction_that_makes_it_news() {
        assert_eq!(rate_label(trend(10.0)), "charge rate");
        assert_eq!(rate_label(trend(-4.0)), "drain rate");
        assert_eq!(rate_label(trend(0.0)), "rate");
        assert_eq!(rate_label(None), "rate");

        assert_eq!(rate(trend(-4.24)), "4.2%/h");
        assert_eq!(rate(None), theme::UNKNOWN);
        assert_eq!(direction(trend(10.0)), "rising");
        assert_eq!(direction(None), theme::UNKNOWN);
    }

    #[test]
    fn an_estimate_names_the_end_it_is_heading_for() {
        assert_eq!(estimate_label(estimate(Some(23), 10.0)), "to full");
        assert_eq!(estimate_label(estimate(Some(23), -4.0)), "to empty");
        assert_eq!(estimate_label(None), "estimate");
    }
}
