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
};

use anyhow::{Context as _, Result, anyhow, bail};
use serde::{Deserialize, Serialize};

use crate::{
    action::Action,
    keys::{self, HOME_ROW, Key, KeyCode, Layout},
    style::Style,
};

/// The file as written. `#[serde(default)]` lets every table and field be
/// left out; `Default` says what "left out" means.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct File {
    keys: FileKeys,
    style: Style,
    pointer: FilePointer,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct FileKeys {
    /// Tag letters in priority order, as the first layout prints them.
    alphabet: Option<String>,
    /// xkb layout to decode keys with; unset means Hyprland's first.
    layout: Option<String>,
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
}

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

/// Keys every mode reads itself, so no verb may take them.
const BUILT_IN: [KeyCode; 5] = [
    KeyCode::ESC,
    KeyCode::BACKSPACE,
    KeyCode::SPACE,
    KeyCode::ENTER,
    KeyCode::TAB,
];

/// Everything wayhint needs from the settings, checked.
#[derive(Debug)]
pub struct Config {
    pub alphabet: Alphabet,
    pub verbs: Verbs,
    /// The layout the keys were decoded with, for `wayhint config`.
    pub layout: String,
    pub style: Style,
    pub after: After,
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
    /// Whether `wayhint config` can write the letters back as they are.
    /// Always for letters the file wrote. The default home row can hold a
    /// position that types nothing on this layout (a dead key on Ethiopic
    /// and Braille layouts), drawn as `?`, which reads back as another key.
    writable: bool,
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
            None => HOME_ROW
                .iter()
                .map(|&code| Letter {
                    code,
                    label: layout.char_at(code).unwrap_or('?'),
                })
                .collect(),
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
        let writable = letters
            .iter()
            .all(|l| decode_unshifted(l.label, layout).is_ok_and(|code| code == l.code));
        let command_of = |code: KeyCode| taken.iter().find(|t| t.0 == code).map(|t| t.1);
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
            writable,
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
    /// `None` for a default key that has no character which reads back as
    /// that same key (see `resolve`).
    keys: Vec<(Action, Key, Option<String>)>,
    /// Verbs left without a key because their default key is a tag key,
    /// with that default, for `wayhint config`.
    unbound: Vec<(Action, String)>,
    /// The repeat key and how it was written, if the file gives one. It
    /// lives with the verbs because it's checked against them: one key,
    /// one command.
    repeat: Option<(Key, String)>,
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

    /// The key that arms `action`, as `wayhint config` prints it (`k`,
    /// `_`, `Home`), for the prefix drawn in front of the tags. `None` for
    /// a verb with no key, such as the default action.
    pub fn label(&self, action: Action) -> Option<&str> {
        self.keys
            .iter()
            .find(|v| v.0 == action)
            .and_then(|v| v.2.as_deref())
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
        let mut keys: Vec<(Action, Key, Option<String>)> = Vec::new();
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
                    (key, Some(label))
                }
                (None, Some(code)) => {
                    let key = Key { code, shift: false };
                    // The character this position prints, kept only if
                    // writing it back gives this same key. On French AZERTY
                    // the `[` position is a dead circumflex and prints
                    // nothing; a "?" stand-in read back as Shift and the `,`
                    // position, and `wayhint config` moved back there
                    // (audit, 2026-10-23).
                    let label = layout
                        .char_at(code)
                        .filter(|&c| layout.decode(c) == Some(key))
                        .map(String::from);
                    if tag_keys.contains(&code) {
                        // A tag key is a letter of the alphabet, so it prints.
                        unbound.push((action, label.unwrap_or_default()));
                        continue;
                    }
                    (key, label)
                }
            };
            // For the messages below.
            let shown = label.as_deref().unwrap_or("its default key");
            if BUILT_IN.contains(&key.code) {
                bail!(
                    "[keys] {field} = {shown:?}: Esc, Backspace, Space, Enter and Tab are built in"
                );
            }
            if let Some(other) = keys.iter().find(|v| v.1 == key) {
                bail!("[keys] {} and {field} are both {shown:?}", other.0.name());
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
                    bail!(
                        "[keys] repeat = {text:?}: Esc, Backspace, Space, Enter and Tab are built in"
                    );
                }
                if let Some(other) = keys.iter().find(|v| v.1 == key) {
                    bail!("[keys] {} and repeat are both {text:?}", other.0.name());
                }
                let label = keys::name_of(key.code).map_or_else(|| text.into(), String::from);
                Some((key, label))
            }
        };
        Ok(Self {
            default,
            keys,
            unbound,
            repeat,
        })
    }

    /// The bound keys' positions and their commands' names: these leave
    /// the alphabet. A shifted key takes its base key too, because Shift on
    /// a tag letter means Shift-click.
    fn taken(&self) -> Vec<(KeyCode, &'static str)> {
        self.keys
            .iter()
            .map(|v| (v.1.code, v.0.name()))
            .chain(self.repeat.iter().map(|(key, _)| (key.code, "repeat")))
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
    /// is an error. `first_layout` is only called when the file doesn't name
    /// a layout, so a config with `layout = "us"` never asks Hyprland.
    pub fn load(
        explicit: Option<&Path>,
        first_layout: impl FnOnce() -> Result<String>,
    ) -> Result<Self> {
        let path = match explicit {
            Some(path) => path.to_path_buf(),
            None => match default_path() {
                Some(path) if path.exists() => path,
                _ => return Self::parse("", first_layout),
            },
        };
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading config {}", path.display()))?;
        Self::parse(&text, first_layout).with_context(|| format!("config {}", path.display()))
    }

    /// Parses and validates config text. Separate from `load` so tests need
    /// no files.
    pub fn parse(text: &str, first_layout: impl FnOnce() -> Result<String>) -> Result<Self> {
        let mut file: File = toml::from_str(text)?;
        // `take` moves the name out and leaves `None`, so `file.keys` stays
        // whole for the verbs below to borrow.
        let layout_name = match file.keys.layout.take() {
            Some(name) => name,
            None => first_layout()
                .context("finding your keyboard layout (set [keys] layout to skip this)")?,
        };
        let layout = Layout::new(&layout_name)?;
        let letters = Alphabet::decode_letters(file.keys.alphabet.as_deref(), &layout)?;
        // The keys the file made tag keys, so default verbs can stay off
        // them. A broken alias is skipped here; `Alphabet::resolve` says why.
        let tag_keys: Vec<KeyCode> = letters
            .iter()
            .map(|l| l.code)
            .chain(file.keys.aliases.keys().filter_map(|from| {
                let ch = single_char(from, "").ok()?;
                decode_unshifted(ch, &layout).ok()
            }))
            .collect();
        let verbs = Verbs::resolve(&file.keys, &layout, &tag_keys)?;
        let alphabet = Alphabet::resolve(letters, &file.keys.aliases, &layout, &verbs.taken())?;
        file.style.validate()?;
        Ok(Self {
            alphabet,
            verbs,
            layout: layout_name,
            style: file.style,
            after: file.pointer.after,
        })
    }

    /// The config in effect, as a TOML file you could save: `wayhint config`.
    pub fn render(&self) -> String {
        let mut out =
            String::from("# wayhint config in effect: defaults merged with your file\n\n[keys]\n");
        let alphabet: String = self.alphabet.letters.iter().map(|l| l.label).collect();
        if self.alphabet.writable {
            let _ = writeln!(
                out,
                "alphabet = {}   # priority order: earlier letters are used more",
                quote(&alphabet)
            );
        } else {
            // Left out, it reads back as the same default.
            let _ = writeln!(
                out,
                "# alphabet: the home row, drawn {alphabet}; some of its keys type no character here"
            );
        }
        let _ = writeln!(
            out,
            "layout = {}         # keys above are decoded with this layout",
            quote(&self.layout)
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
            } else if let Some((_, _, label)) = self.verbs.keys.iter().find(|v| v.0 == action) {
                // A default key with nothing to write it with stays out:
                // left out, it reads back as the same default.
                let _ = match label {
                    Some(label) => writeln!(out, "{name} = {}", quote(label)),
                    None => writeln!(
                        out,
                        "# {name} keeps its default key, which types no character on this layout"
                    ),
                };
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
        let after = match self.after {
            After::Restore => {
                "\"restore\"   # the cursor goes back; \"leave\" keeps it on the target"
            }
            After::Leave => {
                "\"leave\"   # the cursor stays on the target; \"restore\" puts it back"
            }
        };
        let _ = write!(out, "\n[pointer]\nafter = {after}\n");
        out
    }
}

/// `text` as a TOML string, quoted and escaped. Rust's `{:?}` looks the
/// same for plain text, but writes a combining mark as `\u{17d2}`, which
/// TOML can't read: the exported config of Khmer, Thai and eight other
/// layouts didn't load (found 2026-10-23).
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

    #[allow(clippy::unnecessary_wraps)] // must match `first_layout`'s signature
    fn us() -> Result<String> {
        Ok("us".into())
    }

    fn labels(config: &Config) -> String {
        config.alphabet.letters().iter().map(|l| l.label).collect()
    }

    #[test]
    fn empty_file_is_all_defaults() {
        let config = Config::parse("", us).unwrap();
        assert_eq!(labels(&config), "asdfghjkl");
        assert_eq!(config.layout, "us");
    }

    #[test]
    fn alphabet_is_decoded_in_order() {
        let config = Config::parse("[keys]\nalphabet = \"ciaehtns\"", us).unwrap();
        assert_eq!(labels(&config), "ciaehtns");
        assert_eq!(config.alphabet.letters()[0].code, KeyCode::from_evdev(46));
    }

    #[test]
    fn aliases_type_their_letter() {
        let config = Config::parse(
            "[keys]\nalphabet = \"ciaehtns\"\n[keys.aliases]\n\";\" = \"c\"",
            us,
        )
        .unwrap();
        assert_eq!(config.alphabet.index_of(KeyCode::from_evdev(39)), Some(0));
    }

    #[test]
    fn layout_in_file_skips_the_lookup() {
        let config =
            Config::parse("[keys]\nlayout = \"gr\"", || bail!("must not be called")).unwrap();
        assert_eq!(labels(&config), "ασδφγηξκλ");
    }

    #[test]
    fn broken_files_say_why() {
        let err = |text: &str| format!("{:#}", Config::parse(text, us).unwrap_err());
        assert!(err("[keys]\nalpahbet = \"ab\"").contains("unknown field"));
        assert!(err("[keys\n").contains("TOML") || err("[keys\n").contains("expected"));
        assert!(err("[keys]\nalphabet = \"aA\"").contains("needs Shift"));
        assert!(err("[keys]\nalphabet = \"aa\"").contains("same key"));
        assert!(err("[keys]\nalphabet = \"a\"").contains("at least two"));
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
        let config = Config::parse("", us).unwrap();
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
            us,
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
            us,
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
        let config = Config::parse("[keys]\nalphabet = \"ehatincsulodypmgfbwv\"", us).unwrap();
        assert_eq!(labels(&config), "ehatincsulodypmgfbwv");
        assert_eq!(config.verbs.action_of(key(50, false)), None);
        assert_eq!(config.verbs.action_of(key(19, false)), Some(Action::Right));
        let rendered = config.render();
        assert!(rendered.contains("# middle has no key: its default, \"m\", is one of your"));
        let again = Config::parse(&rendered, || bail!("layout is in the file")).unwrap();
        assert_eq!(again.verbs, config.verbs);
    }

    #[test]
    fn broken_verbs_say_why() {
        let err = |text: &str| format!("{:#}", Config::parse(text, us).unwrap_err());
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
        assert!(err("[pointer]\nafter = \"stay\"").contains("unknown variant"));
    }

    #[test]
    fn the_repeat_key_takes_its_letter() {
        let config = Config::parse("[keys]\nalphabet = \"asdf\"\nrepeat = \"f\"", us).unwrap();
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
        let off = Config::parse("[keys]\nrepeat = \"\"", us).unwrap();
        assert!(!off.verbs.is_repeat(f));
    }

    #[test]
    fn rendered_config_reads_back_the_same() {
        let text = "[keys]\nalphabet = \"ciaehtns\"\nright = \"\"\nleft = \".\"\nhover = '\"'\nrepeat = \"q\"\n\
                    [keys.aliases]\n\";\" = \"c\"\n[style]\nsize = 15\n[pointer]\nafter = \"leave\"";
        let config = Config::parse(text, us).unwrap();
        let again = Config::parse(&config.render(), || bail!("layout is in the file")).unwrap();
        assert_eq!(again.alphabet, config.alphabet);
        assert_eq!(again.verbs, config.verbs);
        assert_eq!(again.style, config.style);
        assert_eq!(again.after, config.after);
    }

    #[test]
    fn defaults_read_back_the_same_on_every_kind_of_layout() {
        // Khmer, Thai and Lao alphabets hold combining marks; Ethiopic and
        // Braille have home-row keys that type nothing. Each broke the
        // exported config once (2026-10-23).
        for layout in ["us", "fr", "de", "gr", "ru", "kh", "th", "la", "et", "brai"] {
            let config = Config::parse("", || Ok(layout.into())).unwrap();
            let rendered = config.render();
            let again = Config::parse(&rendered, || bail!("layout is in the file"))
                .unwrap_or_else(|e| panic!("{layout}: {e:#}\n{rendered}"));
            assert_eq!(again.alphabet, config.alphabet, "{layout}");
            assert_eq!(again.verbs, config.verbs, "{layout}");
        }
    }

    #[test]
    fn missing_explicit_file_is_an_error() {
        assert!(Config::load(Some(Path::new("/nonexistent/wayhint.toml")), us).is_err());
    }
}
