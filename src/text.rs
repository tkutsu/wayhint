//! Text mode's matching. Pure: texts and a query in, ordered matches out.
//!
//! What you type and what the screen says rarely match byte for byte, so
//! both are folded the same way first:
//!
//! - Accents go: `é` is `e` plus a combining accent (Unicode's canonical
//!   decomposition), and the accent is dropped, so `α` finds `άλλο`.
//! - Case goes: a query finds text in any case. Shift is for verbs,
//!   never for the text.
//! - Every run of white space is one space, so a space finds a line break.
//!
//! AT-SPI counts in characters, Rust strings in bytes, and folding changes
//! lengths (`é` is two bytes, `e` one; `ß` lowercases to itself, `İ` to two
//! characters). So a folded text keeps, for each of its bytes, the original
//! character it came from: a match folds back to the original's character
//! offsets, which is what the app's geometry calls take.
//!
//! [`Search`] is the state of one text mode: the query, where its matches
//! are, which one is current. Where a match is comes from the app, one
//! request per match, so it's asked for only once ([`Search::wanted`]);
//! in a terminal, from the cells its characters sit in ([`Cells`]).

use std::{collections::HashMap, ops::Range};

use unicode_normalization::char::{decompose_canonical, is_combining_mark};

use crate::geometry::{Point, Rect};

/// A text this short is a label, a link or a cell: when the app can't say
/// where a match in it is, the object's own box is close enough.
const SHORT: usize = 100;

/// The smallest visible part of a match worth a highlight, in px.
const MIN_SIZE: i32 = 2;

/// One object's text, as read when text mode starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Text {
    /// Where it came from: what to ask where a match is.
    pub place: Place,
    /// Its window, as an index into the snapshot's windows.
    pub window: usize,
    /// Its accessible name, for the re-check.
    pub name: String,
    /// Its box, and the part of the screen it can show in (its window, cut
    /// by the scroll areas above it). Global.
    pub rect: Rect,
    pub area: Rect,
    /// The character offset of `content` in the object: a long text is
    /// read from where its visible part starts.
    pub start: u32,
    pub content: String,
}

impl Text {
    /// Its characters `start..end`, counted from the start of the object.
    pub fn chars(&self, start: u32, end: u32) -> String {
        let skip = start.saturating_sub(self.start) as usize;
        let take = end.saturating_sub(start) as usize;
        self.content.chars().skip(skip).take(take).collect()
    }
}

/// Where a text came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Place {
    /// An accessible object, as an index into
    /// [`crate::snapshot::Snapshot::texts`]: the app says where a match is.
    Object(usize),
    /// A terminal's cells: where a match is is arithmetic.
    Cells(Cells),
}

/// Characters `start..end` of the text at `place`, and what they held:
/// what a match must still read before it's acted on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    pub place: Place,
    pub start: u32,
    pub end: u32,
    pub text: String,
}

/// A terminal pane's text laid out in its cells. A terminal has no
/// accessibility tree, but it knows its grid: kitty says what each row
/// shows and where its first cell is (see `desktop::kitty`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cells {
    /// The pane, as kitty numbers it, to read again before acting.
    pub pane: u64,
    /// Its window's top-left, global logical px.
    pub window: Point,
    /// The first cell's top-left in the window, and a cell's size, in the
    /// window's physical px.
    pub first: Point,
    pub cell: (i32, i32),
    /// Physical px to logical px: `scale.0` of the first make `scale.1`.
    pub scale: (i32, i32),
    /// For each character of the text: its row, and the columns
    /// `start..end` it covers. A line break covers none.
    pub at: Vec<(u16, u16, u16)>,
}

impl Cells {
    /// Where characters `start..end` are: a box per row, global.
    pub fn boxes(&self, start: u32, end: u32) -> Vec<Rect> {
        let mut rows: Vec<(u16, u16, u16)> = Vec::new();
        let chars = self.at.iter().skip(start as usize);
        for &(row, from, to) in chars.take(end.saturating_sub(start) as usize) {
            match rows.last_mut() {
                _ if to <= from => {}
                Some(last) if last.0 == row => last.2 = last.2.max(to),
                _ => rows.push((row, from, to)),
            }
        }
        rows.into_iter()
            .map(|(row, from, to)| {
                let (x, y) = (i32::from(from), i32::from(row));
                self.rect(x, y, i32::from(to) - x, 1)
            })
            .collect()
    }

    /// Whether characters `start..end` sit in the same cells, at the same
    /// place on screen, in `other`.
    pub fn same_place(&self, other: &Self, start: u32, end: u32) -> bool {
        let range = start as usize..end as usize;
        (self.window, self.first, self.cell, self.scale)
            == (other.window, other.first, other.cell, other.scale)
            && self.at.get(range.clone()) == other.at.get(range)
    }

    /// The box of every row and column with a character in it.
    pub fn whole(&self) -> Rect {
        let rows = self.at.last().map_or(0, |&(row, ..)| row + 1);
        let columns = self.at.iter().map(|&(.., to)| to).max().unwrap_or(0);
        self.rect(0, 0, i32::from(columns), i32::from(rows))
    }

    /// Columns `x..x + w` of rows `y..y + h`, global logical px.
    fn rect(&self, x: i32, y: i32, w: i32, h: i32) -> Rect {
        let (cw, ch) = self.cell;
        #[allow(clippy::cast_possible_truncation)]
        let logical = |px: i32| {
            (f64::from(px) * f64::from(self.scale.1) / f64::from(self.scale.0)).round() as i32
        };
        let left = logical(self.first.x + x * cw);
        let top = logical(self.first.y + y * ch);
        let right = logical(self.first.x + (x + w) * cw);
        let bottom = logical(self.first.y + (y + h) * ch);
        Rect::new(left, top, right - left, bottom - top).offset(self.window)
    }
}

/// Where a match is: characters `start..end` of text `text`, counted from
/// the start of the object (not of what was read of it).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Span {
    pub text: usize,
    pub start: u32,
    pub end: u32,
}

/// One match on screen: its span and its visible parts, a line each, in
/// reading order. Never empty.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Match {
    pub span: Span,
    pub parts: Vec<Rect>,
}

impl Match {
    /// Where a click on it goes: the middle of its first visible part.
    pub fn point(&self) -> Point {
        self.parts[0].center()
    }
}

/// Whether `c` is part of a word: letters, numbers and `_`. Anything else
/// (spaces, `-`, `/`, brackets) parts words, except an apostrophe inside
/// one: `can't` is one word.
fn in_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// A text folded for matching, with the way back to the original.
#[derive(Debug, Clone)]
pub struct Folded {
    text: String,
    /// For each byte of `text`, the original character it came from.
    from: Vec<u32>,
    /// Where each word starts in `text`, in bytes, first to last.
    words: Vec<usize>,
}

impl Folded {
    /// `text` folded, lower case.
    pub fn new(text: &str) -> Self {
        let mut folded = Self {
            text: String::with_capacity(text.len()),
            from: Vec::with_capacity(text.len()),
            words: Vec::new(),
        };
        let mut after_space = false;
        // Words are found in the original, before accents go: `é` is one
        // word character, never a base letter and a separator.
        let mut word = false;
        // `zip(0..)` counts characters as `u32`, the width AT-SPI uses.
        for (c, at) in text.chars().zip(0u32..) {
            if in_word(c) {
                if !word {
                    folded.words.push(folded.text.len());
                }
                word = true;
            } else if !(is_combining_mark(c) || word && matches!(c, '\'' | '’')) {
                word = false;
            }
            if c.is_whitespace() {
                if !after_space {
                    folded.push(' ', at);
                }
                after_space = true;
                continue;
            }
            after_space = false;
            // The callback gets each character `c` decomposes into: `é`
            // gives `e` then U+0301, the accent, which is dropped.
            decompose_canonical(c, |part| {
                if is_combining_mark(part) {
                    return;
                }
                for lower in part.to_lowercase() {
                    // A word-final sigma is the same letter.
                    folded.push(if lower == 'ς' { 'σ' } else { lower }, at);
                }
            });
        }
        folded
    }

    fn push(&mut self, c: char, from: u32) {
        self.text.push(c);
        self.from.resize(self.text.len(), from);
    }

    pub fn as_str(&self) -> &str {
        &self.text
    }

    /// Where `query`, folded the same way, occurs from the start of a word:
    /// character ranges of the original, first to last, none overlapping.
    /// `con` finds `contact` and `Control`, not `falcon`; it may go on
    /// into a phrase (`save fi`). A query that starts with something other
    /// than a word character (`--help`, `/home`) can't start inside a word,
    /// so it's found anywhere. None for an empty query, which would match
    /// between every two characters.
    pub fn find<'a>(&'a self, query: &'a str) -> impl Iterator<Item = Range<u32>> + 'a {
        let starts: Box<dyn Iterator<Item = usize>> = match query.chars().next() {
            None => Box::new(std::iter::empty()),
            Some(c) if in_word(c) => Box::new(self.words.iter().copied()),
            Some(_) => Box::new(self.text.match_indices(query).map(|(at, _)| at)),
        };
        let mut free = 0;
        starts
            .filter(move |&at| {
                let found = at >= free && self.text[at..].starts_with(query);
                if found {
                    free = at + query.len();
                }
                found
            })
            .map(move |at| self.from[at]..self.from[at + query.len() - 1] + 1)
    }
}

/// One text mode: the texts it searches, what's typed, the matches and the
/// current one.
pub struct Search {
    texts: Vec<Text>,
    /// Each text folded, in `texts`' order.
    folded: Vec<Folded>,
    query: String,
    /// Where each span asked about is, as the app said: global boxes, a line
    /// each, empty if it couldn't say. Kept for the whole text mode, so a
    /// longer query or a Backspace asks nothing it asked before.
    located: HashMap<Span, Vec<Rect>>,
    matches: Vec<Match>,
    current: usize,
    /// Where the first letter of each word shows, for `[text.starts]`.
    starts: Vec<Rect>,
}

impl Search {
    pub fn new(texts: Vec<Text>) -> Self {
        Self {
            folded: texts.iter().map(|t| Folded::new(&t.content)).collect(),
            texts,
            query: String::new(),
            located: HashMap::new(),
            matches: Vec::new(),
            current: 0,
            starts: Vec::new(),
        }
    }

    pub fn texts(&self) -> &[Text] {
        &self.texts
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    /// Types one more character.
    pub fn push(&mut self, c: char) {
        self.query.push(c);
    }

    /// Takes back the last character; `false` if there was none.
    pub fn pop(&mut self) -> bool {
        self.query.pop().is_some()
    }

    /// Whether typing `typed` would find something: it starts a word of a
    /// text. A tag's label that does can't be told apart
    /// from a query, so text mode never gives it to a tag.
    pub fn starts_a_word(&self, typed: &str) -> bool {
        let typed = Folded::new(typed);
        self.folded
            .iter()
            .any(|text| text.find(typed.as_str()).next().is_some())
    }

    /// Every occurrence of the query in the texts, in no particular order.
    fn spans(&self) -> Vec<Span> {
        let query = Folded::new(&self.query);
        self.folded
            .iter()
            .zip(&self.texts)
            .enumerate()
            .flat_map(|(i, (text, read))| {
                text.find(query.as_str()).map(move |found| Span {
                    text: i,
                    start: read.start + found.start,
                    end: read.start + found.end,
                })
            })
            .collect()
    }

    /// The occurrences of the query whose place isn't known yet: what to
    /// ask the app before [`Search::update`].
    pub fn wanted(&self) -> Vec<Span> {
        let mut spans = self.spans();
        spans.retain(|span| !self.located.contains_key(span));
        spans
    }

    /// What the app said about `spans`: for each, its boxes, a line each.
    pub fn located(&mut self, spans: &[Span], boxes: Vec<Vec<Rect>>) {
        self.located.extend(spans.iter().copied().zip(boxes));
    }

    /// The first character of every word: the spans a one-letter query
    /// finds, so once located, typing that letter asks the app nothing.
    pub fn word_starts(&self) -> Vec<Span> {
        self.folded
            .iter()
            .zip(&self.texts)
            .enumerate()
            .flat_map(|(i, (text, read))| {
                text.words.iter().map(move |&at| {
                    let start = read.start + text.from[at];
                    Span {
                        text: i,
                        start,
                        end: start + 1,
                    }
                })
            })
            .collect()
    }

    /// Keeps where `spans`, located, show: what [`Search::starts`] gives.
    /// Only a box the app gave counts, never a fallback to the text's own:
    /// a mark must sit on its letter.
    pub fn mark_starts(&mut self, spans: &[Span], shows: &dyn Fn(usize, Point) -> bool) {
        self.starts = spans
            .iter()
            .filter_map(|span| {
                let text = &self.texts[span.text];
                let part = self.located.get(span)?.first()?.intersect(text.area)?;
                (part.w >= MIN_SIZE && part.h >= MIN_SIZE && shows(text.window, part.center()))
                    .then_some(part)
            })
            .collect();
    }

    /// Where each word's first letter shows, if [`Search::mark_starts`]
    /// was asked.
    pub fn starts(&self) -> &[Rect] {
        &self.starts
    }

    /// The matches for the query as it is now, the first one current. A
    /// match's parts are its boxes cut to where its text can show, and
    /// those whose middle `shows`, given the text's window; a match with
    /// none left is out. They're in reading order, top to bottom, then left
    /// to right, by their first parts' middles, as tags are. One place
    /// reported twice (a label and its parent with the same text) is one
    /// match.
    pub fn update(&mut self, shows: &dyn Fn(usize, Point) -> bool) {
        let mut matches: Vec<Match> = self
            .spans()
            .into_iter()
            .filter_map(|span| {
                let text = &self.texts[span.text];
                let boxes = self.located.get(&span)?;
                let parts = visible(text, boxes)
                    .into_iter()
                    .filter(|part| shows(text.window, part.center()))
                    .collect::<Vec<_>>();
                (!parts.is_empty()).then_some(Match { span, parts })
            })
            .collect();
        matches.sort_by_key(|m| (m.point().y, m.point().x));
        let mut kept: Vec<Match> = Vec::with_capacity(matches.len());
        for found in matches {
            let again = kept
                .iter()
                .any(|k| k.parts[0].contains(found.point()) && found.parts[0].contains(k.point()));
            if !again {
                kept.push(found);
            }
        }
        self.matches = kept;
        self.current = 0;
    }

    pub fn matches(&self) -> &[Match] {
        &self.matches
    }

    /// The current match, if there's any match.
    pub fn current(&self) -> Option<&Match> {
        self.matches.get(self.current)
    }

    /// Which match is current, from 1, and how many there are: `3/7`, and
    /// `0/0` with none.
    pub fn position(&self) -> (usize, usize) {
        let count = self.matches.len();
        (if count == 0 { 0 } else { self.current + 1 }, count)
    }

    /// The next match, from the last back to the first.
    pub fn next(&mut self) {
        if !self.matches.is_empty() {
            self.current = (self.current + 1) % self.matches.len();
        }
    }

    /// The previous match, from the first round to the last.
    pub fn previous(&mut self) {
        if !self.matches.is_empty() {
            self.current = (self.current + self.matches.len() - 1) % self.matches.len();
        }
    }
}

/// The parts of a match's `boxes` that can show: each cut to its text's
/// area. A short text whose app gave no box is matched on its own box.
fn visible(text: &Text, boxes: &[Rect]) -> Vec<Rect> {
    let fallback = [text.rect];
    let boxes = if boxes.is_empty() && text.content.chars().count() <= SHORT {
        &fallback[..]
    } else {
        boxes
    };
    boxes
        .iter()
        .filter_map(|b| b.intersect(text.area))
        .filter(|b| b.w >= MIN_SIZE && b.h >= MIN_SIZE)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Where `query` is in `text`, folded as a search would fold both: the
    /// character ranges as `(start, end)`.
    fn find(text: &str, query: &str) -> Vec<(u32, u32)> {
        let folded = Folded::new(text);
        let query = Folded::new(query);
        folded
            .find(query.as_str())
            .map(|r| (r.start, r.end))
            .collect()
    }

    #[test]
    fn accents_fold_both_ways() {
        assert_eq!(find("άλλο", "α"), [(0, 1)]);
        assert_eq!(find("été été", "e"), [(0, 1), (4, 5)]);
        assert_eq!(
            find("cafe", "café"),
            [(0, 4)],
            "an accent typed is folded too"
        );
        assert_eq!(
            find("naïve café", "cafe"),
            [(6, 10)],
            "offsets count characters"
        );
    }

    #[test]
    fn case_is_ignored() {
        assert_eq!(find("contact Control", "con"), [(0, 3), (8, 11)]);
        assert_eq!(find("contact Control", "Con"), [(0, 3), (8, 11)]);
        assert_eq!(find("ΑΛΛΟ", "αλλο"), [(0, 4)]);
        assert_eq!(find("λόγος", "λογοσ"), [(0, 5)], "a final sigma is a sigma");
        assert_eq!(find("İstanbul", "ist"), [(0, 3)], "a dotted capital I");
    }

    #[test]
    fn white_space_runs_are_one_space() {
        assert_eq!(find("Save\n   as", "save as"), [(0, 10)]);
        assert_eq!(find("a  b", "a b"), [(0, 4)]);
    }

    #[test]
    fn an_empty_query_finds_nothing_and_matches_never_overlap() {
        assert_eq!(find("abc", ""), []);
        assert_eq!(find("aa aa aa", "aa aa"), [(0, 5)]);
        assert_eq!(find("--a--a", "--a"), [(0, 3), (3, 6)]);
    }

    #[test]
    fn a_query_starts_where_a_word_does() {
        assert_eq!(find("contact Control falcon", "con"), [(0, 3), (8, 11)]);
        assert_eq!(find("Save file", "fi"), [(5, 7)]);
        assert_eq!(find("Save file", "ave"), []);
        assert_eq!(find("Save file", "save fi"), [(0, 7)], "into a phrase");
        assert_eq!(find("can't", "t"), [], "an apostrophe keeps the word");
        assert_eq!(find("'quoted'", "quo"), [(1, 4)]);
        assert_eq!(find("file-name/path", "name"), [(5, 9)]);
        assert_eq!(find("file-name/path", "path"), [(10, 14)]);
        assert_eq!(find("snake_case camelCase", "case"), []);
        assert_eq!(find("άλλο", "αλ"), [(0, 2)]);
        assert_eq!(find("e\u{301}te", "ete"), [(0, 4)], "a combining accent");
        assert_eq!(find("x2 22", "2"), [(3, 4)], "numbers are words too");
    }

    #[test]
    fn a_query_that_starts_with_punctuation_is_found_anywhere() {
        assert_eq!(find("run --help", "--help"), [(4, 10)]);
        assert_eq!(find("~/home", "/home"), [(1, 6)]);
    }

    #[test]
    fn a_label_is_free_unless_typing_it_would_find_text() {
        let search = Search::new(vec![text("Save add", Rect::default())]);
        assert!(search.starts_a_word("s") && search.starts_a_word("A"));
        assert!(search.starts_a_word("ad"));
        assert!(!search.starts_a_word("av"), "inside a word");
        assert!(!search.starts_a_word("d"));
    }

    fn text(content: &str, rect: Rect) -> Text {
        Text {
            place: Place::Object(0),
            window: 0,
            name: String::new(),
            rect,
            area: Rect::new(0, 0, 1000, 1000),
            start: 0,
            content: content.into(),
        }
    }

    /// Answers every wanted span with one box per character offset, 10 px
    /// a character on its text's own row.
    fn locate(search: &mut Search) {
        let wanted = search.wanted();
        let boxes = wanted
            .iter()
            .map(|s| {
                let y = search.texts[s.text].rect.y;
                #[allow(clippy::cast_possible_wrap)]
                let (x, w) = (s.start as i32 * 10, (s.end - s.start) as i32 * 10);
                vec![Rect::new(x, y, w, 10)]
            })
            .collect();
        search.located(&wanted, boxes);
    }

    fn everywhere(_: usize, _: Point) -> bool {
        true
    }

    #[test]
    fn word_starts_are_what_a_first_letter_finds_and_only_shown_ones_mark() {
        let mut search = Search::new(vec![
            text("Save as… can't", Rect::new(0, 0, 200, 10)),
            text("é-x", Rect::new(0, 20, 200, 10)),
        ]);
        let spans = search.word_starts();
        let at: Vec<_> = spans.iter().map(|s| (s.text, s.start, s.end)).collect();
        assert_eq!(at, [(0, 0, 1), (0, 5, 6), (0, 9, 10), (1, 0, 1), (1, 2, 3)]);
        search.push('a');
        assert!(
            search.wanted().iter().all(|s| spans.contains(s)),
            "a first letter's matches are word starts"
        );
        let boxes = spans
            .iter()
            .map(|s| {
                #[allow(clippy::cast_possible_wrap)]
                let x = s.start as i32 * 10;
                let y = search.texts[s.text].rect.y;
                // The app can't say for the last one.
                if s.text == 1 && s.start == 2 {
                    vec![]
                } else {
                    vec![Rect::new(x, y, 10, 10)]
                }
            })
            .collect();
        search.located(&spans, boxes);
        assert!(search.wanted().is_empty(), "typing `a` asks nothing more");
        // The first text's window is covered past x = 80.
        search.mark_starts(&spans, &|window, at| window == 1 || at.x < 80);
        assert_eq!(
            search.starts(),
            [
                Rect::new(0, 0, 10, 10),
                Rect::new(50, 0, 10, 10),
                Rect::new(0, 20, 10, 10)
            ]
        );
    }

    #[test]
    fn matches_come_in_reading_order_and_navigation_wraps() {
        let mut search = Search::new(vec![
            text("down here con", Rect::new(0, 100, 200, 10)),
            text("con up con", Rect::new(0, 0, 200, 10)),
        ]);
        "con".chars().for_each(|c| search.push(c));
        locate(&mut search);
        search.update(&everywhere);
        let starts: Vec<(usize, u32)> = search
            .matches()
            .iter()
            .map(|m| (m.span.text, m.span.start))
            .collect();
        assert_eq!(starts, [(1, 0), (1, 7), (0, 10)], "top line first");
        assert_eq!(search.position(), (1, 3));
        search.previous();
        assert_eq!(search.position(), (3, 3), "back from the first is the last");
        search.next();
        assert_eq!(search.position(), (1, 3), "on from the last is the first");
    }

    #[test]
    fn a_query_edit_asks_only_for_new_spans_and_starts_at_the_first() {
        let mut search = Search::new(vec![text("contact control", Rect::new(0, 0, 200, 10))]);
        "co".chars().for_each(|c| search.push(c));
        assert_eq!(search.wanted().len(), 2);
        locate(&mut search);
        search.update(&everywhere);
        search.next();
        search.push('n');
        assert_eq!(search.wanted().len(), 2, "con is a longer span: asked once");
        locate(&mut search);
        search.update(&everywhere);
        assert_eq!(search.position(), (1, 2), "an edit selects the first");
        search.pop();
        assert!(search.wanted().is_empty(), "co was asked before");
        assert!(search.pop() && search.pop() && !search.pop());
        search.update(&everywhere);
        assert_eq!(search.position(), (0, 0), "nothing typed, nothing current");
        search.next();
        assert_eq!(search.current(), None);
    }

    #[test]
    fn hidden_parts_go_and_short_texts_fall_back_to_their_box() {
        let label = Rect::new(500, 500, 60, 20);
        let mut search = Search::new(vec![
            text("OK", label),
            text("ok but scrolled away", Rect::new(0, 2000, 300, 20)),
            text("covered ok", Rect::new(0, 0, 300, 20)),
        ]);
        search.push('o');
        search.push('k');
        let wanted = search.wanted();
        let boxes = wanted
            .iter()
            .map(|s| match s.text {
                // The app can't say where in the label.
                0 => Vec::new(),
                1 => vec![Rect::new(0, 2000, 20, 20)],
                _ => vec![Rect::new(80, 0, 20, 20)],
            })
            .collect();
        search.located(&wanted, boxes);
        search.update(&|_, at: Point| at.x != 90);
        let parts: Vec<&[Rect]> = search.matches().iter().map(|m| &m.parts[..]).collect();
        assert_eq!(parts, [&[label][..]], "only the label shows");
    }

    #[test]
    fn one_place_reported_twice_is_one_match() {
        let row = Rect::new(0, 0, 200, 10);
        let mut search = Search::new(vec![text("save", row), text("save", row)]);
        search.push('s');
        locate(&mut search);
        search.update(&everywhere);
        assert_eq!(search.matches().len(), 1);
    }

    #[test]
    fn a_wrapped_match_keeps_a_part_per_line() {
        let mut search = Search::new(vec![text("save as", Rect::new(0, 0, 40, 20))]);
        "save as".chars().for_each(|c| search.push(c));
        let wanted = search.wanted();
        let lines = vec![vec![Rect::new(0, 0, 40, 10), Rect::new(0, 10, 20, 10)]];
        search.located(&wanted, lines);
        search.update(&everywhere);
        let found = search.current().expect("one match");
        assert_eq!(found.parts.len(), 2);
        assert_eq!(
            found.point(),
            Point::new(20, 5),
            "clicked on its first line"
        );
    }

    /// `ab` then a wide `你` on row 0, a line break, `xy` on row 1: cells
    /// 10x20 physical px from (4, 6), at scale 2, in a window at (100, 50).
    fn cells() -> Cells {
        Cells {
            pane: 1,
            window: Point::new(100, 50),
            first: Point::new(4, 6),
            cell: (10, 20),
            scale: (2, 1),
            at: vec![
                (0, 0, 1),
                (0, 1, 2),
                (0, 2, 4),
                (0, 4, 4),
                (1, 0, 1),
                (1, 1, 2),
            ],
        }
    }

    #[test]
    fn cells_give_a_box_per_row_in_logical_px() {
        let cells = cells();
        assert_eq!(cells.boxes(1, 3), [Rect::new(107, 53, 15, 10)]);
        assert_eq!(
            cells.boxes(2, 6),
            [Rect::new(112, 53, 10, 10), Rect::new(102, 63, 10, 10)],
            "the line break has no box"
        );
        assert_eq!(cells.whole(), Rect::new(102, 53, 20, 20));
    }
}
