//! Text mode's way into windows that can't say what they show: no
//! accessibility tree, no remote control. Their picture is read by
//! Tesseract, run as its own program (the distro's `tesseract`): the
//! picture goes in as a file, each word comes back with its box, as TSV.
//! A file in memory, not stdin: Tesseract reads stdin a byte at a time,
//! 0.45 s of a full-screen sheet's 1.9.
//!
//! Measured on this desktop's captures (`.plans/ocr-text-mode.md`): at one
//! picture px per logical px Tesseract misreads small UI text (`Jan`,
//! `mut`); at twice the size it reads it. So a picture under two px per
//! logical px is doubled first. `--psm 6`, one block of text, read more
//! there than `--psm 11`, sparse text. One thread each: four saved a tenth
//! of the time for two and a half times the CPU.

use std::{
    fs::{self, File},
    io::{self, Write as _},
    path::PathBuf,
    process::{Child, Command, Stdio},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result, anyhow, bail, ensure};

use super::{Window, runtime_dir};
use crate::{geometry::Point, pixels::Frame, text::Glyphs};

/// How often a running Tesseract is checked on.
const POLL: Duration = Duration::from_millis(5);

/// One window's picture being read.
pub struct Job {
    child: Child,
    _picture: Scratch,
    output: JoinHandle<io::Result<Vec<u8>>>,
    errors: JoinHandle<io::Result<Vec<u8>>>,
    /// How much bigger the picture Tesseract reads is than `frame`.
    zoom: i32,
    window: Point,
    scale: (i32, i32),
}

/// Starts reading `frame`, the picture of `window`, in `languages`
/// (the validated config supplies `eng` for English).
pub fn start(frame: &Frame, window: &Window, languages: &str) -> Result<Job> {
    ensure!(frame.width > 0 && frame.height > 0, "its picture is empty");
    let scale = (i32::try_from(frame.width)?, window.rect.w.max(1));
    let zoom = if scale.0 < 2 * scale.1 { 2 } else { 1 };
    let doubled;
    let picture = if zoom == 2 {
        doubled = double(frame);
        &doubled
    } else {
        frame
    };
    // A PPM: a short header, then the pixels as they are.
    let scratch =
        Scratch(runtime_dir()?.join(format!("ocr-{}-{}.ppm", std::process::id(), window.address)));
    let mut file = File::create(&scratch.0)?;
    write!(file, "P6\n{} {}\n255\n", picture.width, picture.height)?;
    file.write_all(&picture.rgb)?;
    drop(file);
    let mut child = Command::new("tesseract")
        .arg(&scratch.0)
        .args(["stdout", "--psm", "6", "-l", languages, "tsv"])
        .env("OMP_THREAD_LIMIT", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| match e.kind() {
            io::ErrorKind::NotFound => anyhow!("tesseract isn't installed"),
            _ => anyhow!("starting tesseract: {e}"),
        })?;
    let output = drain(child.stdout.take().context("tesseract's output")?);
    let errors = drain(child.stderr.take().context("tesseract's errors")?);
    Ok(Job {
        child,
        _picture: scratch,
        output,
        errors,
        zoom,
        window: window.rect.origin(),
        scale,
    })
}

/// A file that goes when this does, however reading it ended.
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

/// Reads all of `pipe` on a thread of its own: Tesseract blocks once a
/// pipe it writes to is full.
fn drain(mut pipe: impl io::Read + Send + 'static) -> JoinHandle<io::Result<Vec<u8>>> {
    thread::spawn(move || {
        let mut all = Vec::new();
        pipe.read_to_end(&mut all).map(|_| all)
    })
}

impl Job {
    /// The text Tesseract found, and where each character is, by `until`.
    pub fn finish(mut self, until: Instant) -> Result<(String, Glyphs)> {
        let status = loop {
            if let Some(status) = self.child.try_wait()? {
                break status;
            }
            if Instant::now() >= until {
                let _ = self.child.kill();
                let _ = self.child.wait();
                bail!("tesseract took too long");
            }
            thread::sleep(POLL);
        };
        let joined = |pipe: JoinHandle<io::Result<Vec<u8>>>| {
            pipe.join()
                .map_err(|_| anyhow!("reading tesseract panicked"))?
                .context("reading tesseract")
        };
        let tsv = joined(self.output)?;
        if !status.success() {
            let errors = joined(self.errors).unwrap_or_default();
            bail!(
                "tesseract failed ({status}): {}",
                String::from_utf8_lossy(&errors).trim()
            );
        }
        let words = words(&String::from_utf8_lossy(&tsv), self.zoom);
        Ok(lay_out(&words, self.window, self.scale))
    }
}

/// `frame` at twice the size, each pixel three quarters itself and one
/// quarter its neighbour on that side: bilinear, as PIL scales, which is
/// what was measured.
#[allow(clippy::cast_possible_truncation)] // a mean of bytes is a byte
fn double(frame: &Frame) -> Frame {
    let (w, h) = (frame.width, frame.height);
    let mix = |near: u8, far: u8| ((3 * u16::from(near) + u16::from(far) + 2) / 4) as u8;
    let mut wide = Vec::with_capacity(w * h * 6);
    for row in frame.rgb.chunks_exact(w * 3) {
        let at = |x: usize| &row[x * 3..x * 3 + 3];
        for x in 0..w {
            let here = at(x);
            for side in [at(x.saturating_sub(1)), at((x + 1).min(w - 1))] {
                wide.extend(here.iter().zip(side).map(|(&a, &b)| mix(a, b)));
            }
        }
    }
    let stride = w * 6;
    let row = |y: usize| &wide[y * stride..(y + 1) * stride];
    let mut rgb = Vec::with_capacity(wide.len() * 2);
    for y in 0..h {
        let here = row(y);
        for side in [row(y.saturating_sub(1)), row((y + 1).min(h - 1))] {
            rgb.extend(here.iter().zip(side).map(|(&a, &b)| mix(a, b)));
        }
    }
    Frame {
        width: w * 2,
        height: h * 2,
        rgb,
    }
}

/// One word as Tesseract found it.
#[derive(Debug, PartialEq, Eq)]
struct Word {
    /// Its block, paragraph and line: words with the same are one line.
    line: [u32; 3],
    /// Left, top, right, bottom, in the picture's px (not the zoomed one).
    corners: [i32; 4],
    text: String,
}

/// The words in Tesseract's TSV, read at `zoom` times the picture's size.
/// A row is a page, block, paragraph, line or word (level 5), with its box
/// and, for a word, its text.
fn words(tsv: &str, zoom: i32) -> Vec<Word> {
    tsv.lines()
        .skip(1)
        .filter_map(|row| {
            let fields: Vec<&str> = row.split('\t').collect();
            let [
                level,
                _,
                block,
                paragraph,
                line,
                _,
                left,
                top,
                width,
                height,
                _,
                text,
            ] = fields.as_slice()
            else {
                return None;
            };
            let text = text.trim();
            if *level != "5" || text.is_empty() {
                return None;
            }
            let number = |field: &str| field.parse::<i32>().ok();
            let (left, top) = (number(left)?, number(top)?);
            let (right, bottom) = (left + number(width)?, top + number(height)?);
            Some(Word {
                line: [block, paragraph, line].map(|f| f.parse().unwrap_or(0)),
                corners: [
                    left.div_euclid(zoom),
                    top.div_euclid(zoom),
                    (right + zoom - 1).div_euclid(zoom),
                    (bottom + zoom - 1).div_euclid(zoom),
                ],
                text: text.to_owned(),
            })
        })
        .collect()
}

/// The words joined into one text, a space between two on a line and a
/// line break between lines, with where each character is in a picture of
/// the window at `window`, `scale` its px to logical px. A word's
/// characters share its width evenly: Tesseract says where words are, not
/// letters.
fn lay_out(words: &[Word], window: Point, scale: (i32, i32)) -> (String, Glyphs) {
    let mut content = String::new();
    let mut lines: Vec<(i32, i32)> = Vec::new();
    let mut at: Vec<(u32, i32, i32)> = Vec::new();
    let mut previous: Option<&Word> = None;
    for word in words {
        let [left, top, right, bottom] = word.corners;
        let end = at.last().map_or(0, |a| a.2);
        match previous {
            Some(last) if last.line == word.line => {
                content.push(' ');
                at.push((at.last().map_or(0, |a| a.0), end, left.max(end)));
                let line = lines.last_mut().expect("a word came before");
                *line = (line.0.min(top), line.1.max(bottom));
            }
            _ => {
                if let Some(&(line, ..)) = at.last() {
                    content.push('\n');
                    at.push((line, end, end));
                }
                lines.push((top, bottom));
            }
        }
        let line = u32::try_from(lines.len() - 1).unwrap_or(u32::MAX);
        let count = i32::try_from(word.text.chars().count()).unwrap_or(i32::MAX);
        let width = right - left;
        for (c, i) in word.text.chars().zip(0..) {
            content.push(c);
            at.push((
                line,
                left + width * i / count,
                left + width * (i + 1) / count,
            ));
        }
        previous = Some(word);
    }
    let glyphs = Glyphs {
        window,
        scale,
        lines,
        at,
    };
    (content, glyphs)
}

/// Whether `now` shows what `then` did in every box of `corners` (left,
/// top, right, bottom, picture px): the same size, the same bytes.
pub fn unchanged(then: &Frame, now: &Frame, corners: &[[i32; 4]]) -> bool {
    if (then.width, then.height) != (now.width, now.height) {
        return false;
    }
    let clamp = |v: i32, most: usize| usize::try_from(v.max(0)).unwrap_or(0).min(most);
    corners.iter().all(|&[left, top, right, bottom]| {
        let (left, right) = (clamp(left, then.width), clamp(right, then.width));
        (clamp(top, then.height)..clamp(bottom, then.height)).all(|y| {
            let row = (y * then.width + left) * 3..(y * then.width + right) * 3;
            then.rgb[row.clone()] == now.rgb[row]
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A TSV as Tesseract writes it: the header, a page, block, paragraph
    /// and line row, then words.
    const TSV: &str = "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext
1\t1\t0\t0\t0\t0\t0\t0\t400\t200\t-1\t
2\t1\t1\t0\t0\t0\t20\t10\t300\t80\t-1\t
3\t1\t1\t1\t0\t0\t20\t10\t300\t80\t-1\t
4\t1\t1\t1\t1\t0\t20\t10\t300\t30\t-1\t
5\t1\t1\t1\t1\t1\t20\t10\t60\t30\t96.1\tSave
5\t1\t1\t1\t1\t2\t100\t12\t40\t28\t95.0\tas
5\t1\t1\t1\t1\t3\t150\t12\t40\t28\t20.0\t\x20
4\t1\t1\t1\t2\t0\t21\t60\t81\t30\t-1\t
5\t1\t1\t1\t2\t1\t21\t60\t81\t30\t90.2\tάλλο";

    #[test]
    fn words_come_at_the_pictures_size() {
        let words = words(TSV, 2);
        let found: Vec<(&str, [i32; 4])> =
            words.iter().map(|w| (w.text.as_str(), w.corners)).collect();
        assert_eq!(
            found,
            [
                ("Save", [10, 5, 40, 20]),
                ("as", [50, 6, 70, 20]),
                ("άλλο", [10, 30, 51, 45]),
            ],
            "blank words go; ends round outwards"
        );
        assert_eq!(words[1].line, words[0].line);
        assert_ne!(words[2].line, words[0].line);
    }

    #[test]
    fn words_lay_out_as_lines_with_a_box_per_character() {
        let (content, Glyphs { lines, at, .. }) = lay_out(&words(TSV, 1), Point::default(), (1, 1));
        assert_eq!(content, "Save as\nάλλο");
        assert_eq!(at.len(), content.chars().count());
        assert_eq!(
            lines,
            [(10, 40), (60, 90)],
            "a line is as tall as its words"
        );
        assert_eq!(
            &at[..4],
            [(0, 20, 35), (0, 35, 50), (0, 50, 65), (0, 65, 80)]
        );
        assert_eq!(at[4], (0, 80, 100), "the space spans the gap");
        assert_eq!(at[7], (0, 140, 140), "the break covers nothing");
        assert_eq!(at[8], (1, 21, 41));
    }

    fn frame(width: usize, height: usize, rgb: Vec<u8>) -> Frame {
        Frame { width, height, rgb }
    }

    #[test]
    fn doubling_mixes_each_pixel_with_its_neighbours() {
        // Two pixels, black then white, one row.
        let doubled = double(&frame(2, 1, vec![0, 0, 0, 255, 255, 255]));
        assert_eq!((doubled.width, doubled.height), (4, 2));
        let reds: Vec<u8> = doubled.rgb.iter().step_by(3).copied().collect();
        assert_eq!(reds, [0, 64, 191, 255, 0, 64, 191, 255]);
    }

    #[test]
    fn a_change_outside_the_boxes_is_no_change() {
        let then = frame(4, 2, vec![10; 24]);
        let mut now = frame(4, 2, vec![10; 24]);
        now.rgb[3 * 3] = 99;
        assert!(unchanged(&then, &now, &[[0, 0, 3, 2]]));
        assert!(!unchanged(&then, &now, &[[2, 0, 4, 1]]));
        assert!(
            !unchanged(&then, &frame(2, 2, vec![10; 12]), &[]),
            "resized"
        );
    }

    /// The saved captures, read as a run reads them: what text mode finds
    /// of words seen in them, and how long it took. Run with
    /// `cargo test --release -- --ignored --nocapture captures_read`.
    #[test]
    #[ignore = "runs tesseract"]
    fn captures_read() {
        let cases = [
            (
                "terminal-nvim",
                &[
                    "rand::Rng",
                    "cmp::Ordering",
                    "fs::File",
                    "BufRead",
                    "BufReader",
                    "ErrorKind",
                    "Write",
                    "what is your name?",
                    "String::new()",
                    "Nice to meet you",
                    "io::stdin()",
                    "read_line",
                    "Didn't Receive Input",
                    "Hello, world!",
                    "main.rs",
                ][..],
                13,
            ),
            (
                "calc",
                &[
                    "File",
                    "Edit",
                    "View",
                    "Insert",
                    "Format",
                    "Styles",
                    "Sheet",
                    "Data",
                    "Tools",
                    "Window",
                    "Help",
                    "Liberation Sans",
                    "Month",
                    "Region",
                    "Units",
                    "Price",
                    "Total",
                    "Jan",
                    "Feb",
                    "Mar",
                    "Apr",
                    "May",
                    "Jun",
                    "North",
                    "South",
                    "East",
                ][..],
                26,
            ),
        ];
        for (name, queries, expected) in cases {
            let path = format!("tests/fixtures/{name}.png");
            let frame = crate::pixels::tests::load(std::path::Path::new(&path));
            let window = Window {
                address: "0x1".into(),
                pid: 1,
                class: name.into(),
                title: name.into(),
                rect: crate::geometry::Rect::new(0, 0, i32::try_from(frame.width).unwrap(), 1),
                monitor: 0,
                focused: false,
                kind: super::super::Kind::Client,
                frames: Vec::new(),
            };
            let started = Instant::now();
            let job = start(&frame, &window, "eng").unwrap();
            let (content, glyphs) = job.finish(started + Duration::from_secs(10)).unwrap();
            let took = started.elapsed();
            assert_eq!(glyphs.at.len(), content.chars().count());
            let folded = crate::text::Folded::new(&content, true);
            let missed: Vec<&str> = queries
                .iter()
                .copied()
                .filter(|q| {
                    !folded
                        .as_str()
                        .contains(crate::text::Folded::new(q, true).as_str())
                })
                .collect();
            let found = queries.len() - missed.len();
            println!(
                "{name}: {found}/{} in {took:?}, missed {missed:?}",
                queries.len()
            );
            assert!(
                found >= expected,
                "{name}: {found} found, {expected} measured"
            );
        }
    }
}
