use std::fmt;

use serde::{Deserialize, Deserializer, Serialize};

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

/// Reads an address back through the same normalising parse that wrote it.
///
/// The state file keys its devices on this, and a key blubat cannot make sense
/// of is worth rejecting there for the same reason it is worth rejecting from
/// a source: an unparsed address matches no device and would leak state.
impl<'de> Deserialize<'de> for Address {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;

        Address::parse(&raw)
            .ok_or_else(|| serde::de::Error::custom(format!("`{raw}` is not a Bluetooth address")))
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

    #[test]
    fn deserialises_through_the_same_parse_that_wrote_it() {
        let address: Address =
            serde_json::from_str("\"30:82:16:F2:24:90\"").expect("deserialisable");

        assert_eq!(address.as_str(), "30-82-16-f2-24-90");
        assert!(serde_json::from_str::<Address>("\"nonsense\"").is_err());
    }
}
