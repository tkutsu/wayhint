//! The settings file, `~/.config/wayhint/config.toml`.
//!
//! Two layers, on purpose:
//!
//! - [`File`] is the TOML as written: every field optional, unknown keys an
//!   error (`deny_unknown_fields` catches `alpahbet`), nothing checked yet.
//! - [`Config`] is what the rest of wayhint sees: every value resolved and
//!   validated, so no other module ever handles a missing or broken setting.
//!
//! An empty or missing file is all defaults. Everything is decoded once,
//! here, before any keyboard grab, so a typo can never leave you with a
//! grabbed keyboard and no way to type the tags.

use std::{
    collections::BTreeMap,
    fmt::Write as _,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context as _, Result, anyhow, bail};
use serde::{Deserialize, Serialize};

use crate::{
    action::Action,
    keys::{self, HOME_ROW, Key, KeyCode, Layout},
    pixels::Detector,
    style::{Badge, Badges, Color, Info, Input, Mark, Marks, Position, Style},
};

/// The default config, every setting explained and commented out:
/// `wayhint config --default`, and `config.default.toml` in the repo. A
/// test keeps it equal to the defaults.
pub const DEFAULT: &str = include_str!("../config.default.toml");

/// The file as written. `#[serde(default)]` lets every table and field be
/// left out; `Default` says what "left out" means.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct File {
    keys: FileKeys,
    hints: FileHints,
    style: Style,
    pointer: FilePointer,
    pixels: FilePixels,
    scan: FileScan,
    text: FileText,
    input: Option<FileInput>,
    info: FileInfo,
}

/// `[hints]`: whether a completed tag acts immediately or waits for Enter.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct FileHints {
    auto_accept: Option<bool>,
}

/// `[text]`: how text mode reads windows, acts on matches and looks.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct FileText {
    cursor: TextCursor,
    /// Act on the only match left, without Enter.
    #[serde(alias = "pick_unique")]
    auto_accept: bool,
    /// Tags on the targets typing can't find: on unless `false`.
    hints: Option<bool>,
    /// `[text.matches]`, once `[text.style]`, which a file may still say.
    #[serde(alias = "style")]
    matches: FileMarks,
    /// `[input]`'s old name, from when it showed only the query.
    input: Option<FileInput>,
}

/// `[input]`, once `[text.input]`: the box for what's typed, with the
/// query's decorations. Unset look fields come from the tags' `[style]`,
/// with the box's own spot on the screen.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct FileInput {
    visible: Option<bool>,
    hints: Option<bool>,
    placeholder: Option<String>,
    prefix: String,
    suffix: String,
    font: Option<String>,
    size: Option<f32>,
    text: Option<Color>,
    #[serde(alias = "matched")]
    dimmed: Option<Color>,
    background: Option<Color>,
    border: Option<Color>,
    border_width: Option<f32>,
    radius: Option<f32>,
    padding: Option<[f32; 2]>,
    position: Option<Position>,
    offset: Option<[i32; 2]>,
}

impl FileInput {
    /// Inherit the tags' look, with screen positioning instead of a
    /// target. `table` is the name the file used.
    fn resolve(self, tags: &Style, table: &str) -> Result<Input> {
        let base = Input::new(tags);
        let style = Style {
            font: self.font.unwrap_or(base.style.font),
            size: self.size.unwrap_or(base.style.size),
            text: self.text.unwrap_or(base.style.text),
            dimmed: self.dimmed.unwrap_or(base.style.dimmed),
            background: self.background.unwrap_or(base.style.background),
            border: self.border.unwrap_or(base.style.border),
            border_width: self.border_width.unwrap_or(base.style.border_width),
            radius: self.radius.unwrap_or(base.style.radius),
            padding: self.padding.unwrap_or(base.style.padding),
            position: self.position.unwrap_or(base.style.position),
            offset: self.offset.unwrap_or(base.style.offset),
            ..base.style
        };
        style.validate_table(table)?;
        let placeholder = self.placeholder.unwrap_or(base.placeholder);
        if [&placeholder, &self.prefix, &self.suffix]
            .iter()
            .any(|s| s.chars().any(char::is_control))
        {
            bail!(
                "[{table}] placeholder, prefix and suffix must be single-line text without control characters"
            );
        }
        Ok(Input {
            visible: self.visible.unwrap_or(base.visible),
            hints: self.hints.unwrap_or(base.hints),
            prefix: self.prefix,
            suffix: self.suffix,
            placeholder,
            style,
        })
    }
}

/// `[info]`: which messages the input box's second line shows, for how
/// long, and in which colours.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct FileInfo {
    visible: Option<bool>,
    scanning: Option<bool>,
    no_targets: Option<bool>,
    no_match: Option<bool>,
    /// Ms a message stays.
    duration: Option<u32>,
    icon_font: Option<String>,
    scanning_icon: Option<String>,
    scanning_color: Option<Color>,
    no_targets_icon: Option<String>,
    no_targets_color: Option<Color>,
    no_match_icon: Option<String>,
    no_match_color: Option<Color>,
    help_icon: Option<String>,
    help_color: Option<Color>,
}

/// The longest a message may stay, in ms.
const DURATION: u32 = 10_000;

impl FileInfo {
    fn resolve(self) -> Result<Info> {
        let base = Info::default();
        let duration = match self.duration {
            Some(ms) if !(1..=DURATION).contains(&ms) => {
                bail!("[info] duration = {ms}: give 1-{DURATION} ms")
            }
            Some(ms) => Duration::from_millis(ms.into()),
            None => base.duration,
        };
        let icon_font = self.icon_font.unwrap_or(base.icon_font);
        if icon_font.trim().is_empty() {
            bail!("[info] icon_font: empty; give a font, and \"\" icons for none");
        }
        let badge = |name: &str, icon: Option<String>, color: Option<Color>, base: Badge| {
            let icon = icon.unwrap_or(base.icon);
            if icon.chars().any(char::is_control) || icon.chars().count() > 2 {
                bail!("[info] {name}_icon = {icon:?}: one glyph, or \"\" for none");
            }
            Ok(Badge {
                icon,
                color: color.unwrap_or(base.color),
            })
        };
        let b = base.badges;
        Ok(Info {
            visible: self.visible.unwrap_or(base.visible),
            scanning: self.scanning.unwrap_or(base.scanning),
            no_targets: self.no_targets.unwrap_or(base.no_targets),
            no_match: self.no_match.unwrap_or(base.no_match),
            duration,
            icon_font,
            badges: Badges {
                scanning: badge(
                    "scanning",
                    self.scanning_icon,
                    self.scanning_color,
                    b.scanning,
                )?,
                no_targets: badge(
                    "no_targets",
                    self.no_targets_icon,
                    self.no_targets_color,
                    b.no_targets,
                )?,
                no_match: badge(
                    "no_match",
                    self.no_match_icon,
                    self.no_match_color,
                    b.no_match,
                )?,
                help: badge("help", self.help_icon, self.help_color, b.help)?,
            },
        })
    }
}

/// `[text.matches]`, every match's mark, with `[text.matches.current]` inside
/// it for the selected match's. Unset fields take the defaults (see
/// [`Mark::new`]); unset current ones come from every match's mark, in
/// orange ([`Mark::for_current`]).
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct FileMarks {
    background: Option<Color>,
    border: Option<Color>,
    border_width: Option<f32>,
    radius: Option<f32>,
    padding: Option<[f32; 2]>,
    current: FileMark,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct FileMark {
    background: Option<Color>,
    border: Option<Color>,
    border_width: Option<f32>,
    radius: Option<f32>,
    padding: Option<[f32; 2]>,
}

impl FileMarks {
    /// The marks, with whatever the file leaves out filled in: radius and
    /// padding from the tags' `style`.
    fn resolve(&self, style: &Style) -> Result<Marks> {
        let base = Mark::new(style);
        let all = Mark {
            background: self.background.unwrap_or(base.background),
            border: self.border.unwrap_or(base.border),
            border_width: self.border_width.unwrap_or(base.border_width),
            radius: self.radius.unwrap_or(base.radius),
            padding: self.padding.unwrap_or(base.padding),
        };
        let orange = all.for_current();
        let c = &self.current;
        let current = Mark {
            background: c.background.unwrap_or(orange.background),
            border: c.border.unwrap_or(orange.border),
            border_width: c.border_width.unwrap_or(orange.border_width),
            radius: c.radius.unwrap_or(orange.radius),
            padding: c.padding.unwrap_or(orange.padding),
        };
        all.validate("text.matches")?;
        current.validate("text.matches.current")?;
        Ok(Marks { all, current })
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct FileScan {
    /// Ms from the end of one scan to the next while the tags are up, `0`
    /// for no rescans but the one after each click. It doubles, up to 2 s,
    /// while nothing changes (`pick::IDLE_REFRESH`).
    refresh: Option<u32>,
    /// Logical px between nearby hint centers, `0` to keep every hint.
    merge_distance: Option<u16>,
}

/// How often the tags are brought up to date, in ms, unless the file says.
const REFRESH: u32 = 500;
const MERGE_DISTANCE: u16 = 0;

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct FilePixels {
    detector: Pixels,
    terminal_hint: bool,
    terminal_processes: Vec<String>,
}

impl Default for FilePixels {
    fn default() -> Self {
        Self {
            detector: Pixels::default(),
            terminal_hint: true,
            terminal_processes: [
                "kitty",
                "foot",
                "footclient",
                "alacritty",
                "ghostty",
                "com.mitchellh.ghostty",
                "wezterm",
                "wezterm-gui",
                "org.wezfurlong.wezterm",
                "xterm",
            ]
            .map(str::to_owned)
            .into(),
        }
    }
}

impl FilePixels {
    fn validate(&self) -> Result<()> {
        if self
            .terminal_processes
            .iter()
            .any(|name| name.trim().is_empty() || name.contains('/'))
        {
            bail!(
                "[pixels] terminal_processes: use executable names or window classes, without paths or empty names"
            );
        }
        Ok(())
    }
}

/// Pixel dumps use the same terminal list without needing a keyboard keymap.
pub fn terminal_processes(explicit: Option<&Path>) -> Result<Vec<String>> {
    let file = unkeyed(explicit)?;
    file.pixels.validate()?;
    Ok(file.pixels.terminal_processes)
}

/// Surface dumps share the terminal hint switch without needing a keymap.
pub fn terminal_hint(explicit: Option<&Path>) -> Result<bool> {
    Ok(unkeyed(explicit)?.pixels.terminal_hint)
}

/// The file at `explicit`, else the default one if it's there, else all
/// defaults: undecoded, since the keys need a keymap.
fn unkeyed(explicit: Option<&Path>) -> Result<File> {
    let path = explicit
        .map(Path::to_path_buf)
        .or_else(|| default_path().filter(|p| p.exists()));
    let Some(path) = path else {
        return Ok(File::default());
    };
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("reading config {}", path.display()))?;
    toml::from_str(&text).with_context(|| format!("config {}", path.display()))
}

/// Tags for windows with no accessibility tree, found in their pixels:
/// `[pixels] detector`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Pixels {
    /// Both passes: the most targets, the fewest stray tags.
    #[default]
    Full,
    /// One pass instead of two, about half the work, for slower CPUs.
    Fast,
    /// No tags in those windows.
    Off,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct FileKeys {
    /// Tag letters, easiest to press first, as the first layout prints them.
    alphabet: Option<String>,
    /// Other keys that count as an alphabet letter: `";" = "c"`.
    aliases: BTreeMap<String, String>,
    // One field per verb: a key as written (`"."`, `"Home"`), or `""` for
    // the action a bare tag does. `Action::ALL` lists the same ten.
    left: Option<String>,
    right: Option<String>,
    middle: Option<String>,
    double: Option<String>,
    hover: Option<String>,
    back: Option<String>,
    forward: Option<String>,
    scroll_up: Option<String>,
    scroll_down: Option<String>,
    hold: Option<String>,
    /// Does the last action again, on the same target. No default key;
    /// `""` is the same as leaving it out.
    repeat: Option<String>,
    /// Shows every key and what it does, in the info box. `""` for none.
    help: Option<String>,
    /// Leaves wayhint, from tags and text mode alike. Defaults to Esc.
    close: Option<String>,
}

impl FileKeys {
    /// The field for `action`, if the file sets it. Named fields and a
    /// `match`, not a map, so a misspelt verb is still serde's "unknown
    /// field" error.
    fn verb(&self, action: Action) -> Option<&str> {
        match action {
            Action::Left => &self.left,
            Action::Right => &self.right,
            Action::Middle => &self.middle,
            Action::Double => &self.double,
            Action::Hover => &self.hover,
            Action::Back => &self.back,
            Action::Forward => &self.forward,
            Action::ScrollUp => &self.scroll_up,
            Action::ScrollDown => &self.scroll_down,
            Action::Hold => &self.hold,
        }
        .as_deref()
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct FilePointer {
    after: After,
    /// Px per arrow press. `u16`, so serde itself refuses a negative.
    nudge: Option<u16>,
    /// Wheel notches per scroll.
    scroll: Option<u16>,
    /// Ms the app gets to react to a click before the windows are read
    /// again.
    settle: Option<u16>,
}

/// How far one arrow press moves a command, in px, unless the file says.
const NUDGE: u16 = 10;
/// How many wheel notches a scroll turns, unless the file says: about half
/// a page in most apps.
pub const SCROLL: u16 = 5;
/// How long the app gets to react to a click before the rescan, in ms,
/// unless the file says: a menu's opening animation is mostly done by
/// then. What the app changes later, the next refresh picks up.
const SETTLE: u16 = 150;
/// The longest settle, in ms: past this a run feels stuck between clicks.
const SETTLE_MAX: u16 = 2000;

/// Where the cursor goes after a click: `[pointer] after`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum After {
    /// Back where it was, so the mouse is where you left it.
    #[default]
    Restore,
    /// On the target, for focus-follows-mouse setups.
    Leave,
}

/// Where an action lands on the selected text match: `[text] cursor`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TextCursor {
    Start,
    #[default]
    Center,
    End,
}

/// The verbs' default keys, as positions, so they sit in the same place on
/// every layout, as the home row does. None types a letter, so text mode
/// reads them too, before any text. On US: `,` `/` `.` for right, middle
/// and double, under the right hand; `'` for hover and `;` for hold beside
/// the home row; `[` and `]` for back and forward; Page Up and Page Down
/// for the wheel. Left has none: a bare tag left-clicks.
const DEFAULT_VERBS: [(Action, Option<KeyCode>); 10] = [
    (Action::Left, None),
    (Action::Right, Some(KeyCode::from_evdev(51))),
    (Action::Middle, Some(KeyCode::from_evdev(53))),
    (Action::Double, Some(KeyCode::from_evdev(52))),
    (Action::Hover, Some(KeyCode::from_evdev(40))),
    (Action::Back, Some(KeyCode::HOME)),
    (Action::Forward, Some(KeyCode::END)),
    (Action::ScrollUp, Some(KeyCode::PAGE_UP)),
    (Action::ScrollDown, Some(KeyCode::PAGE_DOWN)),
    (Action::Hold, Some(KeyCode::from_evdev(39))),
];

/// The help key's default is Shift on this position: `?` on US.
const HELP_KEY: KeyCode = KeyCode::from_evdev(53);

/// Keys every mode reads itself, so no verb may take them, and how the
/// message that says so lists them.
const BUILT_IN: [KeyCode; 8] = [
    KeyCode::BACKSPACE,
    KeyCode::SPACE,
    KeyCode::ENTER,
    KeyCode::TAB,
    KeyCode::UP,
    KeyCode::DOWN,
    KeyCode::LEFT,
    KeyCode::RIGHT,
];
const BUILT_IN_NAMES: &str = "Backspace, Space, Enter, Tab and the arrows";

/// Everything wayhint needs from the settings, checked.
#[derive(Debug)]
#[allow(clippy::struct_excessive_bools)] // independent settings, including each mode's acceptance
pub struct Config {
    pub alphabet: Alphabet,
    pub verbs: Verbs,
    /// Act on a completed tag without Enter, including text mode's tags.
    pub hint_auto_accept: bool,
    /// The name of the layout the keys were decoded with ("English (US)"),
    /// for `wayhint config`.
    pub layout: String,
    pub style: Style,
    /// How text mode marks its matches: `[text.matches]`.
    pub marks: Marks,
    /// The box for an armed verb, `drop` and text mode's query: `[input]`.
    pub input: Input,
    /// The box for what wayhint has to say: `[info]`.
    pub info: Info,
    /// Where an action lands within a text match: `[text] cursor`.
    pub text_cursor: TextCursor,
    /// Act on the only match left once the query is long enough, without
    /// Enter: `[text] auto_accept` (formerly `pick_unique`).
    pub text_auto_accept: bool,
    /// Tags, in text mode, on the targets with no text to type:
    /// `[text] hints`.
    pub text_hints: bool,
    pub after: After,
    /// Px one arrow press moves a command: `[pointer] nudge`.
    pub nudge: i32,
    /// Wheel notches one scroll turns: `[pointer] scroll`.
    pub scroll: u16,
    /// How long the app gets to react to a click before the rescan:
    /// `[pointer] settle`.
    pub settle: Duration,
    /// How windows with no accessibility tree are read, `None` for not at
    /// all: `[pixels] detector`.
    pub pixels: Option<Detector>,
    /// One focus tag per terminal, independent of `[pixels] detector`.
    pub terminal_hint: bool,
    /// Executable basenames or window classes read as terminal input areas.
    pub terminal_processes: Vec<String>,
    /// How long the tags stay up before the windows are scanned again,
    /// `None` for only after a click: `[scan] refresh`.
    pub refresh: Option<Duration>,
    /// Nearby hint centers share one tag: `[scan] merge_distance`.
    pub merge_distance: u16,
}

/// One tag letter: the key to press and the character drawn for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Letter {
    pub code: KeyCode,
    pub label: char,
}

/// The tag letters, earliest first, plus the alias keys that type them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Alphabet {
    letters: Vec<Letter>,
    aliases: Vec<Alias>,
    /// Letters left out because a command has their key, with the
    /// command's name, for `wayhint config`.
    dropped: Vec<(char, &'static str)>,
    /// Every letter, the left-out ones too, in order: what `wayhint config`
    /// writes back. Without the left-out ones a default verb would take a
    /// key on reload that the alphabet had kept it off.
    written: String,
}

/// Another key that types a letter: the Ferris Greek layer sends `;` from
/// the `c` position, so `";" = "c"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Alias {
    code: KeyCode,
    label: char,
    letter: usize,
}

impl Alphabet {
    pub fn letters(&self) -> &[Letter] {
        &self.letters
    }

    pub fn len(&self) -> usize {
        self.letters.len()
    }

    /// Which letter a key types, aliases included.
    pub fn index_of(&self, code: KeyCode) -> Option<usize> {
        self.letters
            .iter()
            .position(|l| l.code == code)
            .or_else(|| {
                self.aliases
                    .iter()
                    .find(|a| a.code == code)
                    .map(|a| a.letter)
            })
    }

    /// Decodes `[keys] alphabet`, or the default home row, and checks no
    /// key is in it twice.
    fn decode_letters(written: Option<&str>, layout: &Layout) -> Result<Vec<Letter>> {
        let letters: Vec<Letter> = match written {
            Some(text) => text
                .chars()
                .map(|ch| decode_unshifted(ch, layout).map(|code| Letter { code, label: ch }))
                .collect::<Result<Vec<_>>>()
                .context("[keys] alphabet")?,
            // Defaults are positions; draw them as this layout prints them.
            // Every Latin layout types a character on each of them.
            None => HOME_ROW
                .iter()
                .map(|&code| {
                    let label = layout.char_at(code).with_context(|| {
                        format!(
                            "layout {}: a home row key types nothing; set [keys] alphabet",
                            layout.name()
                        )
                    })?;
                    Ok(Letter { code, label })
                })
                .collect::<Result<_>>()?,
        };
        for (i, letter) in letters.iter().enumerate() {
            if let Some(earlier) = letters[..i].iter().find(|l| l.code == letter.code) {
                bail!(
                    "[keys] alphabet: {:?} and {:?} are the same key",
                    earlier.label,
                    letter.label
                );
            }
        }
        Ok(letters)
    }

    /// Builds the alphabet from its decoded `letters` and `[keys.aliases]`.
    /// Keys in `taken` belong to a command (a verb, or repeat): a key is a
    /// tag letter or a command, never both, so their letters are left out.
    fn resolve(
        mut letters: Vec<Letter>,
        aliases: &BTreeMap<String, String>,
        layout: &Layout,
        taken: &[(KeyCode, &'static str)],
    ) -> Result<Self> {
        let command_of = |code: KeyCode| taken.iter().find(|t| t.0 == code).map(|t| t.1);
        let written = letters.iter().map(|l| l.label).collect();
        let mut dropped = Vec::new();
        // `retain` keeps what the closure returns `true` for; this closure
        // also writes down each letter it throws out.
        letters.retain(|l| match command_of(l.code) {
            Some(command) => {
                dropped.push((l.label, command));
                false
            }
            None => true,
        });
        if letters.len() < 2 {
            let why = if dropped.is_empty() {
                ""
            } else {
                " once command keys are left out"
            };
            bail!(
                "[keys] alphabet: needs at least two letters, got {}{why}",
                letters.len()
            );
        }

        let mut alphabet = Self {
            letters,
            aliases: Vec::new(),
            dropped,
            written,
        };
        for (from, to) in aliases {
            let label = single_char(from, "[keys.aliases]")?;
            let code = decode_unshifted(label, layout).context("[keys.aliases]")?;
            let target = decode_unshifted(single_char(to, "[keys.aliases]")?, layout)
                .context("[keys.aliases]")?;
            if let Some(command) = command_of(code).or_else(|| command_of(target)) {
                bail!("[keys.aliases] {from:?} = {to:?}: that key is {command}'s");
            }
            let Some(letter) = alphabet.letters.iter().position(|l| l.code == target) else {
                bail!("[keys.aliases] {from:?} = {to:?}: {to:?} isn't in the alphabet");
            };
            if alphabet.index_of(code).is_some() {
                bail!("[keys.aliases] {from:?}: already a tag letter or an alias");
            }
            alphabet.aliases.push(Alias {
                code,
                label,
                letter,
            });
        }
        Ok(alphabet)
    }
}

/// The verb keys, checked: exactly one action has no key (a bare tag does
/// it), and every other has a key of its own or is unbound.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verbs {
    default: Action,
    close: (Key, String),
    /// Each bound verb, its key, and the key as `wayhint config` prints it.
    keys: Vec<(Action, Key, String)>,
    /// Verbs whose default key clashes or types a letter or digit on this
    /// layout, with that default, for `wayhint config`.
    unbound: Vec<(Action, String)>,
    /// The repeat key and how it was written, if the file gives one. It
    /// lives with the verbs because it's checked against them: one key,
    /// one command.
    repeat: Option<(Key, String)>,
    /// The help key and how it's printed, and its default if it has none
    /// because a tag or a command has that.
    help: Option<(Key, String)>,
    help_unbound: Option<String>,
}

impl Verbs {
    /// Named close keys accept modifiers too, as Esc always has.
    pub fn is_close(&self, key: Key) -> bool {
        self.close.0 == key
            || (keys::name_of(self.close.0.code).is_some() && self.close.0.code == key.code)
    }

    /// Whether the Escape helper should give this mode's Escape to wayhint.
    pub fn uses_escape(&self, typing: bool) -> bool {
        let key = Key {
            code: KeyCode::ESC,
            shift: false,
        };
        self.is_close(key)
            || (!typing
                && (self.action_of(key).is_some() || self.is_repeat(key) || self.is_help(key)))
    }

    /// The action a bare tag (or a bare `Space`) does.
    pub fn default_action(&self) -> Action {
        self.default
    }

    /// The verb a key arms. Shift is part of the key: `_` isn't `-`.
    pub fn action_of(&self, key: Key) -> Option<Action> {
        self.keys.iter().find(|v| v.1 == key).map(|v| v.0)
    }

    /// Whether `key` is the repeat key.
    pub fn is_repeat(&self, key: Key) -> bool {
        self.repeat.as_ref().is_some_and(|(k, _)| *k == key)
    }

    /// Whether `key` is the help key.
    pub fn is_help(&self, key: Key) -> bool {
        self.help.as_ref().is_some_and(|(k, _)| *k == key)
    }

    /// Every bound key and what it does, one `key\twhat` line each, for
    /// the help key's legend: the verbs, the commands, then the keys
    /// built in.
    pub fn legend(&self) -> String {
        let mut lines: Vec<String> = Vec::new();
        lines.push(format!("tag\t{}", self.default.short()));
        for (action, _, label) in &self.keys {
            lines.push(format!("{label} tag\t{}", action.short()));
        }
        if let Some((_, label)) = &self.repeat {
            lines.push(format!("{label}\tthe last command again"));
        }
        for (key, what) in [
            ("Space", "at the pointer"),
            ("Enter", "confirm a completed tag or text match"),
            ("arrows", "move where the click lands"),
            ("Backspace", "take back a key"),
        ] {
            if keys::named(key) == Some(self.close.0.code) {
                continue;
            }
            lines.push(format!("{key}\t{what}"));
        }
        lines.push(format!("{}\tleave", self.close.1));
        lines.join("\n")
    }

    /// The key that arms `action`, as `wayhint config` prints it (`k`,
    /// `_`, `Home`), for the prefix drawn in front of the tags. `None` for
    /// a verb with no key, such as the default action.
    pub fn label(&self, action: Action) -> Option<&str> {
        self.keys
            .iter()
            .find(|v| v.0 == action)
            .map(|v| v.2.as_str())
    }

    /// Decodes the verb, repeat, help and close fields, fills in the defaults,
    /// checks them.
    ///
    /// The keys the file writes come first, and each must be free: not
    /// built in, not another command's. A written key takes its letter out
    /// of the alphabet. Then each command the file leaves out gets its
    /// default key, unless that's one of `tag_keys` (the alphabet's letters,
    /// yours or the home row, and aliases) or a key the file gave another
    /// command. Defaults that type letters or digits on this layout are
    /// left unbound too, so every bound verb works in text mode.
    fn resolve(file: &FileKeys, layout: &Layout, tag_keys: &[KeyCode]) -> Result<Self> {
        let text = file.close.as_deref().unwrap_or("Esc");
        let key = decode_key(text, layout).context("[keys] close")?;
        let label = keys::name_of(key.code).map_or_else(|| text.into(), String::from);
        let mut verbs = Self {
            default: Action::Left,
            close: (key, label),
            keys: Vec::new(),
            unbound: Vec::new(),
            repeat: None,
            help: None,
            help_unbound: None,
        };
        let mut defaults = Vec::new();
        for (action, _) in DEFAULT_VERBS {
            match file.verb(action) {
                // `""`: no key, a bare tag does it.
                Some("") => defaults.push(action),
                Some(text) => {
                    let (key, label) = verbs.written(action.name(), text, layout)?;
                    if !is_verb_key(&label) {
                        bail!(
                            "[keys] {} = {text:?}: use a symbol or a named key, not a letter or digit",
                            action.name()
                        );
                    }
                    verbs.keys.push((action, key, label));
                }
                None => {}
            }
        }
        if let Some(text) = file.repeat.as_deref().filter(|t| !t.is_empty()) {
            verbs.repeat = Some(verbs.written("repeat", text, layout)?);
        }
        if let Some(text) = file.help.as_deref().filter(|t| !t.is_empty()) {
            verbs.help = Some(verbs.written("help", text, layout)?);
        }
        for (action, default) in DEFAULT_VERBS {
            if file.verb(action).is_some() {
                continue;
            }
            // Left has no default key: a bare tag left-clicks, unless the
            // file makes another verb the default.
            let Some(code) = default else {
                defaults.push(action);
                continue;
            };
            let (key, label) = default_key(action.name(), code, layout)?;
            if !is_verb_key(&label) || verbs.taken_by(code, tag_keys) {
                verbs.unbound.push((action, label));
            } else {
                verbs.keys.push((action, key, label));
            }
        }
        // Help's default is Shift on its spot: `?` on US. A shifted key
        // gives way to a tag on its base key, whose Shift is a Shift-click,
        // and to a command on exactly that key.
        if file.help.is_none()
            && let Some(label) = layout.shifted_char_at(HELP_KEY)
        {
            let key = Key {
                code: HELP_KEY,
                shift: true,
            };
            let commands = verbs.keys.iter().map(|v| v.1);
            let mut commands = commands
                .chain(verbs.repeat.iter().map(|v| v.0))
                .chain([verbs.close.0]);
            if tag_keys.contains(&HELP_KEY) || commands.any(|k| k == key) {
                verbs.help_unbound = Some(label.into());
            } else {
                verbs.help = Some((key, label.into()));
            }
        }
        // In `Action::ALL`'s order, however they were bound, so the same
        // keys compare equal and print the same.
        verbs.keys.sort_by_key(|v| v.0);
        verbs.unbound.sort_by_key(|v| v.0);
        // A slice pattern names the three cases: one default, none, several.
        verbs.default = match defaults.as_slice() {
            [one] => *one,
            [] => bail!("[keys]: no verb is the default; set one to \"\", as in left = \"\""),
            [a, b, ..] => bail!(
                "[keys] {} and {}: only one verb can be the default (\"\"); give {} a key",
                a.name(),
                b.name(),
                a.name()
            ),
        };
        Ok(verbs)
    }

    /// The key the file writes for `command`, and how it's printed, if no
    /// command bound so far has it. Shift is part of the key: `-` and `_`
    /// can be two commands.
    fn written(&self, command: &str, text: &str, layout: &Layout) -> Result<(Key, String)> {
        let key = decode_key(text, layout).with_context(|| format!("[keys] {command}"))?;
        // A name is printed back as the table spells it: "home" is "Home".
        let label = keys::name_of(key.code).map_or_else(|| text.into(), String::from);
        if BUILT_IN.contains(&key.code) {
            bail!("[keys] {command} = {label:?}: {BUILT_IN_NAMES} are built in");
        }
        let clash = self
            .keys
            .iter()
            .find(|v| v.1 == key)
            .map(|v| v.0.name())
            .or_else(|| self.is_repeat(key).then_some("repeat"))
            .or_else(|| self.is_help(key).then_some("help"))
            .or_else(|| self.is_close(key).then_some("close"));
        if let Some(other) = clash {
            bail!("[keys] {other} and {command} are both {label:?}");
        }
        Ok((key, label))
    }

    /// Whether a default key at `code` must give way: it's a tag key, or a
    /// command already has it. The help key counts only as exactly that
    /// key: a `?` written for it leaves middle its `/`.
    fn taken_by(&self, code: KeyCode, tag_keys: &[KeyCode]) -> bool {
        let unshifted = Key { code, shift: false };
        tag_keys.contains(&code)
            || self.is_help(unshifted)
            || self.taken().iter().any(|t| t.0 == code && t.1 != "help")
    }

    /// The help key's line in `[keys]`, for `wayhint config`.
    fn render_help(&self) -> String {
        match (&self.help, &self.help_unbound) {
            (Some((_, label)), _) => {
                format!("help = {}   # every key and what it does\n", quote(label))
            }
            (None, Some(label)) => format!(
                "# help has no key: its default, {label:?}, is on one of your tag letters or is another command's key\n"
            ),
            (None, None) => "help = \"\"   # no key legend\n".into(),
        }
    }

    /// The bound keys' positions and their commands' names: these leave
    /// the alphabet. A shifted key takes its base key too, because Shift on
    /// a tag letter means Shift-click.
    fn taken(&self) -> Vec<(KeyCode, &'static str)> {
        self.keys
            .iter()
            .map(|v| (v.1.code, v.0.name()))
            .chain(self.repeat.iter().map(|(key, _)| (key.code, "repeat")))
            .chain(self.help.iter().map(|(key, _)| (key.code, "help")))
            .chain([(self.close.0.code, "close")])
            .collect()
    }
}

/// One key field: one character is a printable key as the layout types
/// it, Shift allowed (`"_"`); anything longer is a key name (`"Home"`).
fn decode_key(text: &str, layout: &Layout) -> Result<Key> {
    let mut chars = text.chars();
    if let (Some(ch), None) = (chars.next(), chars.next()) {
        return layout.decode(ch).ok_or_else(|| {
            anyhow!(
                "{ch:?} isn't a key on layout {} (main block only)",
                layout.name()
            )
        });
    }
    let code = keys::named(text).ok_or_else(|| {
        let names: Vec<_> = keys::names().collect();
        anyhow!(
            "{text:?} isn't a key name; the names are {}",
            names.join(", ")
        )
    })?;
    Ok(Key { code, shift: false })
}

/// Verbs reserve symbols and named keys, leaving letters and digits for text.
fn is_verb_key(text: &str) -> bool {
    let mut chars = text.chars();
    keys::named(text).is_some()
        || matches!(
            (chars.next(), chars.next()),
            (Some(ch), None) if !ch.is_alphanumeric() && !ch.is_whitespace() && !ch.is_control()
        )
}

/// `command`'s default key, at `code`, and how it's printed: a named key by
/// its name (`PageUp`), else the character this layout types there, which
/// every Latin layout has.
fn default_key(command: &str, code: KeyCode, layout: &Layout) -> Result<(Key, String)> {
    let label = keys::name_of(code)
        .map(String::from)
        .or_else(|| layout.char_at(code).map(String::from))
        .with_context(|| {
            format!(
                "[keys] {command}: its default key types nothing on layout {}; give it a key",
                layout.name()
            )
        })?;
    Ok((Key { code, shift: false }, label))
}

/// A config key string that must be exactly one character.
fn single_char(text: &str, table: &str) -> Result<char> {
    let mut chars = text.chars();
    match (chars.next(), chars.next()) {
        (Some(ch), None) => Ok(ch),
        _ => bail!("{table}: {text:?} must be one character"),
    }
}

/// The unshifted key that types `ch`. Tags stay unshifted because Shift on
/// a tag's last letter will mean Shift-click.
fn decode_unshifted(ch: char, layout: &Layout) -> Result<KeyCode> {
    match layout.decode(ch) {
        Some(key) if !key.shift => Ok(key.code),
        Some(_) => bail!(
            "{ch:?} needs Shift on layout {}; tag letters are unshifted keys",
            layout.name()
        ),
        None => bail!(
            "{ch:?} isn't a key on layout {} (main block only)",
            layout.name()
        ),
    }
}

impl Config {
    /// Reads the file at `explicit`, or the default path if there is one.
    /// A missing default file is fine (all defaults); a missing explicit one
    /// is an error. Keys are decoded with `layout`, your keyboard's first.
    pub fn load(explicit: Option<&Path>, layout: &Layout) -> Result<Self> {
        let path = match explicit {
            Some(path) => path.to_path_buf(),
            None => match default_path() {
                Some(path) if path.exists() => path,
                _ => return Self::parse("", layout),
            },
        };
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading config {}", path.display()))?;
        Self::parse(&text, layout).with_context(|| format!("config {}", path.display()))
    }

    /// Parses and validates config text. Separate from `load` so tests need
    /// no files.
    pub fn parse(text: &str, layout: &Layout) -> Result<Self> {
        let file: File = toml::from_str(text)?;
        let letters = Alphabet::decode_letters(file.keys.alphabet.as_deref(), layout)?;
        // The keys the file made tag keys, so default verbs can stay off
        // them. A broken alias is skipped here; `Alphabet::resolve` says why.
        let tag_keys: Vec<KeyCode> = letters
            .iter()
            .map(|l| l.code)
            .chain(file.keys.aliases.keys().filter_map(|from| {
                let ch = single_char(from, "").ok()?;
                decode_unshifted(ch, layout).ok()
            }))
            .collect();
        let verbs = Verbs::resolve(&file.keys, layout, &tag_keys)?;
        let alphabet = Alphabet::resolve(letters, &file.keys.aliases, layout, &verbs.taken())?;
        file.style.validate()?;
        let marks = file.text.matches.resolve(&file.style)?;
        let input = match (file.input, file.text.input) {
            (Some(_), Some(_)) => {
                bail!("[input] and [text.input] are the same table; keep [input]")
            }
            (Some(input), None) => input.resolve(&file.style, "input")?,
            (None, Some(old)) => old.resolve(&file.style, "text.input")?,
            (None, None) => FileInput::default().resolve(&file.style, "input")?,
        };
        let info = file.info.resolve()?;
        file.pixels.validate()?;
        let nudge = file.pointer.nudge.unwrap_or(NUDGE);
        if nudge == 0 {
            bail!("[pointer] nudge = 0: an arrow press must move at least 1 px");
        }
        let scroll = file.pointer.scroll.unwrap_or(SCROLL);
        if !(1..=50).contains(&scroll) {
            bail!("[pointer] scroll = {scroll}: give 1-50 wheel notches");
        }
        let settle = file.pointer.settle.unwrap_or(SETTLE);
        if settle > SETTLE_MAX {
            bail!("[pointer] settle = {settle}: give 0-{SETTLE_MAX} ms");
        }
        Ok(Self {
            alphabet,
            verbs,
            hint_auto_accept: file.hints.auto_accept.unwrap_or(true),
            layout: layout.name().to_string(),
            style: file.style,
            marks,
            input,
            info,
            text_cursor: file.text.cursor,
            text_auto_accept: file.text.auto_accept,
            text_hints: file.text.hints.unwrap_or(true),
            after: file.pointer.after,
            nudge: nudge.into(),
            scroll,
            settle: Duration::from_millis(settle.into()),
            pixels: match file.pixels.detector {
                Pixels::Full => Some(Detector::Full),
                Pixels::Fast => Some(Detector::Fast),
                Pixels::Off => None,
            },
            terminal_processes: file.pixels.terminal_processes,
            terminal_hint: file.pixels.terminal_hint,
            refresh: match file.scan.refresh.unwrap_or(REFRESH) {
                0 => None,
                ms => Some(Duration::from_millis(ms.into())),
            },
            merge_distance: file.scan.merge_distance.unwrap_or(MERGE_DISTANCE),
        })
    }

    /// `[text]`'s own settings, for `wayhint config`.
    fn render_text(&self) -> String {
        let cursor = match self.text_cursor {
            TextCursor::Start => "start",
            TextCursor::Center => "center",
            TextCursor::End => "end",
        };
        format!(
            "\n[text]\ncursor = \"{cursor}\"   # start, center or end of the selected match\nauto_accept = {}   # act on the only match left, once {} characters are typed; false requires Enter\nhints = {}   # tags on the targets with no text to type, in letters no word starts with\n",
            self.text_auto_accept,
            crate::search::UNIQUE,
            self.text_hints,
        )
    }

    /// The config in effect, as a TOML file you could save: `wayhint config`.
    pub fn render(&self) -> String {
        let mut out =
            String::from("# wayhint config in effect: defaults merged with your file\n\n");
        out.push_str(&self.render_keys());
        let _ = writeln!(
            out,
            "\n[hints]\nauto_accept = {}   # act on a completed tag; false requires Enter, in either mode",
            self.hint_auto_accept
        );
        out.push('\n');
        out.push_str(&self.style.render());
        out.push_str(&self.input.render());
        out.push_str(&self.info.render());
        out.push_str(&self.render_text());
        out.push_str(&self.marks.render());
        let after = match self.after {
            After::Restore => {
                "\"restore\"   # the cursor goes back; \"leave\" keeps it on the target"
            }
            After::Leave => {
                "\"leave\"   # the cursor stays on the target; \"restore\" puts it back"
            }
        };
        let _ = write!(
            out,
            "\n[pointer]\nafter = {after}\nnudge = {}   # px one arrow press moves a click; held, it goes on, faster\n",
            self.nudge
        );
        let _ = writeln!(
            out,
            "scroll = {}   # wheel notches one scroll_up or scroll_down turns",
            self.scroll
        );
        let _ = writeln!(
            out,
            "settle = {}   # ms the app gets to react to a click before the windows are read again",
            self.settle.as_millis()
        );
        let detector = match self.pixels {
            Some(Detector::Full) => "full",
            Some(Detector::Fast) => "fast",
            None => "off",
        };
        let _ = write!(
            out,
            "\n[pixels]\ndetector = \"{detector}\"   # windows with no accessibility tree: \"full\", \"fast\" for slower CPUs, or \"off\"\n"
        );
        let _ = writeln!(
            out,
            "terminal_hint = {}   # one terminal focus tag, even when detector is off",
            self.terminal_hint
        );
        let _ = writeln!(
            out,
            "terminal_processes = [{}]   # replaces the default list; [] disables terminal detection",
            self.terminal_processes
                .iter()
                .map(|name| quote(name))
                .collect::<Vec<_>>()
                .join(", ")
        );
        let refresh = self.refresh.map_or(0, |every| every.as_millis());
        let _ = write!(
            out,
            "\n[scan]\nrefresh = {refresh}   # ms the tags stay up before a rescan, doubling up to 2 s while nothing changes; 0 for only after a click\n"
        );
        let _ = writeln!(
            out,
            "merge_distance = {}   # logical px between nearby hint centers; 0 keeps every hint",
            self.merge_distance
        );
        out
    }

    /// `[keys]`, and `[keys.aliases]` if there are any, for `render`.
    fn render_keys(&self) -> String {
        let mut out = String::from("[keys]\n");
        let _ = writeln!(
            out,
            "alphabet = {}   # easiest to press first; easy letters carry prefixes",
            quote(&self.alphabet.written)
        );
        // A comment, not a setting: the layout is your keyboard's own.
        let _ = writeln!(
            out,
            "# keys are read with your keyboard's first layout, {}",
            self.layout
        );
        if !self.alphabet.dropped.is_empty() {
            let dropped: Vec<String> = self
                .alphabet
                .dropped
                .iter()
                .map(|(label, command)| format!("{label} ({command})"))
                .collect();
            let _ = writeln!(out, "# left out of the alphabet: {}", dropped.join(", "));
        }
        out.push_str(
            "# The verbs: type one before a tag's last letter, or before Space for\n\
             # the pointer, or twice for where the last command was; in text\n\
             # mode, symbols come before the text, named keys work any time.\n\
             # Letters and digits cannot be verbs. \"\" makes a verb what a\n\
             # bare tag does.\n",
        );
        for action in Action::ALL {
            let (name, about) = (action.name(), action.about());
            if action == self.verbs.default {
                let _ = writeln!(out, "{name} = \"\"   # {about}: a bare tag does this");
            } else if let Some(label) = self.verbs.label(action) {
                let _ = writeln!(out, "{name} = {}   # {about}", quote(label));
            } else if let Some((_, label)) = self.verbs.unbound.iter().find(|v| v.0 == action) {
                let why = if is_verb_key(label) {
                    "is one of your tag letters or another command's key"
                } else {
                    "is not a symbol on this layout; choose a symbol or a named key"
                };
                let _ = writeln!(out, "# {name} has no key: its default, {label:?}, {why}");
            }
        }
        let repeat = self.verbs.repeat.as_ref().map_or("", |(_, label)| label);
        let _ = writeln!(
            out,
            "repeat = {}   # the last command again, on the same target; \"\" for none",
            quote(repeat)
        );
        out.push_str(&self.verbs.render_help());
        let _ = writeln!(
            out,
            "close = {}   # leave wayhint, from tags or text mode",
            quote(&self.verbs.close.1)
        );
        if !self.alphabet.aliases.is_empty() {
            out.push_str("\n[keys.aliases]\n");
            for alias in &self.alphabet.aliases {
                let letter = self.alphabet.letters[alias.letter].label.to_string();
                let _ = writeln!(
                    out,
                    "{} = {}",
                    quote(&alias.label.to_string()),
                    quote(&letter)
                );
            }
        }
        out
    }
}

/// `text` as a TOML string, quoted and escaped by the TOML crate itself.
/// Rust's `{:?}` looks the same for plain text but escapes some characters
/// in ways TOML can't read (`\u{17d2}`, found 2026-10-23).
pub fn quote(text: &str) -> String {
    toml::Value::String(text.into()).to_string()
}

/// `$XDG_CONFIG_HOME/wayhint/config.toml`, else `~/.config/wayhint/config.toml`.
/// Two environment variables don't need a crate.
pub fn default_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    Some(base.join("wayhint").join("config.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn us() -> Layout {
        Layout::named("us", "")
    }

    fn labels(config: &Config) -> String {
        config.alphabet.letters().iter().map(|l| l.label).collect()
    }

    #[test]
    fn empty_file_is_all_defaults() {
        let config = Config::parse("", &us()).unwrap();
        assert_eq!(labels(&config), "asdfghjkl");
        assert_eq!(config.layout, "English (US)");
    }

    #[test]
    fn close_defaults_to_escape_and_a_written_close_key_leaves_the_alphabet() {
        let default = Config::parse("", &us()).unwrap();
        assert!(default.verbs.is_close(key(1, false)));
        assert!(default.verbs.is_close(key(1, true)));
        let config = Config::parse("[keys]\nalphabet = \"asq\"\nclose = \"q\"", &us()).unwrap();
        assert_eq!(labels(&config), "as");
        assert!(config.verbs.is_close(key(16, false)));
        assert!(!config.verbs.is_close(key(1, false)));
        assert!(!config.verbs.uses_escape(false));
        assert!(!config.verbs.uses_escape(true));
        assert!(config.verbs.legend().contains("q\tleave"));
        assert!(!config.verbs.legend().contains("Esc\tleave"));
        assert_eq!(
            Config::parse(&config.render(), &us()).unwrap().verbs,
            config.verbs
        );
    }

    #[test]
    fn close_keys_override_built_ins_and_free_escape_for_other_commands() {
        for name in [
            "Backspace",
            "Space",
            "Enter",
            "Tab",
            "Up",
            "Home",
            "Delete",
            "F1",
            "F12",
            "F24",
            "?",
            "Esc",
            "Escape",
        ] {
            let config = Config::parse(&format!("[keys]\nclose = {name:?}"), &us()).unwrap();
            assert!(
                config.verbs.is_close(decode_key(name, &us()).unwrap()),
                "{name}"
            );
        }
        let config = Config::parse("[keys]\nclose = \"q\"\nhover = \"Esc\"", &us()).unwrap();
        assert!(config.verbs.uses_escape(false));
        assert!(!config.verbs.uses_escape(true));
        assert_eq!(config.verbs.action_of(key(1, false)), Some(Action::Hover));
        let clash = Config::parse("[keys]\nclose = \"q\"\nhover = \"q\"", &us()).unwrap_err();
        assert!(format!("{clash:#}").contains("close and hover"));
        assert!(Config::parse("[keys]\nclose = \"\"", &us()).is_err());
    }

    #[test]
    fn alphabet_is_decoded_in_order() {
        let config = Config::parse("[keys]\nalphabet = \"ciaehtns\"", &us()).unwrap();
        assert_eq!(labels(&config), "ciaehtns");
        assert_eq!(config.alphabet.letters()[0].code, KeyCode::from_evdev(46));
    }

    #[test]
    fn aliases_type_their_letter() {
        let config = Config::parse(
            "[keys]\nalphabet = \"ciaehtns\"\n[keys.aliases]\n\";\" = \"c\"",
            &us(),
        )
        .unwrap();
        assert_eq!(config.alphabet.index_of(KeyCode::from_evdev(39)), Some(0));
    }

    #[test]
    fn broken_files_say_why() {
        let err = |text: &str| format!("{:#}", Config::parse(text, &us()).unwrap_err());
        assert!(err("[keys]\nalpahbet = \"ab\"").contains("unknown field"));
        assert!(err("[keys\n").contains("TOML") || err("[keys\n").contains("expected"));
        assert!(err("[keys]\nalphabet = \"aA\"").contains("needs Shift"));
        assert!(err("[keys]\nalphabet = \"aa\"").contains("same key"));
        assert!(err("[keys]\nalphabet = \"a\"").contains("at least two"));
        assert!(err("[style]\nposition = \"middle\"").contains("bottom-right"));
        assert!(err("[keys]\nalphabet = \"aé\"").contains("isn't a key"));
        assert!(
            err("[keys]\nalphabet = \"ab\"\n[keys.aliases]\n\";\" = \"c\"")
                .contains("isn't in the alphabet")
        );
        assert!(
            err("[keys]\nalphabet = \"ab\"\n[keys.aliases]\n\"b\" = \"a\"").contains("already")
        );
    }

    fn key(code: u32, shift: bool) -> Key {
        Key {
            code: KeyCode::from_evdev(code),
            shift,
        }
    }

    #[test]
    fn default_verbs_type_no_letters() {
        let config = Config::parse("", &us()).unwrap();
        assert_eq!(config.verbs.default_action(), Action::Left);
        assert_eq!(config.verbs.action_of(key(51, false)), Some(Action::Right));
        assert_eq!(config.verbs.action_of(key(51, true)), None, "< isn't ,");
        assert_eq!(labels(&config), "asdfghjkl", "nothing left out");
        assert_eq!(config.after, After::Restore);
    }

    #[test]
    fn symbol_and_named_verb_bindings_load() {
        let config = Config::parse(
            "[keys]\nalphabet = \"ehatincsulodypmgf\"\nright = \"\"\nleft = \".\"\n\
             double = \"!\"\nmiddle = \"/\"\nhover = \"_\"\nback = \"home\"\nforward = \"End\"\n\
             [pointer]\nafter = \"leave\"",
            &us(),
        )
        .unwrap();
        assert_eq!(config.verbs.default_action(), Action::Right);
        assert_eq!(config.verbs.action_of(key(52, false)), Some(Action::Left));
        assert_eq!(config.verbs.action_of(key(12, true)), Some(Action::Hover));
        assert_eq!(config.verbs.action_of(key(102, false)), Some(Action::Back));
        assert_eq!(labels(&config), "ehatincsulodypmgf");
        assert_eq!(config.after, After::Leave);
    }

    #[test]
    fn a_written_verb_key_leaves_the_alphabet() {
        // `_` is shifted -: Shift on a tag letter is kept for Shift-click,
        // so the - position leaves the tags.
        let config = Config::parse(
            "[keys]\nalphabet = \"asd-\"\nhover = \"_\"\nright = \"!\"",
            &us(),
        )
        .unwrap();
        assert_eq!(labels(&config), "asd");
        assert!(
            config
                .render()
                .contains("# left out of the alphabet: - (hover)")
        );
    }

    #[test]
    fn a_default_verb_key_gives_way_to_written_letters() {
        // An alphabet with `;` in it: hold loses its default instead of
        // taking a letter.
        let config = Config::parse("[keys]\nalphabet = \"asdfghjkl;\"", &us()).unwrap();
        assert_eq!(labels(&config), "asdfghjkl;");
        assert_eq!(config.verbs.action_of(key(39, false)), None);
        assert_eq!(config.verbs.action_of(key(51, false)), Some(Action::Right));
        let rendered = config.render();
        assert!(rendered.contains("# hold has no key: its default, \";\", is one of your"));
        let again = Config::parse(&rendered, &us()).unwrap();
        assert_eq!(again.verbs, config.verbs);
    }

    #[test]
    fn broken_verbs_say_why() {
        let err = |text: &str| format!("{:#}", Config::parse(text, &us()).unwrap_err());
        assert!(err("[keys]\nright = \"\"").contains("only one verb can be the default"));
        assert!(err("[keys]\nleft = \".\"").contains("no verb is the default"));
        assert!(err("[keys]\nhover = \"esc\"").contains("close and hover"));
        assert!(err("[keys]\nright = \",\"\nmiddle = \",\"").contains("both"));
        assert!(err("[keys]\nback = \"PageUpp\"").contains("the names are Up, Down"));
        assert!(err("[keys]\nhover = \"é\"").contains("isn't a key"));
        assert!(
            err("[keys]\nalphabet = \"a;\"\nhover = \":\"")
                .contains("once command keys are left out")
        );
        assert!(err("[keys]\nright = \",\"\n[keys.aliases]\n\",\" = \"a\"").contains("right's"));
        assert!(err("[keys]\nright = \",\"\nrepeat = \",\"").contains("right and repeat"));
        assert!(err("[keys]\nrepeat = \"Enter\"").contains("built in"));
        assert!(err("[keys]\nback = \"Left\"").contains("and the arrows are built in"));
        assert!(err("[pointer]\nafter = \"stay\"").contains("unknown variant"));
    }

    #[test]
    fn every_verb_rejects_letters_and_digits_but_repeat_still_accepts_them() {
        for (name, variant, letters) in [("us", "", "aZ09"), ("fr", "", "éà"), ("gr", "", "αΩ")]
        {
            let layout = Layout::named(name, variant);
            for action in Action::ALL {
                for ch in letters.chars() {
                    let text = format!("[keys]\n{} = \"{ch}\"", action.name());
                    let err = format!("{:#}", Config::parse(&text, &layout).unwrap_err());
                    assert!(err.contains("use a symbol or a named key"), "{text}: {err}");
                    assert!(err.contains(action.name()), "{err}");
                }
            }
        }
        let config = Config::parse("[keys]\nrepeat = \"r\"", &us()).unwrap();
        assert!(config.verbs.is_repeat(key(19, false)));
    }

    #[test]
    fn nudge_is_10_px_unless_set() {
        assert_eq!(Config::parse("", &us()).unwrap().nudge, 10);
        let five = Config::parse("[pointer]\nnudge = 5", &us()).unwrap();
        assert_eq!(five.nudge, 5);
        let err = |text: &str| format!("{:#}", Config::parse(text, &us()).unwrap_err());
        assert!(err("[pointer]\nnudge = 0").contains("at least 1 px"));
        assert!(err("[pointer]\nnudge = -3").contains("invalid value"));
    }

    #[test]
    fn pixel_tags_are_full_unless_set() {
        let read = |text: &str| Config::parse(text, &us()).unwrap().pixels;
        assert_eq!(read(""), Some(Detector::Full));
        assert_eq!(read("[pixels]\ndetector = \"fast\""), Some(Detector::Fast));
        assert_eq!(read("[pixels]\ndetector = \"off\""), None);
        let err = format!(
            "{:#}",
            Config::parse("[pixels]\ndetector = \"quick\"", &us()).unwrap_err()
        );
        assert!(err.contains("unknown variant"));
        for text in [
            "",
            "[pixels]\ndetector = \"fast\"",
            "[pixels]\ndetector = \"off\"",
        ] {
            let config = Config::parse(text, &us()).unwrap();
            let again = Config::parse(&config.render(), &us()).unwrap();
            assert_eq!(again.pixels, config.pixels, "{text:?} reads back the same");
        }
    }

    #[test]
    fn text_cursor_defaults_to_center_and_round_trips() {
        assert_eq!(
            Config::parse("", &us()).unwrap().text_cursor,
            TextCursor::Center
        );
        for (name, cursor) in [
            ("start", TextCursor::Start),
            ("center", TextCursor::Center),
            ("end", TextCursor::End),
        ] {
            let config = Config::parse(&format!("[text]\ncursor = {name:?}"), &us()).unwrap();
            assert_eq!(config.text_cursor, cursor);
            let again = Config::parse(&config.render(), &us()).unwrap();
            assert_eq!(again.text_cursor, cursor);
        }
        let err = Config::parse("[text]\ncursor = \"middle\"", &us()).unwrap_err();
        let err = format!("{err:#}");
        assert!(err.contains("cursor"), "{err}");
        for name in ["start", "center", "end"] {
            assert!(err.contains(name), "{err}");
        }
    }

    #[test]
    fn terminal_process_lists_replace_defaults_and_round_trip() {
        let default = Config::parse("", &us()).unwrap();
        assert!(default.terminal_processes.iter().any(|s| s == "kitty"));
        for list in ["[]", "[\"custom-term\", \"kitty\"]", "[\"foot\"]"] {
            let config =
                Config::parse(&format!("[pixels]\nterminal_processes = {list}"), &us()).unwrap();
            let again = Config::parse(&config.render(), &us()).unwrap();
            assert_eq!(config.terminal_processes, again.terminal_processes);
        }
        for list in ["[\"\"]", "[\"  \"]", "[\"/usr/bin/kitty\"]"] {
            assert!(
                Config::parse(&format!("[pixels]\nterminal_processes = {list}"), &us()).is_err()
            );
        }
    }

    #[test]
    fn terminal_hint_is_independent_of_the_pixel_detector() {
        assert!(Config::parse("", &us()).unwrap().terminal_hint);
        for detector in ["full", "fast", "off"] {
            for enabled in [true, false] {
                let text = format!("[pixels]\ndetector = {detector:?}\nterminal_hint = {enabled}");
                let config = Config::parse(&text, &us()).unwrap();
                assert_eq!(config.terminal_hint, enabled);
                let again = Config::parse(&config.render(), &us()).unwrap();
                assert_eq!(again.terminal_hint, enabled);
                assert_eq!(again.pixels, config.pixels);
            }
        }
        assert!(Config::parse("[pixels]\nterminal_hint = \"off\"", &us()).is_err());
    }

    #[test]
    fn nearby_hint_merging_is_disabled_unless_set() {
        for (text, expected) in [
            ("", 0),
            ("[scan]\nmerge_distance = 5", 5),
            ("[scan]\nmerge_distance = 12", 12),
            ("[scan]\nmerge_distance = 0", 0),
        ] {
            let config = Config::parse(text, &us()).unwrap();
            assert_eq!(config.merge_distance, expected);
            let again = Config::parse(&config.render(), &us()).unwrap();
            assert_eq!(again.merge_distance, expected);
        }
        for value in ["-1", "65536", "1.5"] {
            assert!(Config::parse(&format!("[scan]\nmerge_distance = {value}"), &us()).is_err());
        }
    }

    #[test]
    fn tags_refresh_every_half_second_unless_set() {
        let read = |text: &str| Config::parse(text, &us()).unwrap().refresh;
        assert_eq!(read(""), Some(Duration::from_millis(500)));
        assert_eq!(
            read("[scan]\nrefresh = 250"),
            Some(Duration::from_millis(250))
        );
        assert_eq!(read("[scan]\nrefresh = 0"), None);
        let err = |text: &str| format!("{:#}", Config::parse(text, &us()).unwrap_err());
        assert!(err("[scan]\nrefresh = -1").contains("invalid value"));
        assert!(err("[scan]\nrefrash = 9").contains("unknown field"));
        for text in ["", "[scan]\nrefresh = 250", "[scan]\nrefresh = 0"] {
            let config = Config::parse(text, &us()).unwrap();
            let again = Config::parse(&config.render(), &us()).unwrap();
            assert_eq!(
                again.refresh, config.refresh,
                "{text:?} reads back the same"
            );
        }
    }

    #[test]
    fn the_repeat_key_takes_its_letter() {
        let config = Config::parse("[keys]\nalphabet = \"asdf\"\nrepeat = \"f\"", &us()).unwrap();
        let f = Key {
            code: KeyCode::from_evdev(33),
            shift: false,
        };
        assert!(config.verbs.is_repeat(f));
        assert_eq!(config.alphabet.len(), 3, "f left the alphabet");
        assert!(
            config
                .render()
                .contains("# left out of the alphabet: f (repeat)")
        );
        let off = Config::parse("[keys]\nrepeat = \"\"", &us()).unwrap();
        assert!(!off.verbs.is_repeat(f));
    }

    #[test]
    fn text_mode_has_no_key() {
        // Text mode is a run of its own (`--text`), on a compositor
        // shortcut, since 2026-10-26.
        let err = format!(
            "{:#}",
            Config::parse("[keys]\ntext = \"q\"", &us()).unwrap_err()
        );
        assert!(err.contains("unknown field `text`"), "{err}");
        let legend = Config::parse("", &us()).unwrap().verbs.legend();
        assert!(!legend.contains("text mode"), "{legend}");
    }

    #[test]
    fn text_marks_are_yellow_and_orange_unless_set() {
        let config = Config::parse("[style]\nborder_width = 2\nradius = 3", &us()).unwrap();
        let base = Mark::new(&config.style);
        assert_eq!(config.marks.all, base);
        assert_eq!(config.marks.current, base.for_current());
        assert!(
            (config.marks.all.radius - 3.0).abs() < f32::EPSILON,
            "the tags' radius"
        );
        assert!(
            config.marks.all.border_width.abs() < f32::EPSILON,
            "but no border"
        );
        let set = Config::parse(
            "[text.style]\nbackground = \"#e8dcc440\"\npadding = [1, 2]\n\
             [text.style.current]\nborder_width = 3",
            &us(),
        )
        .unwrap();
        assert_eq!(
            set.marks.all.background,
            Color::rgba(0xe8, 0xdc, 0xc4, 0x40)
        );
        assert_eq!(
            set.marks.current.background,
            Color::rgba(0xff, 0x96, 0x32, 0x80),
            "orange, whatever every match's is"
        );
        assert_eq!(
            set.marks.current.padding,
            [1.0, 2.0],
            "every match's padding"
        );
        assert!((set.marks.current.border_width - 3.0).abs() < f32::EPSILON);
        let again = Config::parse(&set.render(), &us()).unwrap();
        assert_eq!(again.marks, set.marks);
        let err = |text: &str| format!("{:#}", Config::parse(text, &us()).unwrap_err());
        assert!(
            err("[text.matches.current]\nradius = -1").contains("[text.matches.current] radius")
        );
        assert!(err("[text.matches]\nopacity = 0.5").contains("unknown field"));
        assert!(err("[text.matches]\nborder = \"red\"").contains("isn't a colour"));
    }

    #[test]
    fn text_input_inherits_tags_but_has_its_own_screen_position() {
        let config = Config::parse(
            "[style]\nfont = 'serif'\nsize = 22\ntext = '#123456'\nposition = 'top-right'\noffset = [3, 4]",
            &us(),
        ).unwrap();
        let mut look = config.style.clone();
        look.position = Position::Bottom;
        look.offset = [0, -48];
        assert_eq!(config.input.style, look);
        assert!(config.input.prefix.is_empty() && config.input.suffix.is_empty());
    }

    #[test]
    fn text_input_and_current_match_settings_round_trip_independently() {
        let config = Config::parse(
            r##"
[style]
size = 11
[text.matches.current]
background = "#10203040"
border = "#50607080"
border_width = 2
radius = 6
padding = [3, 4]
[text.input]
prefix = "find \""
suffix = '" \literal'
font = "serif"
size = 28
text = "#abcdef"
matched = "#778899"
background = "#01020304"
border = "#05060708"
border_width = 3
radius = 7
padding = [8, 9]
position = "top-right"
offset = [-13, 17]
"##,
            &us(),
        )
        .unwrap();
        assert_eq!(config.input.prefix, "find \"");
        assert_eq!(config.input.style.position, Position::TopRight);
        assert_eq!(config.input.style.offset, [-13, 17]);
        assert_eq!(config.input.style.size, 28.0);
        assert_eq!(config.input.style.text, Color::rgba(0xab, 0xcd, 0xef, 0xff));
        assert_eq!(
            config.marks.current.background,
            Color::rgba(0x10, 0x20, 0x30, 0x40)
        );
        assert_eq!(
            config.marks.current.border,
            Color::rgba(0x50, 0x60, 0x70, 0x80)
        );
        assert_eq!(config.marks.current.padding, [3.0, 4.0]);
        assert_eq!(config.style.size, 11.0);
        let again = Config::parse(&config.render(), &us()).unwrap();
        assert_eq!(again.input, config.input);
        assert_eq!(again.marks, config.marks);
    }

    #[test]
    fn invalid_text_input_settings_fail_before_the_keyboard_grab() {
        for (field, expected) in [
            ("size = nan", "[text.input] size"),
            ("size = 0", "[text.input] size"),
            ("font = ''", "[text.input] font"),
            ("padding = [-1, 2]", "[text.input] padding"),
            ("radius = inf", "[text.input] radius"),
            ("border_width = -1", "[text.input] border_width"),
            ("offset = [501, 0]", "[text.input] offset"),
            ("background = 'red'", "isn't a colour"),
            ("position = 'nowhere'", "unknown variant"),
            ("colour = '#ffffff'", "unknown field"),
            (r#"prefix = "line\nline""#, "single-line"),
            (r#"suffix = "\t""#, "single-line"),
        ] {
            let error = Config::parse(&format!("[text.input]\n{field}"), &us()).unwrap_err();
            assert!(
                format!("{error:#}").contains(expected),
                "{field}: {error:#}"
            );
        }
    }

    #[test]
    fn rendered_config_reads_back_the_same() {
        let text = "[keys]\nalphabet = \"ciaehtns\"\nright = \"\"\nleft = \".\"\nhover = '\"'\nrepeat = \"q\"\n\
                    [keys.aliases]\n\";\" = \"c\"\n[style]\nsize = 15\nposition = \"top\"\noffset = [3, -2]\n[pointer]\nafter = \"leave\"\nnudge = 9";
        let config = Config::parse(text, &us()).unwrap();
        let again = Config::parse(&config.render(), &us()).unwrap();
        assert_eq!(again.alphabet, config.alphabet);
        assert_eq!(again.verbs, config.verbs);
        assert_eq!(again.style, config.style);
        assert_eq!(again.input, config.input);
        assert_eq!(again.after, config.after);
        assert_eq!(again.nudge, 9);
    }

    #[test]
    fn a_left_out_letter_is_written_back_so_it_stays_out() {
        // `r` is a tag letter, so right's default stays off it; then repeat
        // takes it. Written as `"as"`, the reload would give right its `r`
        // back, and right and repeat would clash.
        let config = Config::parse("[keys]\nalphabet = \"asr\"\nrepeat = \"r\"", &us()).unwrap();
        assert_eq!(labels(&config), "as");
        let rendered = config.render();
        assert!(rendered.contains("alphabet = \"asr\""), "{rendered}");
        let again = Config::parse(&rendered, &us()).unwrap();
        assert_eq!(again.alphabet, config.alphabet);
        assert_eq!(again.verbs, config.verbs);
    }

    #[test]
    fn defaults_read_back_the_same_on_latin_layouts() {
        // wayhint supports Latin layouts only (2026-10-23). These move the
        // home row and the verb keys the most: on AZERTY hold's `;` is `m`,
        // and a dead key there (`^` on `[`) once failed a default.
        for (name, variant) in [
            ("us", ""),
            ("us", "dvorak"),
            ("us", "colemak"),
            ("fr", ""),
            ("fr", "bepo"),
            ("de", "neo"),
        ] {
            let layout = Layout::named(name, variant);
            let config = Config::parse("", &layout).unwrap();
            assert!(
                config
                    .verbs
                    .keys
                    .iter()
                    .all(|(_, _, label)| is_verb_key(label)),
                "{name}({variant}): every default verb must be a symbol or a named key"
            );
            let rendered = config.render();
            let again = Config::parse(&rendered, &layout)
                .unwrap_or_else(|e| panic!("{name}({variant}): {e:#}\n{rendered}"));
            assert_eq!(again.alphabet, config.alphabet, "{name}({variant})");
            assert_eq!(again.verbs, config.verbs, "{name}({variant})");
        }
        let config = Config::parse("", &Layout::named("fr", "")).unwrap();
        assert_eq!(config.verbs.label(Action::Hold), None, "AZERTY types m");
        assert!(config.render().contains("is not a symbol on this layout"));
    }

    #[test]
    fn missing_explicit_file_is_an_error() {
        assert!(Config::load(Some(Path::new("/nonexistent/wayhint.toml")), &us()).is_err());
    }

    /// The settings in `text`, commented out or not, as TOML: `# size = 13`
    /// reads as `size = 13`. Prose and examples (`# ";" = "c"`) stay
    /// comments: only a lower-case name before `=` is a setting.
    fn settings(text: &str) -> toml::Table {
        let is_name = |key: &str| {
            let key = key.trim();
            !key.is_empty() && key.chars().all(|c| c.is_ascii_lowercase() || c == '_')
        };
        let lines: Vec<&str> = text
            .lines()
            .map(|line| {
                line.strip_prefix("# ")
                    .filter(|rest| rest.split_once('=').is_some_and(|(key, _)| is_name(key)))
                    .unwrap_or(line)
            })
            .collect();
        toml::from_str(&lines.join("\n")).unwrap()
    }

    #[test]
    fn the_default_file_shows_every_setting_at_its_default() {
        let defaults = Config::parse("", &us()).unwrap();
        assert_eq!(
            settings(DEFAULT),
            settings(&defaults.render()),
            "config.default.toml lists every setting `wayhint config` prints, at its default"
        );
        let uncommented = toml::to_string(&settings(DEFAULT)).unwrap();
        let written = Config::parse(&uncommented, &us()).unwrap();
        assert_eq!(
            settings(&written.render()),
            settings(&defaults.render()),
            "uncommented, the same values"
        );
        let copied = Config::parse(DEFAULT, &us()).unwrap();
        assert_eq!(copied.render(), defaults.render(), "copied as it is, too");
    }

    #[test]
    fn the_new_verbs_default_to_page_keys_and_semicolon() {
        let config = Config::parse("", &us()).unwrap();
        let verbs = &config.verbs;
        assert_eq!(
            verbs.action_of(key(104, false)),
            Some(Action::ScrollUp),
            "PageUp"
        );
        assert_eq!(verbs.action_of(key(109, false)), Some(Action::ScrollDown));
        assert_eq!(verbs.action_of(key(39, false)), Some(Action::Hold), ";");
        assert_eq!(config.scroll, 5);
        let mine = Config::parse(
            "[keys]\nscroll_up = \"PgUp\"\nscroll_down = \"pagedown\"\nhold = \"_\"\n[pointer]\nscroll = 3",
            &us(),
        )
        .unwrap();
        assert_eq!(mine.verbs.label(Action::ScrollUp), Some("PageUp"));
        assert_eq!(mine.verbs.action_of(key(12, true)), Some(Action::Hold));
        assert_eq!(mine.scroll, 3);
        let err = |text: &str| format!("{:#}", Config::parse(text, &us()).unwrap_err());
        assert!(err("[pointer]\nscroll = 0").contains("1-50"));
        assert!(err("[pointer]\nscroll = 51").contains("1-50"));
    }

    #[test]
    fn a_default_key_gives_way_to_a_key_the_file_writes() {
        // Right's default is `,`, middle's `/`. Before 2026-10-26 a written
        // `hover = "r"` failed (right's default was `r` then): "right and
        // hover are both r".
        let config = Config::parse("[keys]\nhover = \",\"\nback = \"/\"", &us()).unwrap();
        assert_eq!(config.verbs.action_of(key(51, false)), Some(Action::Hover));
        assert_eq!(config.verbs.label(Action::Right), None);
        assert_eq!(config.verbs.label(Action::Middle), None);
        let rendered = config.render();
        assert!(
            rendered.contains("# right has no key: its default, \",\", is one of your tag letters or another command's key"),
            "{rendered}"
        );
        let again = Config::parse(&rendered, &us()).unwrap();
        assert_eq!(again.verbs, config.verbs);
    }

    #[test]
    fn old_names_still_load() {
        let config = Config::parse(
            "[style]\nmatched = \"#11223344\"\n[text.style]\nbackground = \"#55667788\"\n\
             [text.input]\nmatched = \"#99aabbcc\"",
            &us(),
        )
        .unwrap();
        assert_eq!(config.style.dimmed, Color::rgba(0x11, 0x22, 0x33, 0x44));
        assert_eq!(
            config.marks.all.background,
            Color::rgba(0x55, 0x66, 0x77, 0x88)
        );
        assert_eq!(
            config.input.style.dimmed,
            Color::rgba(0x99, 0xaa, 0xbb, 0xcc)
        );
        let rendered = config.render();
        assert!(rendered.contains("dimmed = \"#11223344\""));
        assert!(rendered.contains("[text.matches]") && !rendered.contains("[text.style]"));
    }

    #[test]
    fn the_leader_follows_the_tags_unless_set() {
        let config = Config::parse("[style]\ntext = \"#123456\"", &us()).unwrap();
        let leader = config.style.leader;
        let (line, dot, outline) = leader.colors(&config.style);
        assert_eq!((line, dot), (config.style.text, config.style.text));
        assert_eq!(outline, config.style.background);
        let again = Config::parse(&config.render(), &us()).unwrap();
        assert_eq!(again.style, config.style, "unset stays unset");
        let set = Config::parse(
            "[style.leader]\nline = \"#ff0000\"\ndot = \"#00ff00\"\noutline_width = 0\nwidth = 2",
            &us(),
        )
        .unwrap();
        let (line, dot, _) = set.style.leader.colors(&set.style);
        assert_eq!(line, Color::rgba(0xff, 0, 0, 0xff));
        assert_eq!(dot, Color::rgba(0, 0xff, 0, 0xff));
        assert_eq!(
            Config::parse(&set.render(), &us()).unwrap().style,
            set.style
        );
        let err = |text: &str| format!("{:#}", Config::parse(text, &us()).unwrap_err());
        assert!(err("[style.leader]\nwidth = -1").contains("[style.leader] width"));
        assert!(err("[style.leader]\ncolour = \"#ffffff\"").contains("unknown field"));
    }

    #[test]
    fn the_input_box_is_input_and_its_old_name_still_loads() {
        let new = Config::parse("[input]\nposition = \"top\"\nhints = false", &us()).unwrap();
        let old = Config::parse("[text.input]\nposition = \"top\"\nhints = false", &us()).unwrap();
        assert_eq!(new.input, old.input);
        assert_eq!(new.input.style.position, Position::Top);
        assert!(!new.input.hints);
        assert!(
            Config::parse("", &us()).unwrap().input.hints,
            "on by default"
        );
        let err = |text: &str| format!("{:#}", Config::parse(text, &us()).unwrap_err());
        assert!(err("[input]\n[text.input]").contains("same table"));
        assert!(err("[input]\nsize = 0").contains("[input] size"));
        assert!(err("[input]\nfade = 100").contains("unknown field"));
    }

    #[test]
    fn info_shows_every_message_unless_told_and_round_trips() {
        let info = Config::parse("", &us()).unwrap().info;
        assert_eq!(info, Info::default());
        assert!(info.scanning && info.no_targets && info.no_match);
        assert_eq!(info.duration, Duration::from_millis(2000));
        let mine = Config::parse(
            "[info]\nscanning = false\nno_match = false\nduration = 400\nno_match_color = \"#000\"",
            &us(),
        )
        .unwrap();
        assert!(!mine.info.scanning && mine.info.no_targets && !mine.info.no_match);
        assert_eq!(mine.info.duration, Duration::from_millis(400));
        assert_eq!(mine.info.badges.no_match.color, Color::rgba(0, 0, 0, 0xff));
        assert_eq!(mine.info.badges.help, info.badges.help);
        let again = Config::parse(&mine.render(), &us()).unwrap();
        assert_eq!(again.info, mine.info);
        let err = |text: &str| format!("{:#}", Config::parse(text, &us()).unwrap_err());
        assert!(err("[info]\nduration = 0").contains("[info] duration"));
        assert!(err("[info]\nposition = \"top\"").contains("unknown field"));
    }

    #[test]
    fn case_pixel_looks_and_pick_unique_read_back_the_same() {
        let plain = Config::parse("", &us()).unwrap();
        assert_eq!(plain.style.case, crate::style::Case::Upper);
        assert!(!plain.text_auto_accept, "Enter is needed unless told");
        let mine = Config::parse(
            "[style]\ncase = \"lower\"\n[style.pixels]\nborder = \"#f00\"\nsize = 11\n[text]\npick_unique = true",
            &us(),
        )
        .unwrap();
        assert_eq!(mine.style.case, crate::style::Case::Lower);
        assert_eq!(
            mine.style.pixels.border,
            Some(Color::rgba(0xff, 0, 0, 0xff))
        );
        assert_eq!(mine.style.for_pixels().size, 11.0);
        assert!(mine.text_auto_accept);
        let again = Config::parse(&mine.render(), &us()).unwrap();
        assert_eq!(again.style, mine.style);
        assert!(again.text_auto_accept);
        let err = |text: &str| format!("{:#}", Config::parse(text, &us()).unwrap_err());
        assert!(err("[style]\ncase = \"title\"").contains("case"));
        assert!(err("[style.pixels]\nsize = 0").contains("[style.pixels] size"));
        assert!(err("[style.pixels]\nposition = \"top\"").contains("unknown field"));
    }

    #[test]
    fn acceptance_settings_are_independent_and_round_trip() {
        let defaults = Config::parse("", &us()).unwrap();
        assert!(defaults.hint_auto_accept);
        assert!(!defaults.text_auto_accept);
        for hints in [false, true] {
            for text in [false, true] {
                let written =
                    format!("[hints]\nauto_accept = {hints}\n[text]\nauto_accept = {text}");
                let config = Config::parse(&written, &us()).unwrap();
                let again = Config::parse(&config.render(), &us()).unwrap();
                assert_eq!(again.hint_auto_accept, hints);
                assert_eq!(again.text_auto_accept, text);
            }
        }
        for invalid in [
            "[hints]\nauto_accept = \"enter\"",
            "[text]\nauto_accept = 1",
            "[text]\nauto_accept = true\npick_unique = false",
            "[hints]\nauto_acceppt = false",
        ] {
            assert!(Config::parse(invalid, &us()).is_err(), "{invalid}");
        }
    }

    #[test]
    fn settle_is_150_ms_unless_set() {
        assert_eq!(
            Config::parse("", &us()).unwrap().settle,
            Duration::from_millis(150)
        );
        let config = Config::parse("[pointer]\nsettle = 400", &us()).unwrap();
        assert_eq!(config.settle, Duration::from_millis(400));
        let again = Config::parse(&config.render(), &us()).unwrap();
        assert_eq!(again.settle, config.settle);
        let err = format!(
            "{:#}",
            Config::parse("[pointer]\nsettle = 2001", &us()).unwrap_err()
        );
        assert!(err.contains("settle"), "{err}");
    }

    #[test]
    fn help_is_shift_on_slash_unless_that_is_taken() {
        let config = Config::parse("", &us()).unwrap();
        let middle = Some(Action::Middle);
        assert!(config.verbs.is_help(key(53, true)), "?");
        assert_eq!(
            config.verbs.action_of(key(53, false)),
            middle,
            "/ stays middle's"
        );
        let again = Config::parse(&config.render(), &us()).unwrap();
        assert_eq!(again.verbs, config.verbs);
        // Written as "?", it still leaves middle its default `/`.
        let written = Config::parse("[keys]\nhelp = \"?\"", &us()).unwrap();
        assert_eq!(written.verbs.action_of(key(53, false)), middle);
        let none = Config::parse("[keys]\nhelp = \"\"", &us()).unwrap();
        assert!(!none.verbs.is_help(key(53, true)));
        // `/` as a tag letter: its Shift is a Shift-click, so no help.
        let letter = Config::parse("[keys]\nalphabet = \"asdf/\"", &us()).unwrap();
        assert!(!letter.verbs.is_help(key(53, true)));
        let rendered = letter.render();
        assert!(rendered.contains("# help has no key"), "{rendered}");
        assert_eq!(Config::parse(&rendered, &us()).unwrap().verbs, letter.verbs);
        let legend = config.verbs.legend();
        assert!(legend.starts_with("tag\tleft click\n"), "{legend}");
        assert!(legend.contains(", tag\tright click"), "{legend}");
    }

    #[test]
    fn each_line_of_the_input_box_can_be_hidden_on_its_own() {
        let both = Config::parse("", &us()).unwrap();
        assert!(both.input.visible && both.info.visible, "drawn by default");
        for (file, input, info) in [
            ("[input]\nvisible = false", false, true),
            ("[info]\nvisible = false", true, false),
            (
                "[input]\nvisible = false\n[info]\nvisible = false",
                false,
                false,
            ),
        ] {
            let config = Config::parse(file, &us()).unwrap();
            assert_eq!((config.input.visible, config.info.visible), (input, info));
            assert!(config.input.hints, "where letters go is apart: {file}");
            let again = Config::parse(&config.render(), &us()).unwrap();
            assert_eq!((again.input.visible, again.info.visible), (input, info));
        }
    }

    #[test]
    fn the_placeholder_is_text_unless_set() {
        assert_eq!(Config::parse("", &us()).unwrap().input.placeholder, "text");
        let config = Config::parse("[text.input]\nplaceholder = \"find…\"", &us()).unwrap();
        assert_eq!(config.input.placeholder, "find…");
        let again = Config::parse(&config.render(), &us()).unwrap();
        assert_eq!(again.input, config.input);
        let err = format!(
            "{:#}",
            Config::parse("[text.input]\nplaceholder = \"a\\nb\"", &us()).unwrap_err()
        );
        assert!(err.contains("single-line"), "{err}");
    }
}
