//! The `[theme]` table: a named built-in scheme with per colour overrides.

use serde::Deserialize;

use crate::error::{Error, Result};

/// A built-in colour scheme, the base any override sits on top of.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Scheme {
    #[default]
    Dark,
    Light,
    /// No colour beyond the terminal's own foreground, for a monochrome setup.
    Mono,
}

/// A colour as the config file writes it: `#rrggbb`.
///
/// Held as its components rather than as the written text so a frontend can
/// hand them straight to whatever colour type it draws with.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(try_from = "String")]
pub struct Rgb {
    pub red: u8,
    pub green: u8,
    pub blue: u8,
}

impl Rgb {
    /// Parses `#39c5cf`, with or without the hash and in either case.
    pub fn parse(text: &str) -> Result<Self> {
        let trimmed = text.trim();
        let digits = trimmed.strip_prefix('#').unwrap_or(trimmed);
        // Every pair is two validated hex digits by the time it is read.
        let octet = |at: usize| u8::from_str_radix(&digits[at..at + 2], 16).unwrap_or_default();

        (digits.len() == 6 && digits.bytes().all(|byte| byte.is_ascii_hexdigit()))
            .then(|| Self {
                red: octet(0),
                green: octet(2),
                blue: octet(4),
            })
            .ok_or_else(|| Error::Format(format!("`{text}` is not a colour such as `#39c5cf`")))
    }
}

impl TryFrom<String> for Rgb {
    type Error = Error;

    fn try_from(text: String) -> Result<Self> {
        Self::parse(&text)
    }
}

/// The colours the dashboard draws with, and the glyph it marks charging with.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct Theme {
    pub scheme: Scheme,
    /// The app name, the selection marker and the keymap.
    pub accent: Option<Rgb>,
    pub critical: Option<Rgb>,
    pub low: Option<Rgb>,
    pub ok: Option<Rgb>,
    /// Replaces the charging mark blubat would otherwise guess at from the
    /// environment, which is the escape hatch when that guess reads wrong.
    pub charging_glyph: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_colour_parses_with_or_without_the_hash_in_either_case() {
        let teal = Rgb {
            red: 0x39,
            green: 0xc5,
            blue: 0xcf,
        };

        assert_eq!(Rgb::parse("#39c5cf").expect("parses"), teal);
        assert_eq!(Rgb::parse("39C5CF").expect("parses"), teal);
        assert_eq!(Rgb::parse("  #39c5cf  ").expect("parses"), teal);
        assert_eq!(
            Rgb::parse("#000000").expect("parses"),
            Rgb {
                red: 0,
                green: 0,
                blue: 0
            }
        );
    }

    #[test]
    fn rejects_anything_that_is_not_six_hex_digits() {
        for text in [
            "",
            "#",
            "#39c5c",
            "#39c5cff",
            "#39c5cg",
            "cyan",
            "#\u{e9}\u{e9}\u{e9}",
        ] {
            assert!(
                matches!(Rgb::parse(text), Err(Error::Format(_))),
                "{text:?} should be rejected"
            );
        }
    }

    #[test]
    fn the_table_defaults_to_the_dark_scheme_with_no_overrides() {
        let theme = Theme::default();

        assert_eq!(theme.scheme, Scheme::Dark);
        assert_eq!(theme.accent, None);
        assert_eq!(theme.charging_glyph, None);
    }

    #[test]
    fn overrides_sit_on_top_of_the_named_scheme() {
        let theme: Theme = toml::from_str(
            r##"
            scheme = "light"
            accent = "#39c5cf"
            charging_glyph = "⚡"
            "##,
        )
        .expect("parses");

        assert_eq!(theme.scheme, Scheme::Light);
        assert_eq!(
            theme.accent,
            Some(Rgb {
                red: 0x39,
                green: 0xc5,
                blue: 0xcf
            })
        );
        assert_eq!(theme.charging_glyph.as_deref(), Some("\u{26a1}"));
        assert_eq!(theme.critical, None, "an unset colour stays the scheme's");
    }

    #[test]
    fn an_unknown_scheme_or_key_is_rejected() {
        assert!(toml::from_str::<Theme>("scheme = \"solarized\"").is_err());
        assert!(toml::from_str::<Theme>("accnt = \"#39c5cf\"").is_err());
        assert!(toml::from_str::<Theme>("accent = \"blue\"").is_err());
    }
}
