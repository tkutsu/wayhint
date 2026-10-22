//! How a tag looks: the `[style]` table.
//!
//! Every field has a default that looks fine untouched, so the file only
//! holds what you change. Opacity is each colour's alpha: a translucent
//! background with solid text is `background = "#1e1e2ef0"`.

use std::fmt::{self, Write as _};

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

/// A colour from `#RRGGBB` or `#RRGGBBAA`.
///
/// `try_from = "String"` makes serde read a string and hand it to our
/// `TryFrom<String>`, so a bad colour fails while the file loads, with our
/// message, and no other code ever sees an unparsed colour. `into` does the
/// same backwards for printing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }
}

impl TryFrom<String> for Color {
    type Error = String;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        let bad = || format!("{text:?} isn't a colour: write #RRGGBB or #RRGGBBAA");
        let hex = text.strip_prefix('#').ok_or_else(bad)?;
        if !matches!(hex.len(), 6 | 8) || !hex.is_ascii() {
            return Err(bad());
        }
        let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).map_err(|_| bad());
        let a = if hex.len() == 8 { byte(6)? } else { 0xff };
        Ok(Self {
            r: byte(0)?,
            g: byte(2)?,
            b: byte(4)?,
            a,
        })
    }
}

impl From<Color> for String {
    fn from(c: Color) -> Self {
        c.to_string()
    }
}

impl fmt::Display for Color {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "#{:02x}{:02x}{:02x}{:02x}",
            self.r, self.g, self.b, self.a
        )
    }
}

/// The `[style]` table. `#[serde(default)]` fills every missing field from
/// `Style::default()`, and `deny_unknown_fields` rejects `colour = ...`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Style {
    /// A fontconfig pattern, as `fc-match` takes it: `"Inter:bold"`.
    pub font: String,
    /// Text size in px.
    pub size: f32,
    pub text: Color,
    /// Letters already typed, dimmed.
    pub matched: Color,
    pub background: Color,
    pub border: Color,
    pub border_width: f32,
    pub radius: f32,
    /// Vertical, then horizontal, in px.
    pub padding: [f32; 2],
}

impl Default for Style {
    fn default() -> Self {
        Self {
            font: "sans-serif:bold".into(),
            size: 13.0,
            text: Color::rgba(0xff, 0xff, 0xff, 0xff),
            matched: Color::rgba(0xff, 0xff, 0xff, 0x66),
            background: Color::rgba(0x1e, 0x1e, 0x2e, 0xf0),
            border: Color::rgba(0xd0, 0xd0, 0xd0, 0xff),
            border_width: 1.0,
            radius: 4.0,
            padding: [2.0, 5.0],
        }
    }
}

impl Style {
    /// Numbers serde can't check by type alone.
    pub fn validate(&self) -> Result<()> {
        if !(4.0..=200.0).contains(&self.size) {
            bail!("[style] size: {} px is outside 4-200", self.size);
        }
        let lengths = [
            ("border_width", self.border_width),
            ("radius", self.radius),
            ("padding", self.padding[0]),
            ("padding", self.padding[1]),
        ];
        for (name, value) in lengths {
            if !(0.0..=100.0).contains(&value) {
                bail!("[style] {name}: {value} px is outside 0-100");
            }
        }
        if self.font.trim().is_empty() {
            bail!("[style] font: empty");
        }
        Ok(())
    }

    /// The table as TOML, for `wayhint config`.
    pub fn render(&self) -> String {
        let mut out = String::from("[style]\n");
        let _ = writeln!(
            out,
            "font = {:?}   # a fontconfig pattern, as fc-match takes it",
            self.font
        );
        let _ = writeln!(out, "size = {}", self.size);
        for (name, color) in [
            ("text", self.text),
            ("matched", self.matched),
            ("background", self.background),
            ("border", self.border),
        ] {
            let _ = writeln!(out, "{name} = \"{color}\"");
        }
        let _ = writeln!(out, "border_width = {}", self.border_width);
        let _ = writeln!(out, "radius = {}", self.radius);
        let _ = writeln!(
            out,
            "padding = [{}, {}]   # vertical, horizontal",
            self.padding[0], self.padding[1]
        );
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colours_parse_with_and_without_alpha() {
        assert_eq!(
            Color::try_from("#1e1e2e".to_string()),
            Ok(Color::rgba(0x1e, 0x1e, 0x2e, 0xff))
        );
        assert_eq!(
            Color::try_from("#ffffff66".to_string()),
            Ok(Color::rgba(255, 255, 255, 0x66))
        );
        assert!(Color::try_from("1e1e2e".to_string()).is_err());
        assert!(Color::try_from("#12345".to_string()).is_err());
        assert!(Color::try_from("#gggggg".to_string()).is_err());
        assert!(
            Color::try_from("#ééé".to_string()).is_err(),
            "non-ASCII never slices mid-character"
        );
    }

    #[test]
    fn render_round_trips() {
        #[derive(Deserialize)]
        struct File {
            style: Style,
        }
        let style = Style {
            size: 20.0,
            ..Style::default()
        };
        let text = style.render();
        assert_eq!(toml::from_str::<File>(&text).unwrap().style, style);
    }

    #[test]
    fn out_of_range_numbers_are_rejected() {
        assert!(
            Style {
                size: 0.0,
                ..Style::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            Style {
                radius: -1.0,
                ..Style::default()
            }
            .validate()
            .is_err()
        );
        assert!(Style::default().validate().is_ok());
    }
}
