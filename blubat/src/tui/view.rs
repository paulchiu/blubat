//! What the dashboard is showing, as opposed to what it has read.
//!
//! The reading is one thing and the view over it is another: the filter, the
//! hidden devices, the order and the split into connected and disconnected all
//! live here, so every one of them can be exercised without a frame.

use blubat_core::Device;

/// The order the table lists devices in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Sort {
    /// Emptiest first, which is the reason to open the dashboard at all.
    #[default]
    Level,
    Name,
    /// Freshest reading first, which is where a degrading merge shows.
    LastSeen,
}

impl Sort {
    /// How the status line names the order.
    pub fn label(self) -> &'static str {
        match self {
            Sort::Level => "level",
            Sort::Name => "name",
            Sort::LastSeen => "last seen",
        }
    }

    /// The direction a column opens in the first time it is chosen: the one
    /// way each column already made sense before there was a second way to
    /// see it.
    pub fn natural(self) -> Direction {
        match self {
            Sort::Level | Sort::Name => Direction::Ascending,
            Sort::LastSeen => Direction::Descending,
        }
    }
}

/// `[dashboard] sort` names a column; the direction it opens on is always
/// that column's own natural one, so the file never has to carry a second key.
impl From<blubat_core::Sort> for Sort {
    fn from(sort: blubat_core::Sort) -> Self {
        match sort {
            blubat_core::Sort::Level => Sort::Level,
            blubat_core::Sort::Name => Sort::Name,
            blubat_core::Sort::LastSeen => Sort::LastSeen,
        }
    }
}

/// Which way a sorted column orders its rows.
///
/// Kept beside [`Sort`] rather than folded into it, since a column and the
/// direction it is read in are two different questions: which one is active
/// never changes what "ascending" means for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Ascending,
    Descending,
}

impl Direction {
    /// The other way, which choosing the active column again switches to.
    pub fn reversed(self) -> Self {
        match self {
            Direction::Ascending => Direction::Descending,
            Direction::Descending => Direction::Ascending,
        }
    }

    /// The glyph the header carries for the column this direction belongs to.
    pub fn arrow(self) -> &'static str {
        match self {
            Direction::Ascending => "\u{2191}",
            Direction::Descending => "\u{2193}",
        }
    }
}

/// The incremental filter: whatever has been typed into it so far.
///
/// Whether it is still being typed is the dashboard's mode rather than a field
/// here, so the query and the keyboard cannot disagree about who owns a key.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Filter {
    pub query: String,
}

impl Filter {
    /// Whether the filter is narrowing the table, which whitespace alone is not.
    pub fn narrows(&self) -> bool {
        !self.query.trim().is_empty()
    }

    fn keeps(&self, device: &Device) -> bool {
        !self.narrows() || device.matches(&self.query)
    }
}

/// Everything the dashboard shows that is not a reading.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct View {
    pub sort: Sort,
    /// Which way `sort` reads. Never `Sort`'s own concern: choosing a column
    /// again is what flips it, and that lives in [`View::choose_sort`] rather
    /// than in the order itself.
    pub direction: Direction,
    pub filter: Filter,
    /// `[dashboard] hidden` as the config file holds it, in file order: the
    /// same substring matches `--device` takes, and what `h` writes back.
    pub hidden: Vec<String>,
    pub show_hidden: bool,
    /// Whether the disconnected section is left off the table, which `i`
    /// toggles and, like `h` toggling `hidden`, writes back to the config file.
    pub hide_inactive: bool,
}

impl Default for View {
    /// A view opens on its default sort at that column's own natural
    /// direction, the same as choosing any other column for the first time.
    fn default() -> Self {
        Self {
            sort: Sort::default(),
            direction: Sort::default().natural(),
            filter: Filter::default(),
            hidden: Vec::new(),
            show_hidden: false,
            hide_inactive: false,
        }
    }
}

impl View {
    /// The view a config file's `[dashboard]` table opens the dashboard on.
    pub fn hiding(hidden: &[String], hide_inactive: bool) -> Self {
        Self {
            hidden: hidden.to_vec(),
            hide_inactive,
            ..Self::default()
        }
    }

    /// Applies `sort`, the same key the sort menu binds it to: the active
    /// column reverses in place, and any other column opens at its own
    /// natural direction rather than carrying the last column's over.
    pub fn choose_sort(&mut self, sort: Sort) {
        self.direction = if self.sort == sort {
            self.direction.reversed()
        } else {
            sort.natural()
        };
        self.sort = sort;
    }

    /// How the status line names the order in force, direction included, so
    /// it stays legible even where the sorted column itself is off screen.
    pub fn sort_label(&self) -> String {
        format!("{} {}", self.sort.label(), self.direction.arrow())
    }

    /// Whether a device is on screen at all, before it is placed in a section.
    fn shows(&self, device: &Device) -> bool {
        self.filter.keeps(device) && (self.show_hidden || !self.hides(device))
    }

    /// Whether `device` is one `h` has hidden, regardless of `show_hidden`.
    pub fn hides(&self, device: &Device) -> bool {
        self.hidden.iter().any(|pattern| device.matches(pattern))
    }

    /// Hides `device`, or shows it again if some match already hid it.
    ///
    /// A new hide is written as the address, the one name for a device that
    /// cannot be renamed out from under it. Showing one again drops every match
    /// that covered it, so a device hidden by a hand written `"MX Master"` goes
    /// back with the same press as one hidden by its address.
    pub fn toggle_hidden(&mut self, device: &Device) {
        let hidden = self.hides(device);
        self.hidden.retain(|pattern| !device.matches(pattern));

        if !hidden {
            self.hidden.push(device.address.to_string());
        }
    }
}

/// The devices on screen, in the two sections the dashboard draws.
///
/// Connected devices come first and disconnected ones follow under their own
/// heading, which is the order the selection moves through them in.
#[derive(Debug, PartialEq, Eq)]
pub struct Rows<'a> {
    pub connected: Vec<&'a Device>,
    pub disconnected: Vec<&'a Device>,
}

impl<'a> Rows<'a> {
    /// The devices `view` shows out of `devices`, in its order.
    ///
    /// `hide_inactive` drops the whole disconnected section rather than
    /// filtering it out device by device, so a device hidden this way is
    /// still connected as far as the status line and the alert count are
    /// concerned. The split itself is raw `connected`: a device that has gone
    /// quiet but is still linked stays in the connected section.
    pub fn of(devices: &'a [Device], view: &View) -> Self {
        let (connected, disconnected) = devices
            .iter()
            .filter(|device| view.shows(device))
            .partition::<Vec<_>, _>(|device| device.connected);

        Self {
            connected: sorted(connected, view.sort, view.direction),
            disconnected: if view.hide_inactive {
                Vec::new()
            } else {
                sorted(disconnected, view.sort, view.direction)
            },
        }
    }

    /// Every row, in the order the selection moves through them.
    pub fn all(&self) -> impl Iterator<Item = &'a Device> {
        self.connected.iter().chain(&self.disconnected).copied()
    }

    pub fn len(&self) -> usize {
        self.connected.len() + self.disconnected.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn get(&self, index: usize) -> Option<&'a Device> {
        self.all().nth(index)
    }
}

/// Orders one section, leaving devices the order cannot separate as they came.
///
/// The merge hands its devices over sorted by name and address, and every sort
/// here is stable, so any two devices at the same level or the same age keep
/// that order rather than swapping between frames.
fn sorted(mut devices: Vec<&Device>, sort: Sort, direction: Direction) -> Vec<&Device> {
    use Direction::{Ascending, Descending};
    use std::cmp::Reverse;

    match (sort, direction) {
        // An unread level sorts last either way: it is not a full battery,
        // and it is not an empty one either, so reversing what the level
        // means never moves it off the end.
        (Sort::Level, Ascending) => devices.sort_by_key(|device| level_key(device)),
        (Sort::Level, Descending) => {
            devices.sort_by_key(|device| {
                let (unread, level) = level_key(device);
                (unread, Reverse(level))
            });
        }
        (Sort::Name, Ascending) => devices.sort_by_key(|device| device.name.to_lowercase()),
        (Sort::Name, Descending) => {
            devices.sort_by_key(|device| Reverse(device.name.to_lowercase()));
        }
        // Freshest first is `Descending`: the natural order for a timestamp
        // is oldest to newest, and freshest first reads against that.
        (Sort::LastSeen, Descending) => devices.sort_by_key(|device| Reverse(device.read_at)),
        (Sort::LastSeen, Ascending) => devices.sort_by_key(|device| device.read_at),
    }

    devices
}

/// A level to sort by, with the absent case pushed to one side so it never
/// competes with a real reading whichever way the rest are ordered.
fn level_key(device: &Device) -> (bool, Option<u8>) {
    let level = device.levels.lowest();

    (level.is_none(), level)
}

#[cfg(test)]
mod tests {
    use blubat_core::{Address, ChargeState, Levels, Source, Timestamp};

    use super::*;

    const READ_AT: i64 = 1_785_643_199;

    fn device(name: &str, address: &str, level: Option<u8>) -> Device {
        Device {
            address: Address::parse(address).expect("valid address"),
            name: name.to_string(),
            kind: None,
            transport: None,
            vendor_id: None,
            product_id: None,
            levels: Levels {
                main: level,
                ..Levels::default()
            },
            charge: ChargeState::Discharging,
            source: Source::IoKit,
            connected: true,
            read_at: Timestamp::from_unix(READ_AT),
        }
    }

    fn offline(name: &str, address: &str, level: Option<u8>) -> Device {
        Device {
            connected: false,
            ..device(name, address, level)
        }
    }

    fn devices() -> Vec<Device> {
        vec![
            device("Magic Trackpad", "30-82-16-f2-24-90", Some(85)),
            device("MX Keys M Mac", "de-df-38-f0-46-9b", Some(9)),
            device("Soundcore Liberty", "d0-03-4b-0b-e6-4e", None),
            offline("AirPods Pro", "74-15-f5-02-8e-38", Some(4)),
        ]
    }

    fn names<'a>(rows: impl Iterator<Item = &'a Device>) -> Vec<String> {
        rows.map(|device| device.name.clone()).collect()
    }

    fn shown(devices: &[Device], view: &View) -> Vec<String> {
        Rows::of(devices, view)
            .all()
            .map(|device| device.name.clone())
            .collect()
    }

    fn filtered(query: &str) -> View {
        View {
            filter: Filter {
                query: query.to_string(),
            },
            ..View::default()
        }
    }

    #[test]
    fn connected_devices_come_before_disconnected_ones() {
        let devices = devices();
        let rows = Rows::of(&devices, &View::default());

        assert_eq!(rows.connected.len(), 3);
        assert_eq!(rows.disconnected.len(), 1);
        assert_eq!(rows.len(), 4);
        assert_eq!(
            rows.get(3).map(|device| device.name.as_str()),
            Some("AirPods Pro"),
            "the disconnected section is selected through last"
        );
        assert_eq!(rows.get(4), None);
    }

    /// A view opened straight on `sort`, at its own natural direction, the
    /// same as choosing it fresh from the sort menu would.
    fn sorted_view(sort: Sort) -> View {
        View {
            sort,
            direction: sort.natural(),
            ..View::default()
        }
    }

    #[test]
    fn each_order_lists_the_devices_its_own_way() {
        let devices = devices();
        let by = |sort| shown(&devices, &sorted_view(sort));

        assert_eq!(
            by(Sort::Level),
            [
                "MX Keys M Mac",
                "Magic Trackpad",
                "Soundcore Liberty",
                "AirPods Pro"
            ],
            "emptiest first, unread last, each section on its own"
        );
        assert_eq!(
            by(Sort::Name),
            [
                "Magic Trackpad",
                "MX Keys M Mac",
                "Soundcore Liberty",
                "AirPods Pro"
            ]
        );
    }

    #[test]
    fn the_freshest_reading_leads_the_last_seen_order() {
        let devices = vec![
            Device {
                read_at: Timestamp::from_unix(READ_AT - 300),
                ..device("Stale", "aa-aa-aa-aa-aa-aa", Some(50))
            },
            device("Fresh", "bb-bb-bb-bb-bb-bb", Some(50)),
        ];

        assert_eq!(
            shown(&devices, &sorted_view(Sort::LastSeen)),
            ["Fresh", "Stale"]
        );
    }

    #[test]
    fn an_order_that_cannot_separate_two_devices_leaves_them_as_they_came() {
        let devices = vec![
            device("Alpha", "aa-aa-aa-aa-aa-aa", Some(50)),
            device("Beta", "bb-bb-bb-bb-bb-bb", Some(50)),
        ];

        for sort in [Sort::Level, Sort::Name, Sort::LastSeen] {
            assert_eq!(
                shown(&devices, &sorted_view(sort)),
                ["Alpha", "Beta"],
                "{sort:?}"
            );
        }
    }

    #[test]
    fn choosing_the_active_column_again_reverses_the_rows() {
        let devices = devices();
        let mut view = sorted_view(Sort::Level);

        assert_eq!(
            shown(&devices, &view),
            [
                "MX Keys M Mac",
                "Magic Trackpad",
                "Soundcore Liberty",
                "AirPods Pro"
            ],
            "emptiest first is where level opens"
        );

        view.choose_sort(Sort::Level);
        assert_eq!(
            shown(&devices, &view),
            [
                "Magic Trackpad",
                "MX Keys M Mac",
                "Soundcore Liberty",
                "AirPods Pro"
            ],
            "the same key again reverses it, the unread device still last"
        );
    }

    #[test]
    fn choosing_a_different_column_resets_it_to_its_own_natural_direction() {
        let mut view = sorted_view(Sort::Level);
        view.choose_sort(Sort::Level);
        assert_eq!(
            view.direction,
            Direction::Descending,
            "level is reversed first"
        );

        view.choose_sort(Sort::Name);
        assert_eq!(view.sort, Sort::Name);
        assert_eq!(
            view.direction,
            Sort::Name.natural(),
            "a different column opens at its own natural order, not level's"
        );
    }

    #[test]
    fn every_column_opens_at_the_direction_that_makes_sense_of_it() {
        assert_eq!(
            Sort::Level.natural(),
            Direction::Ascending,
            "emptiest first"
        );
        assert_eq!(Sort::Name.natural(), Direction::Ascending, "A to Z");
        assert_eq!(
            Sort::LastSeen.natural(),
            Direction::Descending,
            "freshest first"
        );
    }

    #[test]
    fn an_unread_level_stays_last_whichever_way_level_is_read() {
        let devices = vec![
            device("Full", "aa-aa-aa-aa-aa-aa", Some(90)),
            device("Unread", "bb-bb-bb-bb-bb-bb", None),
            device("Empty", "cc-cc-cc-cc-cc-cc", Some(1)),
        ];

        assert_eq!(
            shown(&devices, &sorted_view(Sort::Level)),
            ["Empty", "Full", "Unread"]
        );

        let mut reversed = sorted_view(Sort::Level);
        reversed.choose_sort(Sort::Level);
        assert_eq!(shown(&devices, &reversed), ["Full", "Empty", "Unread"]);
    }

    #[test]
    fn the_filter_matches_a_name_or_an_address() {
        let devices = devices();

        assert_eq!(shown(&devices, &filtered("keys")), ["MX Keys M Mac"]);
        assert_eq!(shown(&devices, &filtered("KEYS")), ["MX Keys M Mac"]);
        assert_eq!(shown(&devices, &filtered("74-15")), ["AirPods Pro"]);
        assert_eq!(shown(&devices, &filtered("74:15")), ["AirPods Pro"]);
        assert!(shown(&devices, &filtered("nothing here")).is_empty());
    }

    #[test]
    fn a_filter_of_whitespace_narrows_nothing() {
        let devices = devices();

        assert!(!filtered("   ").filter.narrows());
        assert_eq!(shown(&devices, &filtered("   ")).len(), 4);
        assert_eq!(shown(&devices, &View::default()).len(), 4);
    }

    #[test]
    fn a_hidden_device_is_gone_until_hidden_devices_are_shown() {
        let devices = devices();
        let mut view = View::default();
        view.toggle_hidden(&devices[0]);

        assert_eq!(shown(&devices, &view).len(), 3);
        assert!(!shown(&devices, &view).contains(&"Magic Trackpad".to_string()));

        view.show_hidden = true;
        assert_eq!(shown(&devices, &view).len(), 4);

        view.toggle_hidden(&devices[0]);
        view.show_hidden = false;
        assert_eq!(shown(&devices, &view).len(), 4, "and hiding is reversible");
    }

    #[test]
    fn a_new_hide_is_written_as_the_address_that_cannot_be_renamed() {
        let devices = devices();
        let mut view = View::default();
        view.toggle_hidden(&devices[0]);

        assert_eq!(view.hidden, ["30-82-16-f2-24-90"]);
    }

    #[test]
    fn the_config_files_matches_are_the_devices_the_dashboard_opens_without() {
        let devices = devices();
        let view = View::hiding(&["MX Keys".to_string()], false);

        assert_eq!(
            shown(&devices, &view),
            ["Magic Trackpad", "Soundcore Liberty", "AirPods Pro"],
            "a name a person wrote hides as readily as an address"
        );
    }

    #[test]
    fn showing_a_device_again_drops_every_match_that_was_hiding_it() {
        let devices = devices();
        let mut view = View::hiding(&["MX Keys".to_string(), "de-df-38".to_string()], false);
        view.toggle_hidden(&devices[1]);

        assert!(view.hidden.is_empty(), "one press, however it was hidden");
        assert_eq!(shown(&devices, &view).len(), 4);
    }

    #[test]
    fn hiding_appends_rather_than_reordering_what_the_file_already_held() {
        let devices = devices();
        let mut view = View::hiding(&["MX Keys".to_string()], false);
        view.toggle_hidden(&devices[0]);

        assert_eq!(view.hidden, ["MX Keys", "30-82-16-f2-24-90"]);
    }

    #[test]
    fn the_dashboard_can_open_with_the_disconnected_section_already_hidden() {
        let devices = devices();
        let view = View::hiding(&[], true);

        assert_eq!(
            shown(&devices, &view),
            ["MX Keys M Mac", "Magic Trackpad", "Soundcore Liberty"],
            "the disconnected device is left off from the first frame"
        );
    }

    #[test]
    fn hiding_the_disconnected_section_drops_it_without_touching_the_connected_one() {
        let devices = devices();
        let mut view = View::default();

        assert_eq!(Rows::of(&devices, &view).disconnected.len(), 1);

        view.hide_inactive = true;
        let rows = Rows::of(&devices, &view);
        assert_eq!(
            rows.connected.len(),
            3,
            "the connected section is untouched"
        );
        assert!(rows.disconnected.is_empty());
        assert_eq!(rows.len(), 3);

        view.hide_inactive = false;
        assert_eq!(
            Rows::of(&devices, &view).disconnected.len(),
            1,
            "and the same key brings it back"
        );
    }

    #[test]
    fn every_order_names_itself_and_level_is_the_default() {
        assert_eq!(Sort::default(), Sort::Level);

        for sort in [Sort::Level, Sort::Name, Sort::LastSeen] {
            assert!(!sort.label().is_empty());
        }
    }

    #[test]
    fn nothing_read_yet_is_no_rows_rather_than_an_absence() {
        let rows = Rows::of(&[], &View::default());

        assert!(rows.is_empty());
        assert_eq!(names(rows.all()), Vec::<String>::new());
    }
}
