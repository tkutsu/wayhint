//! Spreadsheet grids: thin row lines a text line apart, crossed by thin
//! column lines. A sheet is one place to type into, so its cells, row
//! numbers and column letters get no tags: the grid becomes one body
//! ([`super::find`]).
//!
//! Spacing tells a sheet from look-alikes: a calendar's hour or week rows
//! are 48 px and more apart, a month has 6 of them.

use super::{Corners, Frame, group::inside};

/// A line pixel differs this much from both sides of the line, in some
/// channel: Calc's grey lines by 41, Sheets' dark ones by 24.
const EDGE: u8 = 12;
/// Along a line its colour stays within this much of where it started.
const DRIFT: u8 = 10;
/// A line may skip this many pixels: where a column crosses a row, the
/// row's pixel has line on both sides.
const GAP: usize = 2;
/// The shortest row line and column line, px.
const ROW: usize = 120;
const COLUMN: usize = 60;
/// Rows at least, columns at least, and the median row spacing.
const ROWS: usize = 8;
const COLUMNS: usize = 3;
const PITCH: std::ops::RangeInclusive<usize> = 12..=40;
/// Row numbers fit this width left of the cells (Calc's and Sheets' take
/// 46 px); column letters fit a row and a half above them.
const HEADER: i32 = 64;

/// A run of line pixels: across row (or column) `at`, `from` up to `to`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Line {
    at: usize,
    from: usize,
    to: usize,
}

impl Line {
    /// Whether a row line and a column line cross, both running on past
    /// the crossing: a + as a sheet's cells make, not the T or L of a
    /// panel's border meeting a heading's rule.
    const fn crosses(row: Self, column: Self) -> bool {
        row.from + GAP < column.at
            && column.at + GAP < row.to
            && column.from + GAP < row.at
            && row.at + GAP < column.to
    }
}

/// A run of line pixels so far: where it starts, its last line pixel and
/// its colour.
#[derive(Clone, Copy)]
struct Run {
    from: usize,
    last: usize,
    colour: u32,
}

/// Whether two packed pixels differ by `by` or more in some channel.
fn apart(a: u32, b: u32, by: u8) -> bool {
    let (a, b) = (a.to_le_bytes(), b.to_le_bytes());
    (0..3).any(|c| a[c].abs_diff(b[c]) >= by)
}

/// Moves `run` on to pixel `i`, a line pixel when `on`, and returns the
/// span of a run that ended.
fn step(run: &mut Option<Run>, i: usize, on: bool, p: u32) -> Option<(usize, usize)> {
    match *run {
        Some(Run { from, colour, .. }) if on && !apart(p, colour, DRIFT + 1) => {
            *run = Some(Run {
                from,
                last: i,
                colour,
            });
            None
        }
        Some(Run { last, .. }) if i - last <= GAP => None,
        ended => {
            *run = on.then_some(Run {
                from: i,
                last: i,
                colour: p,
            });
            ended.map(|r| (r.from, r.last + 1))
        }
    }
}

/// A spreadsheet's grid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sheet {
    /// Its cells: the box around its lines.
    pub cells: Corners,
    /// Its row numbers, left of the cells, and its column letters above
    /// them: no tags there either.
    pub headers: [Corners; 2],
    rows: Vec<Line>,
    columns: Vec<Line>,
}

impl Sheet {
    /// Whether `p` is in the cells or the headers.
    pub fn holds(&self, p: (i32, i32)) -> bool {
        inside(p, self.cells) || self.headers.iter().any(|&h| inside(p, h))
    }
}

/// The sheets in `frame`.
pub fn sheets(frame: &Frame) -> Vec<Sheet> {
    let (rows, columns) = lines(frame);

    // Lines that cross are one grid: union-find over rows, then columns.
    let mut parent: Vec<usize> = (0..rows.len() + columns.len()).collect();
    let root = |parent: &mut Vec<usize>, mut i: usize| {
        while parent[i] != i {
            parent[i] = parent[parent[i]];
            i = parent[i];
        }
        i
    };
    for (r, &row) in rows.iter().enumerate() {
        for (c, &column) in columns.iter().enumerate() {
            if Line::crosses(row, column) {
                let (a, b) = (root(&mut parent, r), root(&mut parent, rows.len() + c));
                parent[a] = b;
            }
        }
    }
    let mut grids: std::collections::BTreeMap<usize, (Vec<Line>, Vec<Line>)> =
        std::collections::BTreeMap::new();
    for (i, &row) in rows.iter().enumerate() {
        grids.entry(root(&mut parent, i)).or_default().0.push(row);
    }
    for (i, &column) in columns.iter().enumerate() {
        let r = root(&mut parent, rows.len() + i);
        grids.entry(r).or_default().1.push(column);
    }
    grids
        .into_values()
        .filter_map(|(rows, columns)| {
            if places(&columns).len() < COLUMNS {
                return None;
            }
            let pitch = spacing(&rows)?;
            let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0, 0);
            for r in &rows {
                (x0, x1, y0, y1) = (x0.min(r.from), x1.max(r.to), y0.min(r.at), y1.max(r.at + 1));
            }
            for c in &columns {
                (y0, y1, x0, x1) = (y0.min(c.from), y1.max(c.to), x0.min(c.at), x1.max(c.at + 1));
            }
            #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
            let ([x0, y0, x1, y1], head) =
                ([x0, y0, x1, y1].map(|v| v as i32), (pitch * 3 / 2) as i32);
            Some(Sheet {
                cells: [x0, y0, x1, y1],
                headers: [[x0 - HEADER, y0, x0, y1], [x0 - HEADER, y0 - head, x1, y0]],
                rows,
                columns,
            })
        })
        .collect()
}

/// Where `lines` are, each place once, in order.
fn places(lines: &[Line]) -> Vec<usize> {
    let mut at: Vec<usize> = lines.iter().map(|l| l.at).collect();
    at.sort_unstable();
    at.dedup();
    at
}

/// The rows' median spacing, if they are enough rows a text line apart.
fn spacing(rows: &[Line]) -> Option<usize> {
    let ys = places(rows);
    if ys.len() < ROWS {
        return None;
    }
    let mut gaps: Vec<usize> = ys.windows(2).map(|p| p[1] - p[0]).collect();
    gaps.sort_unstable();
    Some(gaps[gaps.len() / 2]).filter(|g| PITCH.contains(g))
}

/// `frame` with the sheets' lines painted in the colour beside them, so
/// only what is in the cells counts as ink: every empty cell is as clear
/// as the next, and the one nearest the middle takes a body's tag.
pub fn unruled(frame: &Frame, sheets: &[Sheet]) -> Frame {
    let w = frame.width;
    let mut rgb = frame.rgb.clone();
    let mut copy = |to: usize, from: usize| rgb.copy_within(from * 3..from * 3 + 3, to * 3);
    for sheet in sheets {
        for r in &sheet.rows {
            for x in r.from..r.to {
                copy(r.at * w + x, (r.at - 1) * w + x);
            }
        }
        for c in &sheet.columns {
            for y in c.from..c.to {
                copy(y * w + c.at, y * w + c.at - 1);
            }
        }
    }
    Frame {
        width: w,
        height: frame.height,
        rgb,
    }
}

/// The row lines and column lines in `frame`: a row line's pixel differs
/// from the pixels above and below it, a column line's from those left
/// and right. Columns are looked for only between the rows, and only when
/// there are rows enough, a text line apart, for a sheet, which most
/// windows haven't.
fn lines(frame: &Frame) -> (Vec<Line>, Vec<Line>) {
    let (w, h) = (frame.width, frame.height);
    // A pixel as one word: most pixels equal their neighbours, and one
    // comparison says so.
    let px = |i: usize| {
        let [r, g, b] = [frame.rgb[i * 3], frame.rgb[i * 3 + 1], frame.rgb[i * 3 + 2]];
        u32::from_le_bytes([r, g, b, 0])
    };
    let differs = |a: u32, b: u32| a != b && apart(a, b, EDGE);
    let keep = |lines: &mut Vec<Line>, at, min, span: Option<(usize, usize)>| {
        if let Some((from, to)) = span
            && to - from >= min
        {
            lines.push(Line { at, from, to });
        }
    };

    let mut rows = Vec::new();
    for y in 1..h.saturating_sub(1) {
        let mut across = None;
        for x in 0..w {
            let i = y * w + x;
            let p = px(i);
            let on = differs(p, px(i - w)) && differs(p, px(i + w));
            // No run and no line pixel: nothing to step.
            if on || across.is_some() {
                keep(&mut rows, y, ROW, step(&mut across, x, on, p));
            }
        }
        keep(&mut rows, y, ROW, across.map(|r| (r.from, r.last + 1)));
    }
    if spacing(&rows).is_none() {
        return (rows, Vec::new());
    }

    let top = rows.iter().map(|r| r.at).min().unwrap_or(0);
    let bottom = rows.iter().map(|r| r.at + 1).max().unwrap_or(h);
    let mut columns = Vec::new();
    let mut down: Vec<Option<Run>> = vec![None; w];
    for y in top..bottom {
        let inner = down.iter_mut().enumerate().take(w.saturating_sub(1));
        for (x, run) in inner.skip(1) {
            let i = y * w + x;
            let p = px(i);
            let on = differs(p, px(i - 1)) && differs(p, px(i + 1));
            if on || run.is_some() {
                keep(&mut columns, x, COLUMN, step(run, y, on, p));
            }
        }
    }
    for (x, run) in down.into_iter().enumerate() {
        keep(&mut columns, x, COLUMN, run.map(|r| (r.from, r.last + 1)));
    }
    (rows, columns)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `w`x`h` of `bg`, with a line of `ink` every `pitch` px down and
    /// every `across` px across, inside `area`.
    fn ruled(w: usize, h: usize, area: [usize; 4], pitch: usize, across: usize) -> Frame {
        let [x0, y0, x1, y1] = area;
        let rgb = (0..h)
            .flat_map(|y| (0..w).map(move |x| (x, y)))
            .flat_map(|(x, y)| {
                let inside = (x0..=x1).contains(&x) && (y0..=y1).contains(&y);
                let line = (y - y0.min(y)) % pitch == 0 || (x - x0.min(x)) % across == 0;
                if inside && line { [163; 3] } else { [204; 3] }
            })
            .collect();
        Frame {
            width: w,
            height: h,
            rgb,
        }
    }

    #[test]
    fn a_ruled_sheet_is_its_crossing_lines_with_headers_beside() {
        let frame = ruled(800, 600, [100, 80, 700, 540], 20, 80);
        let found = sheets(&frame);
        let [sheet] = &found[..] else {
            panic!("{found:?}");
        };
        // The outer lines meet the others in Ts and Ls: the cells are
        // inside them.
        assert_eq!(sheet.cells, [101, 81, 701, 540]);
        assert!(sheet.holds((60, 300)), "a row number");
        assert!(sheet.holds((400, 60)), "a column letter");
        assert!(!sheet.holds((400, 20)) && !sheet.holds((20, 300)));

        let clean = unruled(&frame, &found);
        let [x0, y0, x1, y1] = sheet.cells.map(|v| v as usize);
        let mut inner = (y0 + 1..y1 - 1).flat_map(|y| (x0 + 1..x1 - 1).map(move |x| (x, y)));
        assert!(
            inner.all(|(x, y)| clean.rgb[(y * 800 + x) * 3] == 204),
            "no line left in the cells"
        );
    }

    #[test]
    fn calendar_rows_are_too_far_apart() {
        // Hour rows 48 px apart, 7 day columns: not a sheet.
        let frame = ruled(800, 600, [50, 50, 750, 578], 48, 100);
        assert_eq!(sheets(&frame), Vec::new());
    }

    #[test]
    fn a_few_rows_or_no_columns_are_no_sheet() {
        let few_rows = ruled(800, 300, [100, 80, 700, 200], 20, 80);
        let one_column = ruled(800, 600, [100, 80, 700, 540], 20, 1000);
        assert_eq!(sheets(&few_rows), Vec::new());
        assert_eq!(sheets(&one_column), Vec::new());
    }
}
