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
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct View {
    pub sort: Sort,
    pub filter: Filter,
    /// `[dashboard] hidden` as the config file holds it, in file order: the
    /// same substring matches `--device` takes, and what `h` writes back.
    pub hidden: Vec<String>,
    pub show_hidden: bool,
    /// Whether the disconnected section is left off the table, which `i`
    /// toggles and, like `h` toggling `hidden`, writes back to the config file.
    pub hide_inactive: bool,
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
    pub active: Vec<&'a Device>,
    pub inactive: Vec<&'a Device>,
}

impl<'a> Rows<'a> {
    /// The devices `view` shows out of `devices`, in its order.
    ///
    /// `hide_inactive` drops the whole section rather than filtering it out
    /// device by device, so a device hidden this way is still connected as
    /// far as the status line and the alert count are concerned.
    pub fn of(devices: &'a [Device], view: &View) -> Self {
        let (active, inactive) = devices
            .iter()
            .filter(|device| view.shows(device))
            .partition::<Vec<_>, _>(|device| device.connected);

        Self {
            active: sorted(active, view.sort),
            inactive: if view.hide_inactive {
                Vec::new()
            } else {
                sorted(inactive, view.sort)
            },
        }
    }

    /// Every row, in the order the selection moves through them.
    pub fn all(&self) -> impl Iterator<Item = &'a Device> {
        self.active.iter().chain(&self.inactive).copied()
    }

    pub fn len(&self) -> usize {
        self.active.len() + self.inactive.len()
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
fn sorted(mut devices: Vec<&Device>, sort: Sort) -> Vec<&Device> {
    match sort {
        // An unread level sorts last: it is not a full battery, and it is not
        // an empty one either.
        Sort::Level => devices.sort_by_key(|device| {
            let level = device.levels.lowest();
            (level.is_none(), level)
        }),
        Sort::Name => devices.sort_by_key(|device| device.name.to_lowercase()),
        Sort::LastSeen => devices.sort_by_key(|device| std::cmp::Reverse(device.read_at)),
    }

    devices
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

        assert_eq!(rows.active.len(), 3);
        assert_eq!(rows.inactive.len(), 1);
        assert_eq!(rows.len(), 4);
        assert_eq!(
            rows.get(3).map(|device| device.name.as_str()),
            Some("AirPods Pro"),
            "the inactive section is selected through last"
        );
        assert_eq!(rows.get(4), None);
    }

    #[test]
    fn each_order_lists_the_devices_its_own_way() {
        let devices = devices();
        let by = |sort| {
            shown(
                &devices,
                &View {
                    sort,
                    ..View::default()
                },
            )
        };

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
            shown(
                &devices,
                &View {
                    sort: Sort::LastSeen,
                    ..View::default()
                }
            ),
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
                shown(
                    &devices,
                    &View {
                        sort,
                        ..View::default()
                    }
                ),
                ["Alpha", "Beta"],
                "{sort:?}"
            );
        }
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
    fn the_dashboard_can_open_with_the_inactive_section_already_hidden() {
        let devices = devices();
        let view = View::hiding(&[], true);

        assert_eq!(
            shown(&devices, &view),
            ["MX Keys M Mac", "Magic Trackpad", "Soundcore Liberty"],
            "the disconnected device is left off from the first frame"
        );
    }

    #[test]
    fn hiding_the_inactive_section_drops_it_without_touching_the_active_one() {
        let devices = devices();
        let mut view = View::default();

        assert_eq!(Rows::of(&devices, &view).inactive.len(), 1);

        view.hide_inactive = true;
        let rows = Rows::of(&devices, &view);
        assert_eq!(rows.active.len(), 3, "the active section is untouched");
        assert!(rows.inactive.is_empty());
        assert_eq!(rows.len(), 3);

        view.hide_inactive = false;
        assert_eq!(
            Rows::of(&devices, &view).inactive.len(),
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
