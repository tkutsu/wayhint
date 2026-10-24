//! Boxes bucketed by the grid cells they cover, so "which boxes could
//! touch this one" reads a few cells instead of every box. The two
//! searches that were all pairs in the ports, a blob's parent and the
//! fill-in's overlap test, ask it; their answers don't change, only how
//! many boxes they look at (a dense window of 14,400 boxes took 150 ms in
//! the parent search alone, audit 2026-10-24).

use super::Corners;

/// The cell side, in px: about a control's size.
const CELL: i32 = 32;

pub struct Buckets {
    cols: i32,
    rows: i32,
    cells: Vec<Vec<usize>>,
}

impl Buckets {
    /// `boxes` by index, each in every cell it covers.
    pub fn new(boxes: &[Corners]) -> Self {
        let right = boxes.iter().map(|b| b[2]).max().unwrap_or(0);
        let bottom = boxes.iter().map(|b| b[3]).max().unwrap_or(0);
        let mut buckets = Self {
            cols: right / CELL + 1,
            rows: bottom / CELL + 1,
            cells: Vec::new(),
        };
        buckets.cells = vec![Vec::new(); (buckets.cols * buckets.rows) as usize];
        for (i, &b) in boxes.iter().enumerate() {
            for cell in buckets.cells_of(b) {
                buckets.cells[cell].push(i);
            }
        }
        buckets
    }

    /// The boxes in the cells `area` covers: every box that overlaps it,
    /// and some that don't. A box in several of those cells comes once per
    /// cell.
    pub fn near(&self, area: Corners) -> impl Iterator<Item = usize> + '_ {
        self.cells_of(area)
            .flat_map(move |cell| self.cells[cell].iter().copied())
    }

    /// The cells a box covers, clamped to the grid. The right and bottom
    /// edges are outside the box, as in [`Corners`].
    fn cells_of(&self, b: Corners) -> impl Iterator<Item = usize> + use<> {
        let (cols, rows) = (self.cols, self.rows);
        let col = |x: i32| (x.max(0) / CELL).min(cols - 1);
        let row = |y: i32| (y.max(0) / CELL).min(rows - 1);
        let (x0, x1) = (col(b[0]), col((b[2] - 1).max(b[0])));
        let (y0, y1) = (row(b[1]), row((b[3] - 1).max(b[1])));
        (y0..=y1).flat_map(move |cy| (x0..=x1).map(move |cx| (cy * cols + cx) as usize))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn near_finds_every_overlapping_box() {
        let boxes = [
            [0, 0, 10, 10],
            [100, 100, 300, 120],
            [40, 40, 41, 41],
            [0, 0, 500, 500],
        ];
        let buckets = Buckets::new(&boxes);
        let near = |area| {
            let mut found: Vec<usize> = buckets.near(area).collect();
            found.sort_unstable();
            found.dedup();
            found
        };
        assert_eq!(near([5, 5, 6, 6]), [0, 3]);
        assert_eq!(near([250, 110, 251, 111]), [1, 3]);
        assert_eq!(near([39, 39, 42, 42]), [2, 3]);
        assert_eq!(near([-20, -20, -10, -10]), [0, 3], "clamped into the grid");
        assert_eq!(
            near([900, 900, 950, 950]),
            [3],
            "past the last box: the last cell"
        );
    }
}
