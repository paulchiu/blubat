use std::fmt;

use serde::Serialize;

/// A Bluetooth address normalised to lowercase hex octets joined by hyphens.
///
/// The two sources spell the same device differently: IOKit reports
/// `30-82-16-f2-24-90` and `system_profiler` reports `30:82:16:F2:24:90`. The
/// merge keys on this type so one device stays one record.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
pub struct Address(String);

impl Address {
    /// Normalises six colon or hyphen separated hex octets, rejecting anything else.
    pub fn parse(raw: &str) -> Option<Self> {
        let octets: Vec<&str> = raw.trim().split([':', '-']).collect();
        let usable = octets.len() == 6
            && octets
                .iter()
                .all(|octet| octet.len() == 2 && octet.bytes().all(|b| b.is_ascii_hexdigit()));

        usable.then(|| Address(octets.join("-").to_ascii_lowercase()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalises_separator_and_case() {
        let colons = Address::parse("30:82:16:F2:24:90").expect("valid address");
        let hyphens = Address::parse("30-82-16-f2-24-90").expect("valid address");

        assert_eq!(colons.as_str(), "30-82-16-f2-24-90");
        assert_eq!(colons, hyphens);
    }

    #[test]
    fn accepts_mixed_separators_and_surrounding_space() {
        let address = Address::parse("  AA:BB-CC:DD-EE:FF ").expect("valid address");

        assert_eq!(address.as_str(), "aa-bb-cc-dd-ee-ff");
    }

    #[test]
    fn rejects_anything_that_is_not_six_hex_octets() {
        for raw in [
            "",
            "not-an-address",
            "30:82:16:F2:24",
            "30:82:16:F2:24:90:11",
            "30:82:16:F2:24:9",
            "30:82:16:F2:24:9G",
            "308216f22490",
        ] {
            assert!(Address::parse(raw).is_none(), "{raw} should be rejected");
        }
    }

    #[test]
    fn serialises_as_the_normalised_string() {
        let address = Address::parse("30:82:16:F2:24:90").expect("valid address");

        assert_eq!(
            serde_json::to_string(&address).expect("serialisable"),
            "\"30-82-16-f2-24-90\""
        );
    }
}
