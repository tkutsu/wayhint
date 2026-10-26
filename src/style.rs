//! How a tag looks: the `[style]` table.
//!
//! Every field has a default that looks fine untouched, so the file only
//! holds what you change. Opacity is each colour's alpha: a translucent
//! background with solid text is `background = "#1e1e2ef0"`.

use std::fmt::{self, Write as _};

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

use crate::geometry::{Point, Rect};

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

/// Where a tag sits on its target: `[style] position`. The tag's own
/// matching point goes on the target's, so `BottomRight` puts the tag in
/// the box's bottom-right corner, inside it if it fits.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Position {
    TopLeft,
    Top,
    TopRight,
    Left,
    Center,
    Right,
    BottomLeft,
    Bottom,
    #[default]
    BottomRight,
}

impl Position {
    /// How far across and down a box this point is, in halves: 0 is the
    /// left or top edge, 1 the middle, 2 the right or bottom edge.
    pub const fn halves(self) -> (i32, i32) {
        match self {
            Self::TopLeft => (0, 0),
            Self::Top => (1, 0),
            Self::TopRight => (2, 0),
            Self::Left => (0, 1),
            Self::Center => (1, 1),
            Self::Right => (2, 1),
            Self::BottomLeft => (0, 2),
            Self::Bottom => (1, 2),
            Self::BottomRight => (2, 2),
        }
    }

    /// This point on `rect`.
    pub const fn on(self, rect: Rect) -> Point {
        let (across, down) = self.halves();
        Point::new(rect.x + rect.w * across / 2, rect.y + rect.h * down / 2)
    }

    /// As the config writes it: `"bottom-right"`.
    const fn name(self) -> &'static str {
        match self {
            Self::TopLeft => "top-left",
            Self::Top => "top",
            Self::TopRight => "top-right",
            Self::Left => "left",
            Self::Center => "center",
            Self::Right => "right",
            Self::BottomLeft => "bottom-left",
            Self::Bottom => "bottom",
            Self::BottomRight => "bottom-right",
        }
    }
}

/// The farthest `[style] offset` moves a tag, either way, in px.
const OFFSET: i32 = 500;

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
    /// Letters already typed, an armed verb's name, a placeholder. Once
    /// called `matched`, which a file may still say.
    #[serde(alias = "matched")]
    pub dimmed: Color,
    pub background: Color,
    pub border: Color,
    pub border_width: f32,
    pub radius: f32,
    /// Vertical, then horizontal, in px.
    pub padding: [f32; 2],
    pub position: Position,
    /// Moves every tag from its position, right then down, in px.
    pub offset: [i32; 2],
    /// The line from a tag to its dot, and the dot: `[style.leader]`.
    pub leader: Leader,
}

/// `[style.leader]`: the line from a tag to where it clicks, and the dot
/// there, drawn when an arrow moves a click or a tag had to move off its
/// target. Each is a stroke inside an outline, so it shows on light and
/// dark windows alike. An unset colour is the tags' own: the letters' for
/// the line and dot, the background's for the outline.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Leader {
    pub line: Option<Color>,
    pub dot: Option<Color>,
    pub outline: Option<Color>,
    /// The line's width, in px.
    pub width: f32,
    /// The dot's radius, in px.
    pub radius: f32,
    /// The outline's width, in px: all of it around the dot, half on each
    /// side of the line. 0 for none.
    pub outline_width: f32,
}

impl Default for Leader {
    fn default() -> Self {
        Self {
            line: None,
            dot: None,
            outline: None,
            width: 1.0,
            radius: 3.0,
            outline_width: 2.0,
        }
    }
}

impl Leader {
    /// The line's, the dot's and the outline's colours, unset ones taken
    /// from the tags' `style`.
    pub fn colors(&self, style: &Style) -> (Color, Color, Color) {
        (
            self.line.unwrap_or(style.text),
            self.dot.unwrap_or(style.text),
            self.outline.unwrap_or(style.background),
        )
    }

    fn validate(&self) -> Result<()> {
        for (name, value) in [
            ("width", self.width),
            ("radius", self.radius),
            ("outline_width", self.outline_width),
        ] {
            if !(0.0..=50.0).contains(&value) {
                bail!("[style.leader] {name}: {value} px is outside 0-50");
            }
        }
        Ok(())
    }

    /// The table as TOML, for `wayhint config`. An unset colour is written
    /// commented out, as the tags' colour it is now, so the file still
    /// follows the tags.
    fn render(&self, style: &Style) -> String {
        let (line, dot, outline) = self.colors(style);
        let mut out =
            String::from("\n[style.leader]   # the line from a tag to its dot, and the dot\n");
        for (name, set, now, from) in [
            ("line", self.line, line, "text"),
            ("dot", self.dot, dot, "text"),
            ("outline", self.outline, outline, "background"),
        ] {
            match set {
                Some(color) => {
                    let _ = writeln!(out, "{name} = \"{color}\"");
                }
                None => {
                    let _ = writeln!(out, "# {name} = \"{now}\"   # unset: the tags' {from}");
                }
            }
        }
        let _ = writeln!(out, "width = {}   # the line's, in px", self.width);
        let _ = writeln!(out, "radius = {}   # the dot's, in px", self.radius);
        let _ = writeln!(
            out,
            "outline_width = {}   # px, around the dot and across the line; 0 for none",
            self.outline_width
        );
        out
    }
}

impl Default for Style {
    fn default() -> Self {
        Self {
            font: "sans-serif:bold".into(),
            size: 13.0,
            text: Color::rgba(0xff, 0xff, 0xff, 0xff),
            dimmed: Color::rgba(0xff, 0xff, 0xff, 0x66),
            background: Color::rgba(0x1e, 0x1e, 0x2e, 0xf0),
            border: Color::rgba(0xd0, 0xd0, 0xd0, 0xff),
            border_width: 1.0,
            radius: 4.0,
            padding: [2.0, 5.0],
            position: Position::default(),
            offset: [0, 0],
            leader: Leader::default(),
        }
    }
}

impl Style {
    /// Numbers serde can't check by type alone.
    pub fn validate(&self) -> Result<()> {
        self.validate_table("style")
    }

    /// Validate a tag or input style, naming the table that supplied it.
    pub fn validate_table(&self, table: &str) -> Result<()> {
        if !(4.0..=200.0).contains(&self.size) {
            bail!("[{table}] size: {} px is outside 4-200", self.size);
        }
        let lengths = [
            ("border_width", self.border_width),
            ("radius", self.radius),
            ("padding", self.padding[0]),
            ("padding", self.padding[1]),
        ];
        for (name, value) in lengths {
            if !(0.0..=100.0).contains(&value) {
                bail!("[{table}] {name}: {value} px is outside 0-100");
            }
        }
        for value in self.offset {
            if !(-OFFSET..=OFFSET).contains(&value) {
                bail!("[{table}] offset: {value} px is outside -{OFFSET} to {OFFSET}");
            }
        }
        if self.font.trim().is_empty() {
            bail!("[{table}] font: empty");
        }
        self.leader.validate()
    }

    /// The table as TOML, `[style.leader]` too, for `wayhint config`.
    pub fn render(&self) -> String {
        self.render_table("style", "on the target") + &self.leader.render(self)
    }

    /// A complete look under its owning table, for `wayhint config`.
    fn render_table(&self, table: &str, anchor: &str) -> String {
        let mut out = format!("[{table}]\n");
        let _ = writeln!(
            out,
            "font = {}   # a fontconfig pattern, as fc-match takes it",
            crate::config::quote(&self.font)
        );
        let _ = writeln!(out, "size = {}", self.size);
        for (name, color, what) in [
            ("text", self.text, ""),
            (
                "dimmed",
                self.dimmed,
                "   # typed letters, a verb's name, a placeholder",
            ),
            ("background", self.background, ""),
            ("border", self.border, ""),
        ] {
            let _ = writeln!(out, "{name} = \"{color}\"{what}");
        }
        let _ = writeln!(out, "border_width = {}", self.border_width);
        let _ = writeln!(out, "radius = {}", self.radius);
        let _ = writeln!(
            out,
            "padding = [{}, {}]   # vertical, horizontal",
            self.padding[0], self.padding[1]
        );
        let _ = writeln!(
            out,
            "position = \"{}\"   # {anchor}: center, top, bottom, left, right, or a corner as top-left",
            self.position.name()
        );
        let _ = writeln!(
            out,
            "offset = [{}, {}]   # px from there, right and down",
            self.offset[0], self.offset[1]
        );
        out
    }
}

/// The typed query's decorations and look, resolved from `[text.input]`.
#[derive(Debug, Clone, PartialEq)]
pub struct Input {
    pub prefix: String,
    pub suffix: String,
    /// Shown, dimmed, while nothing is typed.
    pub placeholder: String,
    pub style: Style,
}

impl Input {
    /// Keep the tags' colours and font, centred near the screen's bottom.
    pub fn new(tags: &Style) -> Self {
        Self {
            prefix: String::new(),
            suffix: String::new(),
            placeholder: "text".into(),
            style: Style {
                position: Position::Bottom,
                offset: [0, -48],
                ..tags.clone()
            },
        }
    }

    /// The resolved query settings as editable TOML.
    pub fn render(&self) -> String {
        let look = self.style.render_table("text.input", "on the screen");
        let (header, fields) = look.split_once('\n').unwrap();
        let quote = crate::config::quote;
        format!(
            "\n{header}   # the query you type, apart from the tags\n\
             placeholder = {}   # shown while nothing is typed\n\
             prefix = {}   # before the query, as \"find: \"\n\
             suffix = {}   # after the query, before the match count\n{fields}",
            quote(&self.placeholder),
            quote(&self.prefix),
            quote(&self.suffix),
        )
    }
}

/// How text mode marks a match: a translucent box over its text, which
/// shows through. `[text.matches]` for every match, `[text.matches.current]`
/// for the selected one.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Mark {
    pub background: Color,
    pub border: Color,
    pub border_width: f32,
    pub radius: f32,
    /// Vertical, then horizontal, in px, around the text's box.
    pub padding: [f32; 2],
}

/// Every match's mark, and the current one's.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Marks {
    pub all: Mark,
    pub current: Mark,
}

impl Marks {
    /// The marks a file with no `[text.matches]` gets.
    #[cfg(test)]
    pub const fn new(style: &Style) -> Self {
        let all = Mark::new(style);
        Self {
            all,
            current: all.for_current(),
        }
    }

    /// Both tables as TOML, each after a blank line, for `wayhint config`.
    pub fn render(&self) -> String {
        format!(
            "\n{}\n{}",
            self.all.render("text.matches"),
            self.current.render("text.matches.current")
        )
    }
}

impl Mark {
    /// The default for every match: neon yellow, half see-through, with no
    /// border, and the tags' radius and padding.
    pub const fn new(style: &Style) -> Self {
        Self {
            background: Color::rgba(0xff, 0xff, 0x00, 0x80),
            border: style.border,
            border_width: 0.0,
            radius: style.radius,
            padding: style.padding,
        }
    }

    /// The default for the current match, from every match's: the same in
    /// orange, as browsers mark the current match of a search.
    pub const fn for_current(self) -> Self {
        Self {
            background: Color::rgba(0xff, 0x96, 0x32, 0x80),
            ..self
        }
    }

    /// The lengths serde can't check by type alone; `table` names where
    /// they came from.
    pub fn validate(&self, table: &str) -> Result<()> {
        let lengths = [
            ("border_width", self.border_width),
            ("radius", self.radius),
            ("padding", self.padding[0]),
            ("padding", self.padding[1]),
        ];
        for (name, value) in lengths {
            if !(0.0..=100.0).contains(&value) {
                bail!("[{table}] {name}: {value} px is outside 0-100");
            }
        }
        Ok(())
    }

    /// The table as TOML, under `[table]`, for `wayhint config`.
    pub fn render(&self, table: &str) -> String {
        let mut out = format!("[{table}]\n");
        let _ = writeln!(out, "background = \"{}\"", self.background);
        let _ = writeln!(out, "border = \"{}\"", self.border);
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
    fn matches_are_yellow_and_the_current_one_orange_by_default() {
        let all = Mark::new(&Style::default());
        assert_eq!(all.background, Color::rgba(0xff, 0xff, 0x00, 0x80));
        assert!(all.border_width.abs() < f32::EPSILON, "no border");
        let current = all.for_current();
        assert_eq!(current.background, Color::rgba(0xff, 0x96, 0x32, 0x80));
        assert_eq!(
            current,
            Mark {
                background: current.background,
                ..all
            }
        );
    }

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
            position: Position::TopLeft,
            offset: [-3, 4],
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
        assert!(
            Style {
                offset: [0, 501],
                ..Style::default()
            }
            .validate()
            .is_err()
        );
        assert!(Style::default().validate().is_ok());
    }

    #[test]
    fn positions_name_points_on_a_box() {
        let rect = Rect::new(10, 20, 40, 30);
        assert_eq!(Position::TopLeft.on(rect), Point::new(10, 20));
        assert_eq!(Position::Center.on(rect), rect.center());
        assert_eq!(Position::Right.on(rect), Point::new(50, 35));
        assert_eq!(Position::BottomRight.on(rect), Point::new(50, 50));
    }
}
