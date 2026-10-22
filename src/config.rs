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

use anyhow::{Context as _, Result, bail};
use serde::Deserialize;

use crate::{
    keys::{HOME_ROW, KeyCode, Layout},
    style::Style,
};

/// The file as written. `#[serde(default)]` lets every table and field be
/// left out; `Default` says what "left out" means.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct File {
    keys: FileKeys,
    style: Style,
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
}

/// Everything wayhint needs from the settings, checked.
#[derive(Debug)]
pub struct Config {
    pub alphabet: Alphabet,
    /// The layout the keys were decoded with, for `wayhint config`.
    pub layout: String,
    pub style: Style,
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

    /// Decodes and checks `[keys] alphabet` and `[keys.aliases]`.
    fn resolve(
        written: Option<&str>,
        aliases: &BTreeMap<String, String>,
        layout: &Layout,
    ) -> Result<Self> {
        let letters = match written {
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
        if letters.len() < 2 {
            bail!(
                "[keys] alphabet: needs at least two letters, got {}",
                letters.len()
            );
        }

        let mut alphabet = Self {
            letters,
            aliases: Vec::new(),
        };
        for (from, to) in aliases {
            let label = single_char(from, "[keys.aliases]")?;
            let code = decode_unshifted(label, layout).context("[keys.aliases]")?;
            let target = decode_unshifted(single_char(to, "[keys.aliases]")?, layout)
                .context("[keys.aliases]")?;
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
        let file: File = toml::from_str(text)?;
        let layout_name = match file.keys.layout {
            Some(name) => name,
            None => first_layout()
                .context("finding your keyboard layout (set [keys] layout to skip this)")?,
        };
        let layout = Layout::new(&layout_name)?;
        let alphabet =
            Alphabet::resolve(file.keys.alphabet.as_deref(), &file.keys.aliases, &layout)?;
        file.style.validate()?;
        Ok(Self {
            alphabet,
            layout: layout_name,
            style: file.style,
        })
    }

    /// The config in effect, as a TOML file you could save: `wayhint config`.
    pub fn render(&self) -> String {
        let mut out =
            String::from("# wayhint config in effect: defaults merged with your file\n\n[keys]\n");
        let alphabet: String = self.alphabet.letters.iter().map(|l| l.label).collect();
        let _ = writeln!(
            out,
            "alphabet = {alphabet:?}   # priority order: earlier letters are used more"
        );
        let _ = writeln!(
            out,
            "layout = {:?}         # keys above are decoded with this layout",
            self.layout
        );
        if !self.alphabet.aliases.is_empty() {
            out.push_str("\n[keys.aliases]\n");
            for alias in &self.alphabet.aliases {
                // `{:?}` of a `String` quotes and escapes it the way TOML
                // reads it; of a `char` it would print `'c'`, which TOML doesn't.
                let letter = self.alphabet.letters[alias.letter].label.to_string();
                let _ = writeln!(out, "{:?} = {letter:?}", alias.label.to_string());
            }
        }
        out.push('\n');
        out.push_str(&self.style.render());
        out
    }
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

    #[test]
    fn rendered_config_reads_back_the_same() {
        let text =
            "[keys]\nalphabet = \"ciaehtns\"\n[keys.aliases]\n\";\" = \"c\"\n[style]\nsize = 15";
        let config = Config::parse(text, us).unwrap();
        let again = Config::parse(&config.render(), || bail!("layout is in the file")).unwrap();
        assert_eq!(again.alphabet, config.alphabet);
        assert_eq!(again.style, config.style);
    }

    #[test]
    fn missing_explicit_file_is_an_error() {
        assert!(Config::load(Some(Path::new("/nonexistent/wayhint.toml")), us).is_err());
    }
}
