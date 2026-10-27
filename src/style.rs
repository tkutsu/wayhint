//! How a tag looks: the `[hints]` table.
//!
//! Every field has a default that looks fine untouched, so the file only
//! holds what you change. Opacity is each colour's alpha: a translucent
//! background with solid text is `background = "#1e1e2ef0"`.

use std::{
    fmt::{self, Write as _},
    time::Duration,
};

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

use crate::geometry::{Point, Rect};

/// A colour from `#RGB`, `#RGBA`, `#RRGGBB` or `#RRGGBBAA`.
///
/// `try_from = "String"` makes serde read a string and hand it to our
/// `TryFrom<String>`, so a bad colour fails while the file loads, with our
/// message, and no other code ever sees an unparsed colour. `into` does the
/// same backwards for printing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
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
        let bad = || format!("{text:?} isn't a colour: write #RGB, #RGBA, #RRGGBB or #RRGGBBAA");
        let hex = text.strip_prefix('#').ok_or_else(bad)?;
        if !hex.bytes().all(|c| c.is_ascii_hexdigit()) {
            return Err(bad());
        }
        // A channel written with one digit is that digit twice: #f80 is #ff8800.
        let (digits, scale) = match hex.len() {
            3 | 4 => (1, 0x11),
            6 | 8 => (2, 1),
            _ => return Err(bad()),
        };
        let channel = |i: usize| {
            u8::from_str_radix(&hex[i * digits..(i + 1) * digits], 16)
                .map(|value| value * scale)
                .map_err(|_| bad())
        };
        let a = if hex.len() == 4 * digits {
            channel(3)?
        } else {
            0xff
        };
        Ok(Self::rgba(channel(0)?, channel(1)?, channel(2)?, a))
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

/// Where a tag sits on its target: `[hints] position`. The tag's own
/// matching point goes on the target's, so `BottomRight` puts the tag in
/// the box's bottom-right corner, inside it if it fits.
///
/// `Auto` starts as `Left`, at the box's start, then the overlay moves the
/// tag outside an edge, aligned to its centre: left, below, above, then right. Aligned rows
/// and columns share the side toward their screen edge; two blocked
/// members make the whole line try another side. Boxed in, it stays at
/// the centre. The input box has no `Auto`: nothing is beside it.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Position {
    #[default]
    Auto,
    TopLeft,
    Top,
    TopRight,
    Left,
    Center,
    Right,
    BottomLeft,
    Bottom,
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
            Self::Auto | Self::Left => (0, 1),
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
            Self::Auto => "auto",
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

/// The farthest `[hints] offset` moves a tag, either way, in px.
const OFFSET: i32 = 500;

/// The direction from a gradient's first colour to its second, across a monitor.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Direction {
    Left,
    #[default]
    Right,
    Top,
    Bottom,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

impl Direction {
    const fn name(self) -> &'static str {
        match self {
            Self::Left => "left",
            Self::Right => "right",
            Self::Top => "top",
            Self::Bottom => "bottom",
            Self::TopLeft => "top-left",
            Self::TopRight => "top-right",
            Self::BottomLeft => "bottom-left",
            Self::BottomRight => "bottom-right",
        }
    }

    /// Start and end in fractions of the monitor's width and height.
    pub const fn ends(self) -> ((f32, f32), (f32, f32)) {
        match self {
            Self::Left => ((1.0, 0.0), (0.0, 0.0)),
            Self::Right => ((0.0, 0.0), (1.0, 0.0)),
            Self::Top => ((0.0, 1.0), (0.0, 0.0)),
            Self::Bottom => ((0.0, 0.0), (0.0, 1.0)),
            Self::TopLeft => ((1.0, 1.0), (0.0, 0.0)),
            Self::TopRight => ((0.0, 1.0), (1.0, 0.0)),
            Self::BottomLeft => ((1.0, 0.0), (0.0, 1.0)),
            Self::BottomRight => ((0.0, 0.0), (1.0, 1.0)),
        }
    }
}

/// Keep an omitted endpoint solid when effective settings are read back.
fn render_gradient(
    out: &mut String,
    name: &str,
    direction_name: &str,
    start: Color,
    end: Option<Color>,
    direction: Direction,
) {
    match end {
        Some(color) => {
            let _ = writeln!(out, "{name}_end = \"{color}\"");
        }
        None => {
            let _ = writeln!(out, "# {name}_end = \"{start}\"   # unset: solid colour");
        }
    }
    let _ = writeln!(out, "{direction_name} = \"{}\"", direction.name());
}

/// The `[hints]` table. `#[serde(default)]` fills every missing field from
/// `Style::default()`, and `deny_unknown_fields` rejects `colour = ...`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Style {
    /// A fontconfig pattern, as `fc-match` takes it: `"Inter:bold"`.
    pub font: String,
    /// Text size in px.
    pub size: f32,
    pub text: Color,
    /// A second colour enables one monitor-wide gradient on hint letters.
    pub text_end: Option<Color>,
    pub text_direction: Direction,
    /// Letters already typed, a placeholder. Once called `matched`, which
    /// a file may still say.
    #[serde(alias = "matched")]
    pub dimmed: Color,
    /// An armed verb: its key in front of a tag's letters, its name or
    /// `drop` in the input box.
    pub verb: Color,
    pub background: Color,
    /// A second colour enables one monitor-wide gradient on hint backgrounds.
    pub background_end: Option<Color>,
    pub background_direction: Direction,
    pub border: Color,
    pub border_width: f32,
    pub radius: f32,
    /// Vertical, then horizontal, in px.
    pub padding: [f32; 2],
    pub position: Position,
    /// Moves every tag from its position, right then down, in px.
    pub offset: [i32; 2],
    /// The letters' case on the tags.
    pub case: Case,
    /// The tiny arrow toward a tag's target: `[hints.arrow]`.
    pub arrow: Arrow,
    /// How tags found in a window's pixels differ: `[hints.pixels]`.
    pub pixels: Look,
}

/// Which case tags are drawn in: `[hints] case`. The keys are the same
/// either way.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Case {
    #[default]
    Upper,
    Lower,
}

impl Case {
    /// `letter` in this case, when that's one character (`ß` upper is
    /// `SS`, which would make a tag longer than the keys that type it).
    pub fn of(self, letter: char) -> char {
        fn one(letter: char, mut cased: impl Iterator<Item = char>) -> char {
            match (cased.next(), cased.next()) {
                (Some(c), None) => c,
                _ => letter,
            }
        }
        match self {
            Self::Upper => one(letter, letter.to_uppercase()),
            Self::Lower => one(letter, letter.to_lowercase()),
        }
    }

    const fn name(self) -> &'static str {
        match self {
            Self::Upper => "upper",
            Self::Lower => "lower",
        }
    }
}

/// The letters of tags found in pixels, unless `[hints.pixels] text` sets
/// them: teal.
const PIXELS_TEXT: Color = Color::rgba(0x94, 0xe2, 0xd5, 0xff);

/// A table that changes part of `[hints]` for one kind of tag:
/// `[hints.pixels]` for tags found in a window's pixels, which are guesses
/// and miss more often than tags from the accessibility tree. Unset fields
/// are `[hints]`'s, but for solid letters, which have a colour of their own.
/// A hint gradient is shared; setting this table's text or background makes
/// that part solid instead.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Look {
    pub font: Option<String>,
    pub size: Option<f32>,
    pub text: Option<Color>,
    pub dimmed: Option<Color>,
    pub verb: Option<Color>,
    pub background: Option<Color>,
    pub border: Option<Color>,
    pub border_width: Option<f32>,
    pub radius: Option<f32>,
    pub padding: Option<[f32; 2]>,
}

impl Look {
    /// The table as TOML, for `wayhint config`, under `header`. An unset
    /// field is written commented out, as the value it has now (`now`):
    /// `[hints]`'s, or the letters' own colour.
    fn render(&self, header: &str, now: &Style) -> String {
        let mut out = format!("\n{header}\n");
        let quote = crate::config::quote;
        let [pad_y, pad_x] = now.padding;
        let color = |c: Color| format!("\"{c}\"");
        for (name, set, value) in [
            ("font", self.font.is_some(), quote(&now.font)),
            ("size", self.size.is_some(), now.size.to_string()),
            ("text", self.text.is_some(), color(now.text)),
            ("dimmed", self.dimmed.is_some(), color(now.dimmed)),
            ("verb", self.verb.is_some(), color(now.verb)),
            (
                "background",
                self.background.is_some(),
                color(now.background),
            ),
            ("border", self.border.is_some(), color(now.border)),
            (
                "border_width",
                self.border_width.is_some(),
                now.border_width.to_string(),
            ),
            ("radius", self.radius.is_some(), now.radius.to_string()),
            (
                "padding",
                self.padding.is_some(),
                format!("[{pad_y}, {pad_x}]"),
            ),
        ] {
            let _ = match (set, name) {
                (true, _) => writeln!(out, "{name} = {value}"),
                (false, "text") => {
                    writeln!(out, "# {name} = {value}   # unset: this table's default")
                }
                (false, _) => writeln!(out, "# {name} = {value}   # unset: [hints]'s"),
            };
        }
        out
    }
}

/// A short arrow from a hint toward its target.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Arrow {
    pub color: Option<Color>,
    pub outline: Option<Color>,
    pub length: f32,
    pub outline_width: f32,
}

impl Default for Arrow {
    fn default() -> Self {
        Self {
            color: None,
            outline: None,
            length: 3.0,
            outline_width: 2.0,
        }
    }
}

impl Arrow {
    /// Unset colors follow the hint's text and background.
    pub fn colors(&self, style: &Style) -> (Color, Color) {
        (
            self.color.unwrap_or(style.text),
            self.outline.unwrap_or(style.background),
        )
    }

    fn validate(&self) -> Result<()> {
        for (name, value) in [
            ("length", self.length),
            ("outline_width", self.outline_width),
        ] {
            if !(0.0..=50.0).contains(&value) {
                bail!("[hints.arrow] {name}: {value} px is outside 0-50");
            }
        }
        Ok(())
    }

    /// Preserve inheritance when printing a config that can be read again.
    fn render(&self, style: &Style) -> String {
        let (color, outline) = self.colors(style);
        let mut out = String::from("\n[hints.arrow]\n");
        for (name, set, now, from) in [
            ("color", self.color, color, "text"),
            ("outline", self.outline, outline, "background"),
        ] {
            match set {
                Some(color) => {
                    let _ = writeln!(out, "{name} = \"{color}\"");
                }
                None => {
                    let _ = writeln!(out, "# {name} = \"{now}\"   # follows hints.{from}");
                }
            }
        }
        let _ = writeln!(out, "length = {}", self.length);
        let _ = writeln!(out, "outline_width = {}", self.outline_width);
        out
    }
}

impl Default for Style {
    fn default() -> Self {
        Self {
            font: "sans-serif:bold".into(),
            size: 13.0,
            text: Color::rgba(0xff, 0xff, 0xff, 0xff),
            text_end: None,
            text_direction: Direction::default(),
            dimmed: Color::rgba(0xff, 0xff, 0xff, 0x66),
            verb: Color::rgba(0xfa, 0xb3, 0x87, 0xff),
            background: Color::rgba(0x1e, 0x1e, 0x2e, 0xf0),
            background_end: None,
            background_direction: Direction::default(),
            border: Color::rgba(0xd0, 0xd0, 0xd0, 0xff),
            border_width: 1.0,
            radius: 4.0,
            padding: [2.0, 5.0],
            position: Position::default(),
            offset: [0, 0],
            case: Case::default(),
            arrow: Arrow::default(),
            pixels: Look::default(),
        }
    }
}

impl Style {
    /// Numbers serde can't check by type alone.
    pub fn validate(&self) -> Result<()> {
        self.validate_table("hints")?;
        self.for_pixels().validate_table("hints.pixels")
    }

    /// The look of tags found in pixels: this one, with `[hints.pixels]`
    /// over it.
    pub fn for_pixels(&self) -> Self {
        let text = if self.text_end.is_some() {
            self.text
        } else {
            PIXELS_TEXT
        };
        self.with(&self.pixels, text)
    }

    /// This look with `p`'s fields over it, the letters in `text` unless
    /// `p` sets them.
    fn with(&self, p: &Look, text: Color) -> Self {
        Self {
            font: p.font.clone().unwrap_or_else(|| self.font.clone()),
            size: p.size.unwrap_or(self.size),
            text: p.text.unwrap_or(text),
            text_end: self.text_end.filter(|_| p.text.is_none()),
            dimmed: p.dimmed.unwrap_or(self.dimmed),
            verb: p.verb.unwrap_or(self.verb),
            background: p.background.unwrap_or(self.background),
            background_end: self.background_end.filter(|_| p.background.is_none()),
            border: p.border.unwrap_or(self.border),
            border_width: p.border_width.unwrap_or(self.border_width),
            radius: p.radius.unwrap_or(self.radius),
            padding: p.padding.unwrap_or(self.padding),
            ..self.clone()
        }
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
        self.arrow.validate()
    }

    /// The table as TOML, `[hints.arrow]` and `[hints.pixels]` too, for
    /// `wayhint config`.
    pub fn render(&self) -> String {
        format!(
            "{}{}case = \"{}\"   # the tags' letters: upper or lower\n{}{}",
            self.render_table(
                "hints",
                "on the target: auto (beside it, where no other control is), or "
            ),
            self.render_gradients(),
            self.case.name(),
            self.arrow.render(self),
            self.pixels.render(
                "[hints.pixels]   # tags found in a window's pixels: guesses; unset fields are [hints]'s",
                &self.for_pixels(),
            ),
        )
    }

    /// Optional hint gradients stay unset when the config is printed and read again.
    fn render_gradients(&self) -> String {
        let mut out = String::new();
        for (name, start, end, direction) in [
            ("text", self.text, self.text_end, self.text_direction),
            (
                "background",
                self.background,
                self.background_end,
                self.background_direction,
            ),
        ] {
            render_gradient(
                &mut out,
                name,
                &format!("{name}_direction"),
                start,
                end,
                direction,
            );
        }
        out
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
            ("dimmed", self.dimmed, "   # typed letters, a placeholder"),
            (
                "verb",
                self.verb,
                "   # an armed verb's key or name, and drop",
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
            "position = \"{}\"   # {anchor}center, top, bottom, left, right, or a corner as top-left",
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

/// The input box, resolved from `[input]`: what's typed. An armed verb's
/// name, `drop` while a hold is down, the tag letters typed so far, and
/// text mode's query with its decorations. `[messages]`'s messages are its
/// second line.
#[derive(Debug, Clone, PartialEq)]
pub struct Input {
    /// Whether the overlay draws the box's first line. Hidden, it's still
    /// sent to a listener (`feed.rs`).
    pub visible: bool,
    /// Whether typed tag letters show here, and leave the tags.
    pub hints: bool,
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
            visible: true,
            hints: true,
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

    /// The resolved box as editable TOML.
    pub fn render(&self) -> String {
        let look = self.style.render_table("input", "on the screen: ");
        let (header, fields) = look.split_once('\n').unwrap();
        let quote = crate::config::quote;
        format!(
            "\n{header}   # what's typed, and [messages]'s messages under it\n\
             visible = {}   # false: not drawn, still sent to a listener such as a bar\n\
             show_typed_hints = {}   # typed tag letters show here, and leave the tags\n\
             placeholder = {}   # shown while nothing is typed in text mode\n\
             prefix = {}   # before the query, as \"find: \"\n\
             suffix = {}   # after the query, before the match count\n{fields}",
            self.visible,
            self.hints,
            quote(&self.placeholder),
            quote(&self.prefix),
            quote(&self.suffix),
        )
    }
}

/// What wayhint has to say, under what's typed in the input box: each
/// kind has its own icon and colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Notice {
    /// The windows are being read, and it's taking a while.
    Scanning,
    /// Nothing on the monitor has a tag.
    NoTargets,
    /// A key matched no tag, or a character no text, and was dropped.
    NoMatch,
    /// Every key and what it does.
    Help,
}

/// `[messages]`: which messages the input box's second line shows, for how
/// long, and in which colours.
#[derive(Debug, Clone, PartialEq)]
#[allow(clippy::struct_excessive_bools)] // independent switches, as the file has them
pub struct Info {
    /// Whether the overlay draws the messages. Hidden, they're still
    /// sent to a listener (`feed.rs`).
    pub visible: bool,
    /// `scanning`, once a scan takes long enough to notice.
    pub scanning: bool,
    /// `no targets`, while nothing on the monitor has a tag.
    pub no_targets: bool,
    /// A key that matches no tag, or a character no text: the key is
    /// dropped, and the box says so.
    pub no_match: bool,
    /// How long a message stays.
    pub duration: Duration,
    /// A fontconfig pattern for the icons: a Nerd Font's symbols.
    pub icon_font: String,
    pub badges: Badges,
}

/// A message's icon, a glyph from `[messages] icon_font` (`""` for none), and
/// the colour of the icon and the words.
#[derive(Debug, Clone, PartialEq)]
pub struct Badge {
    pub icon: String,
    pub color: Color,
}

/// Each message's badge.
#[derive(Debug, Clone, PartialEq)]
pub struct Badges {
    pub scanning: Badge,
    pub no_targets: Badge,
    pub no_match: Badge,
    pub help: Badge,
}

impl Default for Info {
    /// Every message on, for two seconds, with Font Awesome's search, ban,
    /// times-circle and keyboard glyphs in blue, yellow, red and lavender.
    fn default() -> Self {
        let badge = |icon: char, r, g, b| Badge {
            icon: icon.into(),
            color: Color::rgba(r, g, b, 0xff),
        };
        Self {
            visible: true,
            scanning: true,
            no_targets: true,
            no_match: true,
            duration: Duration::from_millis(2000),
            icon_font: "Symbols Nerd Font".into(),
            badges: Badges {
                scanning: badge('\u{f002}', 0x89, 0xb4, 0xfa),
                no_targets: badge('\u{f05e}', 0xf9, 0xe2, 0xaf),
                no_match: badge('\u{f057}', 0xf3, 0x8b, 0xa8),
                help: badge('\u{f11c}', 0xb4, 0xbe, 0xfe),
            },
        }
    }
}

impl Info {
    pub const fn badge(&self, notice: Notice) -> &Badge {
        match notice {
            Notice::Scanning => &self.badges.scanning,
            Notice::NoTargets => &self.badges.no_targets,
            Notice::NoMatch => &self.badges.no_match,
            Notice::Help => &self.badges.help,
        }
    }

    /// The resolved table as editable TOML.
    pub fn render(&self) -> String {
        let quote = crate::config::quote;
        let mut out = format!(
            "\n[messages]   # the input box's second line: what wayhint has to say\n\
             visible = {}   # false: not drawn, still sent to a listener such as a bar\n\
             scanning = {}   # once a scan takes over half a second\n\
             no_targets = {}   # while nothing on the monitor has a tag\n\
             no_match = {}   # report a tag key or query that matches nothing\n\
             duration_ms = {}   # message lifetime\n\
             icon_font = {}   # a fontconfig pattern for the icons: a Nerd Font's symbols\n",
            self.visible,
            self.scanning,
            self.no_targets,
            self.no_match,
            self.duration.as_millis(),
            quote(&self.icon_font),
        );
        let b = &self.badges;
        for (name, badge) in [
            ("scanning", &b.scanning),
            ("no_targets", &b.no_targets),
            ("no_match", &b.no_match),
            ("help", &b.help),
        ] {
            let _ = writeln!(out, "{name}_icon = {}", quote(&badge.icon));
            let _ = writeln!(out, "{name}_color = \"{}\"", badge.color);
        }
        out
    }
}

/// How text mode marks a match: a translucent box over its text, which
/// shows through. `[words.matches]` for every match, `[words.matches.current]`
/// for the selected one.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Mark {
    pub background: Color,
    pub background_end: Option<Color>,
    pub background_direction: Direction,
    pub border: Color,
    pub border_width: f32,
    pub radius: f32,
    /// Vertical, then horizontal, in px, around the text's box.
    pub padding: [f32; 2],
}

/// Every match's mark, the current one's, and the words' first letters'.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Marks {
    pub all: Mark,
    pub current: Mark,
    pub starts: Starts,
}

impl Marks {
    /// The marks a file with no `[words.matches]` or `[words.first_character]` gets.
    #[cfg(test)]
    pub const fn new(style: &Style) -> Self {
        let all = Mark::new(style);
        Self {
            all,
            current: all.for_current(),
            starts: Starts::new(),
        }
    }

    /// The tables as TOML, each after a blank line, for `wayhint config`.
    pub fn render(&self) -> String {
        format!(
            "\n{}\n{}\n{}",
            self.all.render("words.matches"),
            self.current.render("words.matches.current"),
            self.starts.render(),
        )
    }
}

/// How text mode marks the first letter of each word, what typing starts a
/// match from, while nothing is typed: `[words.first_character]`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Starts {
    pub mark: StartMark,
    /// Underline height or dot diameter, in logical px.
    pub thickness: f32,
    /// Shared fill; border, radius and padding apply only to boxes.
    pub look: Mark,
}

/// Which mark `[words.first_character]` draws.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StartMark {
    #[default]
    Off,
    Underline,
    Box,
    Dot,
}

impl Starts {
    /// Off; when enabled, blue with a 2 px underline height or dot diameter.
    pub const fn new() -> Self {
        Self {
            mark: StartMark::Off,
            thickness: 2.0,
            look: Mark {
                background: Color::rgba(0x89, 0xb4, 0xfa, 0xff),
                background_end: None,
                background_direction: Direction::Right,
                border: Color::rgba(0x89, 0xb4, 0xfa, 0xff),
                border_width: 0.0,
                radius: 2.0,
                padding: [0.0, 1.0],
            },
        }
    }

    pub fn validate(&self) -> Result<()> {
        if !(0.5..=20.0).contains(&self.thickness) {
            bail!(
                "[words.first_character] thickness: {} px is outside 0.5-20",
                self.thickness
            );
        }
        self.look.validate("words.first_character")
    }

    /// The table as TOML, for `wayhint config`.
    fn render(&self) -> String {
        let mark = match self.mark {
            StartMark::Off => "off",
            StartMark::Underline => "underline",
            StartMark::Box => "box",
            StartMark::Dot => "dot",
        };
        let mut out = format!(
            "[words.first_character]   # the first character of each word, while nothing is typed\n\
             mark = \"{mark}\"   # off, underline, box or dot\n\
             color = \"{}\"\n",
            self.look.background,
        );
        render_gradient(
            &mut out,
            "color",
            "direction",
            self.look.background,
            self.look.background_end,
            self.look.background_direction,
        );
        let _ = writeln!(
            out,
            "thickness = {}   # underline height or dot diameter, in px",
            self.thickness,
        );
        self.look.render_shape(&mut out);
        out
    }
}

impl Mark {
    /// The default for every match: neon yellow, half see-through, with no
    /// border, and the tags' radius and padding.
    pub const fn new(style: &Style) -> Self {
        Self {
            background: Color::rgba(0xff, 0xff, 0x00, 0x80),
            background_end: None,
            background_direction: Direction::Right,
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
        render_gradient(
            &mut out,
            "background",
            "background_direction",
            self.background,
            self.background_end,
            self.background_direction,
        );
        self.render_shape(&mut out);
        out
    }

    /// Border and geometry fields shared by match and first-character boxes.
    fn render_shape(&self, out: &mut String) {
        let _ = writeln!(out, "border = \"{}\"", self.border);
        let _ = writeln!(out, "border_width = {}", self.border_width);
        let _ = writeln!(out, "radius = {}", self.radius);
        let _ = writeln!(
            out,
            "padding = [{}, {}]   # vertical, horizontal",
            self.padding[0], self.padding[1]
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_case_keeps_one_character() {
        assert_eq!(Case::Upper.of('a'), 'A');
        assert_eq!(Case::Upper.of('α'), 'Α');
        assert_eq!(Case::Upper.of(';'), ';');
        assert_eq!(Case::Upper.of('ß'), 'ß', "SS would be two");
        assert_eq!(Case::Lower.of('A'), 'a');
        assert_eq!(Case::Lower.of('a'), 'a');
    }

    #[test]
    fn pixel_tags_differ_only_where_set() {
        let style = Style {
            size: 17.0,
            ..Style::default()
        };
        let pixels = style.for_pixels();
        assert_eq!(pixels.text, Color::rgba(0x94, 0xe2, 0xd5, 0xff), "teal");
        assert_eq!(
            Style {
                text: style.text,
                ..pixels.clone()
            },
            style,
            "the rest is [hints]'s"
        );
        let mut bordered = style.clone();
        bordered.pixels.border = Some(Color::rgba(0xff, 0, 0, 0xff));
        bordered.pixels.border_width = Some(2.0);
        let pixels = bordered.for_pixels();
        assert_eq!((pixels.border.r, pixels.border_width), (0xff, 2.0));
        assert_eq!(pixels.size, 17.0);
        assert_eq!(pixels.text, PIXELS_TEXT, "still teal with the border set");
    }

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
        assert_eq!(
            Color::try_from("#f80".to_string()),
            Ok(Color::rgba(0xff, 0x88, 0x00, 0xff))
        );
        assert_eq!(
            Color::try_from("#f808".to_string()),
            Ok(Color::rgba(0xff, 0x88, 0x00, 0x88))
        );
        assert!(Color::try_from("1e1e2e".to_string()).is_err());
        assert!(Color::try_from("#12345".to_string()).is_err());
        assert!(Color::try_from("#12".to_string()).is_err());
        assert!(
            Color::try_from("#+1+2+3".to_string()).is_err(),
            "a sign isn't a digit"
        );
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
            hints: Style,
        }
        let style = Style {
            size: 20.0,
            position: Position::TopLeft,
            offset: [-3, 4],
            ..Style::default()
        };
        let text = style.render();
        assert_eq!(toml::from_str::<File>(&text).unwrap().hints, style);
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
        assert_eq!(
            Position::Auto.on(rect),
            Position::Left.on(rect),
            "the start"
        );
        assert_eq!(Position::TopLeft.on(rect), Point::new(10, 20));
        assert_eq!(Position::Center.on(rect), rect.center());
        assert_eq!(Position::Right.on(rect), Point::new(50, 35));
        assert_eq!(Position::BottomRight.on(rect), Point::new(50, 50));
    }
}
