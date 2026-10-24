//! Color themes: the same sixteen-color palettes as tuigram and tuimeta
//! (`themes/` in the repository), mapped to what tuitube paints by role, so
//! drawing code never names a palette color directly.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use ratatui::style::Color;
use serde::Deserialize;

/// The theme used when the settings name none, or one that isn't built in.
pub const DEFAULT: &str = "mocha";

/// The two colors drawn that aren't the theme's, as exact colors rather
/// than the terminal's named ones, whose shade its own palette picks (the
/// animations, `ui::effects`, blend from what a cell shows): white on a
/// length badge or the logo's red, and black behind Pepper's picture.
pub const WHITE: Color = Color::Rgb(255, 255, 255);
pub const BLACK: Color = Color::Rgb(0, 0, 0);

/// The built-in themes by name, lightest Catppuccin flavor first.
pub const BUILT_IN: [(&str, &str); 9] = [
    ("latte", include_str!("../themes/latte.toml")),
    ("frappe", include_str!("../themes/frappe.toml")),
    ("macchiato", include_str!("../themes/macchiato.toml")),
    ("mocha", include_str!("../themes/mocha.toml")),
    ("tokyonight", include_str!("../themes/tokyonight.toml")),
    ("dracula", include_str!("../themes/dracula.toml")),
    ("gruvbox", include_str!("../themes/gruvbox.toml")),
    ("nord", include_str!("../themes/nord.toml")),
    ("rose-pine", include_str!("../themes/rose-pine.toml")),
];

#[derive(Deserialize)]
struct ThemeFile {
    name: Option<String>,
    #[serde(default)]
    palette: BTreeMap<String, String>,
}

/// What the UI paints, by role.
#[derive(Clone, Debug, PartialEq)]
pub struct Colors {
    pub name: String,
    /// The window's background.
    pub bg: Color,
    /// The sidebar and popups: a shade off the background.
    pub panel: Color,
    /// The selected row or card.
    pub selected: Color,
    /// Borders of what doesn't have the keys.
    pub border: Color,
    /// Key hints, placeholders, separators.
    pub dim: Color,
    /// Channel names, view counts, descriptions.
    pub subtext: Color,
    pub text: Color,
    /// Errors, and YouTube's red: the logo, the watched part of a video.
    pub red: Color,
    pub warning: Color,
    pub ok: Color,
    /// What has the keys: the selected card's ring, the cursor.
    pub accent: Color,
    /// Live videos.
    pub live: Color,
    /// SponsorBlock's segments on the seek bar, by category
    /// ([`SEGMENT_COLORS`]).
    segments: Vec<(&'static str, Color)>,
}

/// The palette color each SponsorBlock category is drawn in on the seek
/// bar: its browser extension's colors, as near as the palette has them
/// (sponsors green, self-promotion yellow, intros cyan, outros blue).
const SEGMENT_COLORS: [(&str, &str); 9] = [
    ("sponsor", "green"),
    ("selfpromo", "yellow"),
    ("interaction", "pink"),
    ("intro", "cyan"),
    ("outro", "blue"),
    ("preview", "cyan"),
    ("hook", "blue"),
    ("music_offtopic", "orange"),
    ("filler", "purple"),
];

fn hex(s: &str) -> Option<Color> {
    let s = s.strip_prefix('#')?;
    if s.len() != 6 {
        return None;
    }
    let n = u32::from_str_radix(s, 16).ok()?;
    Some(Color::Rgb((n >> 16) as u8, (n >> 8) as u8, n as u8))
}

impl Colors {
    /// The built-in theme called `id`, else the default one.
    pub fn named(id: &str) -> Self {
        let text = BUILT_IN
            .iter()
            .find(|(name, _)| *name == id)
            .or_else(|| BUILT_IN.iter().find(|(name, _)| *name == DEFAULT))
            .map(|(_, text)| *text)
            .expect("the default theme is built in");
        Self::parse(text).expect("built-in themes parse")
    }

    fn parse(text: &str) -> Option<Self> {
        let file: ThemeFile = toml::from_str(text).ok()?;
        let p = |name: &str| file.palette.get(name).and_then(|c| hex(c));
        Some(Self {
            name: file.name.unwrap_or_default(),
            bg: p("bg")?,
            panel: p("bg_alt")?,
            selected: p("surface")?,
            border: p("overlay")?,
            dim: p("comment")?,
            subtext: p("subtext")?,
            text: p("fg")?,
            red: p("red")?,
            warning: p("yellow")?,
            ok: p("green")?,
            accent: p("accent")?,
            live: p("orange")?,
            segments: SEGMENT_COLORS
                .iter()
                .map(|&(category, name)| Some((category, p(name)?)))
                .collect::<Option<_>>()?,
        })
    }

    /// The color SponsorBlock's `category` is drawn in on the seek bar.
    pub fn segment(&self, category: &str) -> Color {
        self.segments
            .iter()
            .find(|(c, _)| *c == category)
            .map_or(self.ok, |&(_, color)| color)
    }
}

/// The built-in themes' ids and names, in [`BUILT_IN`]'s order: the
/// settings list them.
pub fn names() -> &'static [(&'static str, String)] {
    static NAMES: LazyLock<Vec<(&'static str, String)>> = LazyLock::new(|| {
        BUILT_IN
            .iter()
            .map(|(id, text)| (*id, Colors::parse(text).map(|c| c.name).unwrap_or_default()))
            .collect()
    });
    &NAMES
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_built_in_theme_has_the_whole_palette() {
        for (id, text) in BUILT_IN {
            assert!(Colors::parse(text).is_some(), "{id}");
        }
        assert_eq!(Colors::named("nope"), Colors::named(DEFAULT));
        assert_eq!(names().len(), BUILT_IN.len());
        assert!(names().iter().all(|(_, name)| !name.is_empty()));
    }

    #[test]
    fn every_sponsorblock_category_has_a_color_of_its_own_kind() {
        let c = Colors::named(DEFAULT);
        for (category, _) in crate::sponsorblock::CATEGORIES {
            assert!(
                SEGMENT_COLORS.iter().any(|(c, _)| *c == category),
                "{category}"
            );
            assert_ne!(c.segment(category), c.red, "red is what played");
        }
        assert_eq!(c.segment("sponsor"), c.ok, "green, as SponsorBlock's");
        assert_eq!(c.segment("selfpromo"), c.warning, "yellow");
    }
}
