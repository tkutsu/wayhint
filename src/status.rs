//! The input box as data: what the overlay draws in it, and what `feed`
//! sends to another program, Quickshell's bar for one, to show its own
//! way. Both read the same [`Status`], so they can't disagree.
//!
//! The first line is a list of segments, each with its role and whether
//! it's dimmed; the dimmed ones come first. Joined, they read as the box's
//! first line. The raw values come too: what's typed even when the tags
//! show it instead (`[input] hints = false`), the armed verb, a hold.

use serde::Serialize;

use crate::style::{Input, Notice};

/// The input box, whole: which mode, what's typed, its first line, the
/// match count, the message.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Status {
    /// `None` between rounds: while the windows are read, or a command is
    /// done.
    pub mode: Option<Mode>,
    /// The tag letters typed, in `[style] case`, or the text query.
    pub input: String,
    /// The armed verb's name.
    pub verb: Option<&'static str>,
    /// A hold keeps a button down: whatever is picked next lets go.
    pub holding: bool,
    /// The first line as the box shows it.
    pub segments: Vec<Segment>,
    /// Text mode's position among its matches, once something is typed.
    pub matches: Option<Matches>,
    /// The second line.
    pub info: Option<Message>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Tags,
    Text,
}

/// A run of the first line's text with one meaning.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Segment {
    pub role: Role,
    pub text: String,
    pub dimmed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// The armed verb's name, and a space when letters follow.
    Verb,
    /// `drop`, while a hold keeps a button down.
    Drop,
    /// The tag letters typed.
    TagInput,
    Query,
    /// `[input] placeholder`, while nothing is typed in text mode.
    Placeholder,
    /// `[input] prefix` and `suffix`, around the query.
    Prefix,
    Suffix,
    /// `  1/3` after the query, or `...` while it's being found.
    MatchCount,
}

/// Which match is current, from 1, of how many: both `None` while the
/// query's matches are being found (`pending`). No match is `0/0`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Matches {
    pub current: Option<usize>,
    pub total: Option<usize>,
    pub pending: bool,
}

/// What wayhint has to say, under the first line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Message {
    pub kind: Notice,
    pub text: String,
}

impl Message {
    pub fn new(kind: Notice, text: impl Into<String>) -> Self {
        Self {
            kind,
            text: text.into(),
        }
    }
}

impl Segment {
    fn new(role: Role, text: impl Into<String>, dimmed: bool) -> Self {
        Self {
            role,
            text: text.into(),
            dimmed,
        }
    }
}

impl Status {
    /// With the tags up: `typed`, the letters typed so far, show after
    /// `drop` while `holding`, else after the armed `verb`, both dimmed.
    /// Without `hints` the letters stay on the tags, and the line has only
    /// the verb.
    pub fn tags(typed: String, verb: Option<&'static str>, holding: bool, hints: bool) -> Self {
        let lead = if holding {
            Some((Role::Drop, "drop"))
        } else {
            verb.map(|verb| (Role::Verb, verb))
        };
        let shown = if hints { typed.as_str() } else { "" };
        let mut segments = Vec::new();
        if let Some((role, name)) = lead {
            let gap = if shown.is_empty() { "" } else { " " };
            segments.push(Segment::new(role, format!("{name}{gap}"), true));
        }
        if !shown.is_empty() {
            segments.push(Segment::new(Role::TagInput, shown, false));
        }
        Self {
            mode: Some(Mode::Tags),
            input: typed,
            verb,
            holding,
            segments,
            ..Self::default()
        }
    }

    /// Text mode: `drop` while `holding`, else the armed `verb`, dimmed,
    /// then the `query` between `[input] prefix` and `suffix`, and its
    /// `position`, the current match and how many there are; `None` while
    /// they're being found. With nothing typed, `[input] placeholder`,
    /// dimmed. `query` is `None` while the windows' text is being read.
    pub fn text(
        verb: Option<&'static str>,
        holding: bool,
        query: Option<&str>,
        position: Option<(usize, usize)>,
        input: &Input,
    ) -> Self {
        let mut segments = Vec::new();
        if holding {
            segments.push(Segment::new(Role::Drop, "drop ", true));
        } else if let Some(verb) = verb {
            segments.push(Segment::new(Role::Verb, format!("{verb} "), true));
        }
        let pending = Matches {
            current: None,
            total: None,
            pending: true,
        };
        let matches = match query {
            None => {
                segments.push(Segment::new(Role::Placeholder, &input.placeholder, true));
                segments.push(Segment::new(Role::MatchCount, " ...", true));
                Some(pending)
            }
            Some("") => {
                segments.push(Segment::new(Role::Placeholder, &input.placeholder, true));
                None
            }
            Some(query) => {
                let decorated = [
                    (Role::Prefix, input.prefix.as_str()),
                    (Role::Query, query),
                    (Role::Suffix, input.suffix.as_str()),
                ];
                segments.extend(
                    decorated
                        .into_iter()
                        .filter(|(_, text)| !text.is_empty())
                        .map(|(role, text)| Segment::new(role, text, false)),
                );
                let count = position.map_or_else(|| "...".into(), |(at, of)| format!("{at}/{of}"));
                segments.push(Segment::new(Role::MatchCount, format!("  {count}"), false));
                Some(position.map_or(pending, |(at, of)| Matches {
                    current: Some(at),
                    total: Some(of),
                    pending: false,
                }))
            }
        };
        Self {
            mode: Some(Mode::Text),
            input: query.unwrap_or_default().to_owned(),
            verb,
            holding,
            segments,
            matches,
            ..Self::default()
        }
    }

    /// The first line as one string, and how many of its leading
    /// characters are dimmed; `None` with nothing to show.
    pub fn line(&self) -> Option<(String, usize)> {
        if self.segments.is_empty() {
            return None;
        }
        let text = self.segments.iter().map(|s| s.text.as_str()).collect();
        let dimmed = self
            .segments
            .iter()
            .take_while(|s| s.dimmed)
            .map(|s| s.text.chars().count())
            .sum();
        Some((text, dimmed))
    }

    /// What the overlay draws of it: the first line only if `input`
    /// (`[input] visible`), the message only if `info` (`[info]
    /// visible`). The feed always gets the whole.
    pub fn visible(&self, input: bool, info: bool) -> Self {
        Self {
            segments: if input {
                self.segments.clone()
            } else {
                Vec::new()
            },
            info: self.info.clone().filter(|_| info),
            ..self.clone()
        }
    }
}

#[cfg(test)]
impl Status {
    /// A first line of plain `text`, and nothing else.
    pub fn plain(text: &str) -> Self {
        Self {
            segments: vec![Segment::new(Role::Query, text, false)],
            ..Self::default()
        }
    }

    /// Only a message.
    pub fn saying(kind: Notice, text: &str) -> Self {
        Self {
            info: Some(Message::new(kind, text)),
            ..Self::default()
        }
    }

    /// A first line of `dimmed` then `rest`.
    pub fn dimmed(dimmed: &str, rest: &str) -> Self {
        Self {
            segments: vec![
                Segment::new(Role::Verb, dimmed, true),
                Segment::new(Role::Query, rest, false),
            ],
            ..Self::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::style::Style;

    fn roles(status: &Status) -> Vec<(Role, &str, bool)> {
        status
            .segments
            .iter()
            .map(|s| (s.role, s.text.as_str(), s.dimmed))
            .collect()
    }

    /// The dimmed segments lead, as the overlay draws only a dimmed start.
    fn dimmed_first(status: &Status) -> bool {
        status
            .segments
            .iter()
            .skip_while(|s| s.dimmed)
            .all(|s| !s.dimmed)
    }

    #[test]
    fn tag_letters_follow_the_verb_or_drop() {
        let armed = Status::tags("AB".into(), Some("right"), false, true);
        assert_eq!(
            roles(&armed),
            [(Role::Verb, "right ", true), (Role::TagInput, "AB", false)]
        );
        assert_eq!(armed.line(), Some(("right AB".into(), 6)));
        let held = Status::tags("AB".into(), Some("right"), true, true);
        assert_eq!(
            held.line(),
            Some(("drop AB".into(), 5)),
            "drop beats a verb"
        );
        assert_eq!(held.verb, Some("right"), "still armed underneath");
        assert_eq!(
            Status::tags(String::new(), None, false, true).line(),
            None,
            "nothing typed, nothing to show"
        );
        assert_eq!(
            Status::tags("A".into(), None, false, true).line(),
            Some(("A".into(), 0))
        );
    }

    #[test]
    fn without_hints_the_letters_stay_out_of_the_line_but_not_the_input() {
        let status = Status::tags("AB".into(), Some("right"), false, false);
        assert_eq!(status.line(), Some(("right".into(), 5)));
        assert_eq!(status.input, "AB");
        assert_eq!(Status::tags("AB".into(), None, false, false).line(), None);
    }

    #[test]
    fn text_mode_reads_then_counts() {
        let input = Input {
            prefix: "find [".into(),
            suffix: "]".into(),
            ..Input::new(&Style::default())
        };
        let reading = Status::text(Some("double"), false, None, None, &input);
        assert_eq!(reading.line(), Some(("double text ...".into(), 15)));
        assert!(reading.matches.unwrap().pending);
        let holding = Status::text(Some("double"), true, Some(""), None, &input);
        assert_eq!(
            holding.line(),
            Some(("drop text".into(), 9)),
            "drop beats a verb"
        );
        assert!(holding.holding);
        let empty = Status::text(None, false, Some(""), Some((0, 0)), &input);
        assert_eq!(empty.line(), Some(("text".into(), 4)));
        assert_eq!(empty.matches, None, "nothing typed, nothing counted");
        let found = Status::text(Some("double"), false, Some("αλλ"), Some((1, 2)), &input);
        assert_eq!(
            roles(&found),
            [
                (Role::Verb, "double ", true),
                (Role::Prefix, "find [", false),
                (Role::Query, "αλλ", false),
                (Role::Suffix, "]", false),
                (Role::MatchCount, "  1/2", false),
            ]
        );
        assert_eq!(found.input, "αλλ", "decorations are display only");
        assert_eq!(
            found.matches,
            Some(Matches {
                current: Some(1),
                total: Some(2),
                pending: false
            })
        );
        let finding = Status::text(None, false, Some("αλλ"), None, &input);
        assert_eq!(finding.line().unwrap().0, "find [αλλ]  ...");
        assert_eq!(
            finding.matches,
            Some(Matches {
                current: None,
                total: None,
                pending: true
            }),
            "unknown counts are null, not zero"
        );
        let plain = Status::text(
            None,
            false,
            Some("x"),
            Some((0, 0)),
            &Input::new(&Style::default()),
        );
        assert_eq!(
            roles(&plain),
            [
                (Role::Query, "x", false),
                (Role::MatchCount, "  0/0", false)
            ],
            "no empty decorations"
        );
        for status in [reading, empty, found, finding, plain] {
            assert!(dimmed_first(&status), "{status:?}");
        }
    }

    #[test]
    fn hiding_a_line_keeps_the_rest() {
        let status = Status {
            info: Some(Message::new(Notice::NoMatch, "no tag starts with Q")),
            ..Status::tags("A".into(), Some("right"), false, true)
        };
        assert_eq!(status.visible(true, true), status);
        let no_input = status.visible(false, true);
        assert_eq!(no_input.line(), None);
        assert_eq!(no_input.info, status.info);
        let no_info = status.visible(true, false);
        assert_eq!(no_info.segments, status.segments);
        assert_eq!(no_info.info, None);
        let neither = status.visible(false, false);
        assert_eq!((neither.line(), neither.info), (None, None));
        assert_eq!(neither.input, "A", "the raw values stay");
    }

    #[test]
    fn it_reads_as_json_a_bar_can_use() {
        let status = Status {
            info: Some(Message::new(Notice::NoTargets, "no targets")),
            ..Status::tags("AB".into(), Some("right"), false, true)
        };
        let json = serde_json::to_value(&status).unwrap();
        assert_eq!(json["mode"], "tags");
        assert_eq!(json["segments"][0]["role"], "verb");
        assert_eq!(json["segments"][1]["role"], "tag_input");
        assert_eq!(json["matches"], serde_json::Value::Null);
        assert_eq!(json["info"]["kind"], "no_targets");
        assert_eq!(
            serde_json::to_value(Status::default()).unwrap()["mode"],
            serde_json::Value::Null
        );
    }
}
