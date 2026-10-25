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
    style::{Color, Mark, Marks, Style},
};

/// The file as written. `#[serde(default)]` lets every table and field be
/// left out; `Default` says what "left out" means.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct File {
    keys: FileKeys,
    style: Style,
    pointer: FilePointer,
    pixels: FilePixels,
    scan: FileScan,
    text: FileText,
}

/// `[text]`: how text mode looks.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct FileText {
    style: FileMarks,
}

/// `[text.style]`, every match's mark, with `[text.style.current]` inside
/// it for the selected match's. Unset fields come from the tags' style
/// (see [`Mark::from_tags`]); unset current ones from every match's mark,
/// made stronger ([`Mark::emphasized`]).
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
    /// The marks, with whatever the file leaves out filled in from the
    /// tags' `style`.
    fn resolve(&self, style: &Style) -> Result<Marks> {
        let tags = Mark::from_tags(style);
        let all = Mark {
            background: self.background.unwrap_or(tags.background),
            border: self.border.unwrap_or(tags.border),
            border_width: self.border_width.unwrap_or(tags.border_width),
            radius: self.radius.unwrap_or(tags.radius),
            padding: self.padding.unwrap_or(tags.padding),
        };
        let stronger = all.emphasized();
        let c = &self.current;
        let current = Mark {
            background: c.background.unwrap_or(stronger.background),
            border: c.border.unwrap_or(stronger.border),
            border_width: c.border_width.unwrap_or(stronger.border_width),
            radius: c.radius.unwrap_or(stronger.radius),
            padding: c.padding.unwrap_or(stronger.padding),
        };
        all.validate("text.style")?;
        current.validate("text.style.current")?;
        Ok(Marks { all, current })
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct FileScan {
    /// Ms from the end of one scan to the next while the tags are up, `0`
    /// for no rescans but the one after each click.
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
    terminal_processes: Vec<String>,
}

impl Default for FilePixels {
    fn default() -> Self {
        Self {
            detector: Pixels::default(),
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
    let path = explicit
        .map(Path::to_path_buf)
        .or_else(|| default_path().filter(|p| p.exists()));
    let Some(path) = path else {
        return Ok(FilePixels::default().terminal_processes);
    };
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("reading config {}", path.display()))?;
    let file: File = toml::from_str(&text).with_context(|| format!("config {}", path.display()))?;
    file.pixels.validate()?;
    Ok(file.pixels.terminal_processes)
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
    /// Tag letters in priority order, as the first layout prints them.
    alphabet: Option<String>,
    /// Other keys that count as an alphabet letter: `";" = "c"`.
    aliases: BTreeMap<String, String>,
    // One field per verb: a key as written (`"."`, `"Home"`), or `""` for
    // the action a bare tag does. `Action::ALL` lists the same seven.
    left: Option<String>,
    right: Option<String>,
    middle: Option<String>,
    double: Option<String>,
    hover: Option<String>,
    back: Option<String>,
    forward: Option<String>,
    /// Does the last action again, on the same target. No default key;
    /// `""` is the same as leaving it out.
    repeat: Option<String>,
    /// Text mode: the tags go and typing finds text on screen. `""` for
    /// none.
    text: Option<String>,
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
}

/// How far one arrow press moves a command, in px, unless the file says.
const NUDGE: u16 = 10;

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

/// The verbs' default keys, as positions, so they sit in the same place on
/// every layout, as the home row does. On US they print `r m t o b n`:
/// right, middle, twice, over, back, next. Mnemonic letters on the home row
/// (`d`, `h`, `f`) would clash with the default alphabet. Left has none: a
/// bare tag left-clicks.
const DEFAULT_VERBS: [(Action, Option<KeyCode>); 7] = [
    (Action::Left, None),
    (Action::Right, Some(KeyCode::from_evdev(19))),
    (Action::Middle, Some(KeyCode::from_evdev(50))),
    (Action::Double, Some(KeyCode::from_evdev(20))),
    (Action::Hover, Some(KeyCode::from_evdev(24))),
    (Action::Back, Some(KeyCode::from_evdev(48))),
    (Action::Forward, Some(KeyCode::from_evdev(49))),
];

/// The text key's default, as a position, as the verbs' are: `q` on US.
const TEXT_KEY: KeyCode = KeyCode::from_evdev(16);

/// Keys every mode reads itself, so no verb may take them, and how the
/// message that says so lists them.
const BUILT_IN: [KeyCode; 9] = [
    KeyCode::ESC,
    KeyCode::BACKSPACE,
    KeyCode::SPACE,
    KeyCode::ENTER,
    KeyCode::TAB,
    KeyCode::UP,
    KeyCode::DOWN,
    KeyCode::LEFT,
    KeyCode::RIGHT,
];
const BUILT_IN_NAMES: &str = "Esc, Backspace, Space, Enter, Tab and the arrows";

/// Everything wayhint needs from the settings, checked.
#[derive(Debug)]
pub struct Config {
    pub alphabet: Alphabet,
    pub verbs: Verbs,
    /// The name of the layout the keys were decoded with ("English (US)"),
    /// for `wayhint config`.
    pub layout: String,
    pub style: Style,
    /// How text mode marks its matches: `[text.style]`.
    pub marks: Marks,
    pub after: After,
    /// Px one arrow press moves a command: `[pointer] nudge`.
    pub nudge: i32,
    /// How windows with no accessibility tree are read, `None` for not at
    /// all: `[pixels] detector`.
    pub pixels: Option<Detector>,
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
    /// Each bound verb, its key, and the key as `wayhint config` prints it.
    keys: Vec<(Action, Key, String)>,
    /// Verbs left without a key because their default key is a tag key,
    /// with that default, for `wayhint config`.
    unbound: Vec<(Action, String)>,
    /// The repeat key and how it was written, if the file gives one. It
    /// lives with the verbs because it's checked against them: one key,
    /// one command.
    repeat: Option<(Key, String)>,
    /// The text key and how it's printed, if it has one, and if it has
    /// none because its default is a tag key or a command's, that default.
    text: Option<(Key, String)>,
    text_unbound: Option<String>,
}

impl Verbs {
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

    /// Whether `key` is the text key.
    pub fn is_text(&self, key: Key) -> bool {
        self.text.as_ref().is_some_and(|(k, _)| *k == key)
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

    /// Decodes the seven verb fields, fills in the defaults, checks them.
    ///
    /// A default never takes a tag key: a verb the file doesn't bind gets
    /// its default key only if that isn't one of `tag_keys`, the alphabet's
    /// letters (yours, or the home row) and aliases. A verb key the file
    /// does write takes its letter out of the alphabet instead.
    fn resolve(file: &FileKeys, layout: &Layout, tag_keys: &[KeyCode]) -> Result<Self> {
        let mut defaults = Vec::new();
        let mut unbound = Vec::new();
        let mut keys: Vec<(Action, Key, String)> = Vec::new();
        for (action, default) in DEFAULT_VERBS {
            let field = action.name();
            let (key, label) = match (file.verb(action), default) {
                // `""`, written or by default: no key, a bare tag does it.
                (Some(""), _) | (None, None) => {
                    defaults.push(action);
                    continue;
                }
                (Some(text), _) => {
                    let key =
                        decode_key(text, layout).with_context(|| format!("[keys] {field}"))?;
                    // A name is printed back as the table spells it: "home" is "Home".
                    let label = keys::name_of(key.code).map_or_else(|| text.into(), String::from);
                    (key, label)
                }
                (None, Some(code)) => {
                    // The character this position prints, which every
                    // Latin layout has.
                    let label = layout.char_at(code).with_context(|| {
                        format!(
                            "[keys] {field}: its default key types nothing on layout {}; give it a key",
                            layout.name()
                        )
                    })?;
                    if tag_keys.contains(&code) {
                        unbound.push((action, label.into()));
                        continue;
                    }
                    (Key { code, shift: false }, label.into())
                }
            };
            if BUILT_IN.contains(&key.code) {
                bail!("[keys] {field} = {label:?}: {BUILT_IN_NAMES} are built in");
            }
            if let Some(other) = keys.iter().find(|v| v.1 == key) {
                bail!("[keys] {} and {field} are both {label:?}", other.0.name());
            }
            keys.push((action, key, label));
        }
        // A slice pattern names the three cases: one default, none, several.
        let default = match defaults.as_slice() {
            [one] => *one,
            [] => bail!("[keys]: no verb is the default; set one to \"\", as in left = \"\""),
            [a, b, ..] => bail!(
                "[keys] {} and {}: only one verb can be the default (\"\"); give {} a key",
                a.name(),
                b.name(),
                a.name()
            ),
        };
        let repeat = match file.repeat.as_deref() {
            None | Some("") => None,
            Some(text) => {
                let key = decode_key(text, layout).context("[keys] repeat")?;
                if BUILT_IN.contains(&key.code) {
                    bail!("[keys] repeat = {text:?}: {BUILT_IN_NAMES} are built in");
                }
                if let Some(other) = keys.iter().find(|v| v.1 == key) {
                    bail!("[keys] {} and repeat are both {text:?}", other.0.name());
                }
                let label = keys::name_of(key.code).map_or_else(|| text.into(), String::from);
                Some((key, label))
            }
        };
        let mut verbs = Self {
            default,
            keys,
            unbound,
            repeat,
            text: None,
            text_unbound: None,
        };
        verbs.resolve_text(file.text.as_deref(), layout, tag_keys)?;
        Ok(verbs)
    }

    /// The text key: as written, else its default position, unless that's
    /// a tag key or any command's. A default that came after the other
    /// commands mustn't break a file that bound its key to one of them.
    fn resolve_text(
        &mut self,
        written: Option<&str>,
        layout: &Layout,
        tag_keys: &[KeyCode],
    ) -> Result<()> {
        let key = match written {
            Some("") => return Ok(()),
            Some(text) => {
                let key = decode_key(text, layout).context("[keys] text")?;
                let label = keys::name_of(key.code).map_or_else(|| text.into(), String::from);
                (key, label)
            }
            None => {
                let label = layout.char_at(TEXT_KEY).with_context(|| {
                    format!(
                        "[keys] text: its default key types nothing on layout {}; give it a key",
                        layout.name()
                    )
                })?;
                let commands = self.taken();
                if tag_keys.contains(&TEXT_KEY) || commands.iter().any(|t| t.0 == TEXT_KEY) {
                    self.text_unbound = Some(label.into());
                    return Ok(());
                }
                (
                    Key {
                        code: TEXT_KEY,
                        shift: false,
                    },
                    label.into(),
                )
            }
        };
        let label = &key.1;
        if BUILT_IN.contains(&key.0.code) {
            bail!("[keys] text = {label:?}: {BUILT_IN_NAMES} are built in");
        }
        if let Some(other) = self.keys.iter().find(|v| v.1 == key.0) {
            bail!("[keys] {} and text are both {label:?}", other.0.name());
        }
        if self.is_repeat(key.0) {
            bail!("[keys] repeat and text are both {label:?}");
        }
        self.text = Some(key);
        Ok(())
    }

    /// The text key's line in `[keys]`, for `wayhint config`.
    fn render_text(&self) -> String {
        match (&self.text, &self.text_unbound) {
            (Some((_, label)), _) => format!(
                "text = {}   # text mode: type text on screen, Enter acts on it\n",
                quote(label)
            ),
            (None, Some(label)) => format!(
                "# text has no key: its default, {label:?}, is one of your tag letters or commands\n"
            ),
            (None, None) => "text = \"\"   # no text mode\n".into(),
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
            .chain(self.text.iter().map(|(key, _)| (key.code, "text")))
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
        let marks = file.text.style.resolve(&file.style)?;
        file.pixels.validate()?;
        let nudge = file.pointer.nudge.unwrap_or(NUDGE);
        if nudge == 0 {
            bail!("[pointer] nudge = 0: an arrow press must move at least 1 px");
        }
        Ok(Self {
            alphabet,
            verbs,
            layout: layout.name().to_string(),
            style: file.style,
            marks,
            after: file.pointer.after,
            nudge: nudge.into(),
            pixels: match file.pixels.detector {
                Pixels::Full => Some(Detector::Full),
                Pixels::Fast => Some(Detector::Fast),
                Pixels::Off => None,
            },
            terminal_processes: file.pixels.terminal_processes,
            refresh: match file.scan.refresh.unwrap_or(REFRESH) {
                0 => None,
                ms => Some(Duration::from_millis(ms.into())),
            },
            merge_distance: file.scan.merge_distance.unwrap_or(MERGE_DISTANCE),
        })
    }

    /// The config in effect, as a TOML file you could save: `wayhint config`.
    pub fn render(&self) -> String {
        let mut out =
            String::from("# wayhint config in effect: defaults merged with your file\n\n[keys]\n");
        let _ = writeln!(
            out,
            "alphabet = {}   # priority order: earlier letters are used more",
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
        for action in Action::ALL {
            let name = action.name();
            if action == self.verbs.default {
                let _ = writeln!(out, "{name} = \"\"   # a bare tag does this");
            } else if let Some(label) = self.verbs.label(action) {
                let _ = writeln!(out, "{name} = {}", quote(label));
            } else if let Some((_, label)) = self.verbs.unbound.iter().find(|v| v.0 == action) {
                let _ = writeln!(
                    out,
                    "# {name} has no key: its default, {label:?}, is one of your tag letters"
                );
            }
        }
        match &self.verbs.repeat {
            Some((_, label)) => {
                let _ = writeln!(out, "repeat = {}", quote(label));
            }
            None => out.push_str("# repeat has no key: give it one to do the last action again\n"),
        }
        out.push_str(&self.verbs.render_text());
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
        out.push('\n');
        out.push_str(&self.style.render());
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
            "\n[scan]\nrefresh = {refresh}   # ms the tags stay up before a rescan; 0 for only after a click\n"
        );
        let _ = writeln!(
            out,
            "merge_distance = {}   # logical px between nearby hint centers; 0 keeps every hint",
            self.merge_distance
        );
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
    fn default_verbs_sit_off_the_home_row() {
        let config = Config::parse("", &us()).unwrap();
        assert_eq!(config.verbs.default_action(), Action::Left);
        assert_eq!(config.verbs.action_of(key(19, false)), Some(Action::Right));
        assert_eq!(config.verbs.action_of(key(19, true)), None, "R isn't r");
        assert_eq!(labels(&config), "asdfghjkl", "nothing left out");
        assert_eq!(config.after, After::Restore);
    }

    #[test]
    fn the_plans_bindings_load() {
        let config = Config::parse(
            "[keys]\nalphabet = \"ehatincsulodypmgf\"\nright = \"\"\nleft = \".\"\n\
             double = \"k\"\nmiddle = \"j\"\nhover = \"_\"\nback = \"home\"\nforward = \"End\"\n\
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
        // `X` is shifted x: Shift on a tag letter is kept for Shift-click,
        // so the x position leaves the tags.
        let config = Config::parse(
            "[keys]\nalphabet = \"asdx\"\nhover = \"X\"\nright = \"q\"",
            &us(),
        )
        .unwrap();
        assert_eq!(labels(&config), "asd");
        assert!(
            config
                .render()
                .contains("# left out of the alphabet: x (hover)")
        );
    }

    #[test]
    fn a_default_verb_key_gives_way_to_written_letters() {
        // The file as it stood on 2026-10-23: an alphabet with m and o in
        // it, the verbs still commented out. Middle (m) and hover (o) lose
        // their defaults instead of taking two letters.
        let config = Config::parse("[keys]\nalphabet = \"ehatincsulodypmgfbwv\"", &us()).unwrap();
        assert_eq!(labels(&config), "ehatincsulodypmgfbwv");
        assert_eq!(config.verbs.action_of(key(50, false)), None);
        assert_eq!(config.verbs.action_of(key(19, false)), Some(Action::Right));
        let rendered = config.render();
        assert!(rendered.contains("# middle has no key: its default, \"m\", is one of your"));
        let again = Config::parse(&rendered, &us()).unwrap();
        assert_eq!(again.verbs, config.verbs);
    }

    #[test]
    fn broken_verbs_say_why() {
        let err = |text: &str| format!("{:#}", Config::parse(text, &us()).unwrap_err());
        assert!(err("[keys]\nright = \"\"").contains("only one verb can be the default"));
        assert!(err("[keys]\nleft = \".\"").contains("no verb is the default"));
        assert!(err("[keys]\nhover = \"esc\"").contains("built in"));
        assert!(err("[keys]\nright = \"x\"\nmiddle = \"x\"").contains("both"));
        assert!(err("[keys]\nback = \"PageUp\"").contains("the names are Up, Down"));
        assert!(err("[keys]\nhover = \"é\"").contains("isn't a key"));
        assert!(
            err("[keys]\nalphabet = \"ab\"\nhover = \"a\"")
                .contains("once command keys are left out")
        );
        assert!(err("[keys]\nright = \"r\"\n[keys.aliases]\n\"r\" = \"a\"").contains("right's"));
        assert!(err("[keys]\nright = \"x\"\nrepeat = \"x\"").contains("right and repeat"));
        assert!(err("[keys]\nrepeat = \"Enter\"").contains("built in"));
        assert!(err("[keys]\nback = \"Left\"").contains("and the arrows are built in"));
        assert!(err("[pointer]\nafter = \"stay\"").contains("unknown variant"));
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
    fn the_text_key_is_q_unless_a_tag_or_a_command_has_it() {
        let q = key(16, false);
        assert!(Config::parse("", &us()).unwrap().verbs.is_text(q));
        // Your file: q is a tag letter, so text has no key until it's given one.
        let tagged = Config::parse("[keys]\nalphabet = \"ehatincsulodypmgfxwbvq\"", &us()).unwrap();
        assert!(!tagged.verbs.is_text(q));
        assert!(labels(&tagged).ends_with('q'), "the letter stays");
        let rendered = tagged.render();
        assert!(
            rendered.contains("# text has no key: its default, \"q\""),
            "{rendered}"
        );
        assert_eq!(Config::parse(&rendered, &us()).unwrap().verbs, tagged.verbs);
        let repeat = Config::parse("[keys]\nrepeat = \"q\"", &us()).unwrap();
        assert!(repeat.verbs.is_repeat(q) && !repeat.verbs.is_text(q));
        let slash = Config::parse("[keys]\nalphabet = \"asdf/\"\ntext = \"/\"", &us()).unwrap();
        assert!(slash.verbs.is_text(key(53, false)));
        assert_eq!(
            labels(&slash),
            "asdf",
            "a written text key leaves the alphabet"
        );
        let off = Config::parse("[keys]\ntext = \"\"", &us()).unwrap();
        assert!(!off.verbs.is_text(q));
        assert_eq!(
            Config::parse(&off.render(), &us()).unwrap().verbs,
            off.verbs
        );
        let err = |text: &str| format!("{:#}", Config::parse(text, &us()).unwrap_err());
        assert!(err("[keys]\nright = \"x\"\ntext = \"x\"").contains("right and text"));
        assert!(err("[keys]\nrepeat = \"x\"\ntext = \"x\"").contains("repeat and text"));
        assert!(err("[keys]\ntext = \"Enter\"").contains("built in"));
    }

    #[test]
    fn text_marks_come_from_the_tags_unless_set() {
        let config = Config::parse("[style]\nborder_width = 0\nradius = 3", &us()).unwrap();
        let tags = Mark::from_tags(&config.style);
        assert_eq!(config.marks.all, tags);
        assert_eq!(config.marks.current, tags.emphasized());
        let widths = (
            config.marks.all.border_width,
            config.marks.current.border_width,
        );
        assert_eq!(
            widths,
            (1.0, 2.0),
            "tags with no border: marks get one anyway"
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
            set.marks.current.background.a, 0x70,
            "stronger than the set one"
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
        assert!(err("[text.style.current]\nradius = -1").contains("[text.style.current] radius"));
        assert!(err("[text.style]\nopacity = 0.5").contains("unknown field"));
        assert!(err("[text.style]\nborder = \"red\"").contains("isn't a colour"));
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
        // home row and the verb keys the most: AZERTY puts `,` on middle,
        // BÉPO puts `,` in the alphabet and `è` on double.
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
            let rendered = config.render();
            let again = Config::parse(&rendered, &layout)
                .unwrap_or_else(|e| panic!("{name}({variant}): {e:#}\n{rendered}"));
            assert_eq!(again.alphabet, config.alphabet, "{name}({variant})");
            assert_eq!(again.verbs, config.verbs, "{name}({variant})");
        }
    }

    #[test]
    fn missing_explicit_file_is_an_error() {
        assert!(Config::load(Some(Path::new("/nonexistent/wayhint.toml")), &us()).is_err());
    }
}
