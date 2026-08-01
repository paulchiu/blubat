//! The glyphs the dashboard draws with, and the guess behind the default.

/// The marks the dashboard draws that not every font can render.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Glyphs {
    /// Prefixes the charging state in the table.
    pub charging: &'static str,
}

impl Glyphs {
    /// What every terminal can draw, and what blubat falls back to.
    pub const ASCII: Self = Self { charging: "+" };

    /// The Nerd Fonts bolt, single width so it cannot shift a column.
    pub const NERD_FONT: Self = Self {
        charging: "\u{f0e7}",
    };

    /// The glyphs to draw with, guessed from the environment.
    pub fn detected() -> Self {
        Self::from_env(|name| std::env::var(name).ok())
    }

    /// Guesses whether the terminal can draw the Nerd Fonts private use area.
    ///
    /// Best effort and deliberately shy: no terminal reports the font it draws
    /// with, so this reads the variables some terminals and shells set and
    /// answers ascii whenever it is not sure, since a wrong yes prints tofu
    /// while a wrong no only prints a plainer glyph. A variable that exists to
    /// answer this question is taken at its word either way, which is the
    /// escape hatch for a terminal the guess reads wrong.
    fn from_env(var: impl Fn(&str) -> Option<String>) -> Self {
        const ANSWERS: [&str; 2] = ["BLUBAT_NERD_FONT", "NERD_FONT"];
        const FONTS: [&str; 3] = ["TERM_PROGRAM", "TERMINAL_FONT", "FONT"];

        let answered = ANSWERS.iter().find_map(|name| var(name));
        if let Some(answer) = answered {
            return Self::of(matches!(answer.trim(), "1" | "true" | "yes"));
        }

        Self::of(
            FONTS
                .iter()
                .filter_map(|name| var(name))
                .any(|value| value.to_lowercase().contains("nerd")),
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
    }

    #[test]
    fn the_bolt_is_one_cell_so_it_cannot_shift_a_column() {
        assert_eq!(Glyphs::NERD_FONT.charging.chars().count(), 1);
        assert_eq!(Glyphs::ASCII.charging.chars().count(), 1);
    }
}
