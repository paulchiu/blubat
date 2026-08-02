//! The glyphs the dashboard draws with, and the guess behind the default.

use std::borrow::Cow;

/// The marks the dashboard draws that not every font can render.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Glyphs {
    /// Prefixes the charging state in the table.
    pub charging: Cow<'static, str>,
    /// Marks a row `H` is showing that would otherwise be hidden.
    pub hidden: Cow<'static, str>,
}

impl Glyphs {
    /// What every terminal can draw, and what blubat falls back to.
    ///
    /// `hidden` is three cells here against the Nerd Font glyph's one: unlike
    /// `charging`, it is drawn inside the Name cell's own clip rather than a
    /// shared fixed-width gutter, so nothing depends on the two matching.
    pub const ASCII: Self = Self {
        charging: Cow::Borrowed("+"),
        hidden: Cow::Borrowed("[h]"),
    };

    /// The Nerd Fonts bolt and eye-slash, both single width so a switch
    /// between the two glyph sets cannot shift the charging column, which is
    /// a shared gutter. `hidden` carries no such guarantee; see `ASCII`.
    pub const NERD_FONT: Self = Self {
        charging: Cow::Borrowed("\u{f0e7}"),
        hidden: Cow::Borrowed("\u{f070}"),
    };

    /// The glyphs to draw with, guessed from the environment.
    pub fn detected() -> Self {
        Self::from_env(|name| std::env::var(name).ok())
    }

    /// The charging mark the config named, in place of the guessed one.
    ///
    /// The guess is best effort and the file is not, so anything written wins.
    /// Blank is nothing written: a mark of no characters would leave the state
    /// column reading as a stray space.
    pub fn overridden(self, charging: Option<&str>) -> Self {
        match charging.map(str::trim).filter(|glyph| !glyph.is_empty()) {
            Some(glyph) => Self {
                charging: Cow::Owned(glyph.to_string()),
                ..self
            },
            None => self,
        }
    }

    /// Guesses whether the terminal can draw the Nerd Fonts private use area.
    ///
    /// Best effort and deliberately shy: no terminal reports the font it draws
    /// with, so this reads the variables some terminals and shells set and
    /// answers ascii whenever it is not sure, since a wrong yes prints tofu
    /// while a wrong no only prints a plainer glyph. Only blubat's own variable
    /// is an answer, taken at its word either way, which is the escape hatch
    /// for a terminal the guess reads wrong. Every other name is a hint: they
    /// belong to whoever set them, and `NERD_FONT` holding a font name is as
    /// likely as it holding a yes.
    fn from_env(var: impl Fn(&str) -> Option<String>) -> Self {
        const ANSWER: &str = "BLUBAT_NERD_FONT";
        const FONTS: [&str; 4] = ["NERD_FONT", "TERM_PROGRAM", "TERMINAL_FONT", "FONT"];

        if let Some(answer) = var(ANSWER) {
            return Self::of(said_yes(&answer));
        }

        Self::of(
            FONTS
                .iter()
                .filter_map(|name| var(name))
                .any(|value| said_yes(&value) || value.to_lowercase().contains("nerd")),
        )
    }

    fn of(nerd_font: bool) -> Self {
        if nerd_font {
            Self::NERD_FONT
        } else {
            Self::ASCII
        }
    }
}

impl Default for Glyphs {
    /// What every terminal can draw, since a wrong yes prints tofu.
    fn default() -> Self {
        Self::ASCII
    }
}

/// Whether a variable's value reads as yes rather than as a font name.
fn said_yes(value: &str) -> bool {
    matches!(value.trim().to_lowercase().as_str(), "1" | "true" | "yes")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An environment holding exactly the variables a case sets.
    fn env(set: Vec<(&'static str, &'static str)>) -> impl Fn(&str) -> Option<String> {
        move |name| {
            set.iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_string())
        }
    }

    #[test]
    fn nothing_known_about_the_font_means_ascii() {
        assert_eq!(Glyphs::from_env(env(Vec::new())), Glyphs::ASCII);
        assert_eq!(
            Glyphs::from_env(env(vec![("TERM_PROGRAM", "Apple_Terminal")])),
            Glyphs::ASCII
        );
    }

    #[test]
    fn a_font_naming_itself_nerd_is_taken_at_its_word() {
        for hint in [
            ("TERMINAL_FONT", "JetBrainsMono Nerd Font Mono"),
            ("FONT", "hack nerd font"),
            ("NERD_FONT", "1"),
            ("NERD_FONT", "JetBrainsMono Nerd Font"),
            ("TERM_PROGRAM", "WezTerm-NerdFont"),
        ] {
            assert_eq!(
                Glyphs::from_env(env(vec![hint])),
                Glyphs::NERD_FONT,
                "{hint:?}"
            );
        }
    }

    #[test]
    fn the_override_settles_it_in_both_directions() {
        assert_eq!(
            Glyphs::from_env(env(vec![("BLUBAT_NERD_FONT", "1")])),
            Glyphs::NERD_FONT
        );
        assert_eq!(
            Glyphs::from_env(env(vec![
                ("BLUBAT_NERD_FONT", "0"),
                ("FONT", "Hack Nerd Font Mono"),
            ])),
            Glyphs::ASCII,
            "an explicit no beats a hint"
        );
        assert_eq!(
            Glyphs::from_env(env(vec![("NERD_FONT", "0")])),
            Glyphs::ASCII,
            "a hint that is not blubat's own says no by saying nothing"
        );
    }

    #[test]
    fn the_bolt_is_one_cell_so_it_cannot_shift_a_column() {
        assert_eq!(Glyphs::NERD_FONT.charging.chars().count(), 1);
        assert_eq!(Glyphs::ASCII.charging.chars().count(), 1);
    }

    #[test]
    fn the_eye_slash_is_one_cell_the_same_way() {
        assert_eq!(Glyphs::NERD_FONT.hidden.chars().count(), 1);
    }

    #[test]
    fn the_ascii_hidden_marker_is_wider_which_is_fine_inside_the_name_cells_own_clip() {
        assert_eq!(Glyphs::ASCII.hidden.chars().count(), 3);
    }

    #[test]
    fn a_configured_mark_wins_over_whatever_was_guessed() {
        assert_eq!(
            Glyphs::NERD_FONT.overridden(Some("^")).charging,
            "^",
            "the escape hatch for a terminal the guess reads wrong"
        );
        assert_eq!(
            Glyphs::ASCII.overridden(Some(" \u{26a1} ")).charging,
            "\u{26a1}"
        );
    }

    #[test]
    fn nothing_written_leaves_the_guess_standing() {
        for written in [None, Some(""), Some("   ")] {
            assert_eq!(
                Glyphs::NERD_FONT.overridden(written),
                Glyphs::NERD_FONT,
                "{written:?}"
            );
        }
    }
}
