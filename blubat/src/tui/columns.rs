//! Which columns the device table draws, and what a narrow terminal costs.
//!
//! Nothing here draws: it answers what fits, so the widths and the drop order
//! can be tested at every terminal size without a frame.

use ratatui::layout::Constraint;

/// Cells the selection marker takes out of the table before any column does.
const MARKER_WIDTH: u16 = 2;

/// One column of the device table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Column {
    Name,
    Kind,
    Bar,
    Level,
    State,
    Trend,
    Age,
}

impl Column {
    /// Every column, in the order the table draws them.
    pub const ALL: [Column; 7] = [
        Column::Name,
        Column::Kind,
        Column::Bar,
        Column::Level,
        Column::State,
        Column::Trend,
        Column::Age,
    ];

    /// The order columns are given up in as the terminal narrows.
    ///
    /// Freshness and trend go first because they are context around a reading;
    /// the bar goes before the state because the percentage beside it says the
    /// same thing in fewer cells. The name and the percentage never appear
    /// here: they are the reading itself, so below the width that holds them
    /// the table truncates rather than drops.
    const GIVEN_UP: [Column; 5] = [
        Column::Age,
        Column::Trend,
        Column::Kind,
        Column::Bar,
        Column::State,
    ];

    pub fn header(self) -> &'static str {
        match self {
            Column::Name => "Device",
            Column::Kind => "Type",
            Column::Bar => "Battery",
            Column::Level => "%",
            Column::State => "State",
            Column::Trend => "Trend",
            Column::Age => "Age",
        }
    }

    /// The cells the column is drawn in where there is room for all of them.
    pub fn width(self) -> u16 {
        match self {
            Column::Name => 26,
            Column::Kind | Column::Bar => 12,
            Column::Level => 4,
            Column::State => 11,
            Column::Trend => 8,
            Column::Age => 9,
        }
    }

    /// How the layout is asked for the column.
    ///
    /// Only the name gives ground: everything else is fixed width, so the
    /// columns pack to the left and a wide terminal leaves slack on the right
    /// rather than stretching the table across it.
    pub fn constraint(self) -> Constraint {
        match self {
            Column::Name => Constraint::Max(self.width()),
            other => Constraint::Length(other.width()),
        }
    }

    /// The cells the column is worth keeping in, which only the name can go
    /// below its full width to reach.
    ///
    /// A name has to stay long enough to tell two devices apart, so the table
    /// gives up a whole column before squeezing it further than this.
    fn floor(self) -> u16 {
        match self {
            Column::Name => 14,
            other => other.width(),
        }
    }
}

/// The columns that fit across a table `width` cells wide.
///
/// Columns are given up one at a time in [`Column::GIVEN_UP`] order until what
/// is left fits, so the same terminal always shows the same table and a narrow
/// one loses the least useful column first.
pub fn fitting(width: u16) -> Vec<Column> {
    Column::GIVEN_UP
        .iter()
        .fold(Column::ALL.to_vec(), |columns, given_up| {
            if needed(&columns) <= width {
                columns
            } else {
                columns.into_iter().filter(|c| c != given_up).collect()
            }
        })
}

/// The cells `columns` need, with the gap between each and the selection marker.
fn needed(columns: &[Column]) -> u16 {
    let cells: u16 = columns.iter().map(|column| column.floor()).sum();
    let gaps = u16::try_from(columns.len().saturating_sub(1)).unwrap_or_default();

    cells + gaps + MARKER_WIDTH
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wide_terminal_shows_every_column() {
        assert_eq!(fitting(100), Column::ALL);
        assert_eq!(fitting(200), Column::ALL);
    }

    #[test]
    fn columns_are_given_up_in_the_documented_order() {
        // The narrowest terminal a column survives on: the first column given
        // up is the one that needs the most room to come back.
        let survives_at = |column| (0..=200).find(|width| fitting(*width).contains(&column));
        let thresholds: Vec<Option<u16>> =
            Column::GIVEN_UP.iter().copied().map(survives_at).collect();

        assert!(
            thresholds.windows(2).all(|pair| pair[0] > pair[1]),
            "{thresholds:?}"
        );
    }

    #[test]
    fn the_name_and_the_level_are_never_given_up() {
        for width in 0..=120 {
            let columns = fitting(width);

            assert!(columns.contains(&Column::Name), "at {width}");
            assert!(columns.contains(&Column::Level), "at {width}");
        }
    }

    #[test]
    fn a_wider_terminal_never_shows_fewer_columns() {
        let counts: Vec<usize> = (0..=120).map(|width| fitting(width).len()).collect();

        assert!(
            counts.windows(2).all(|pair| pair[0] <= pair[1]),
            "{counts:?}"
        );
    }

    #[test]
    fn what_is_kept_fits_unless_nothing_can() {
        for width in 20..=120 {
            let columns = fitting(width);

            assert!(
                needed(&columns) <= width || columns.len() == 2,
                "{columns:?} do not fit in {width}"
            );
        }
    }

    #[test]
    fn every_column_keeps_its_header_and_its_room() {
        for column in Column::ALL {
            assert!(!column.header().is_empty());
            assert!(column.floor() <= column.width());
        }
    }
}
