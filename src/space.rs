//! Clear space in a window: coarse ink cells and constant-time rectangle
//! queries. Body hints (terminals, sheets, pages) and displaced labels use
//! this map.

// Frame coordinates fit i32; signed values are clipped before indexing.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss
)]

use crate::{
    geometry::{Point, Rect},
    pixels::Frame,
};

const CELL: usize = 4;

pub struct Space {
    area: Rect,
    cols: usize,
    rows: usize,
    sums: Vec<u32>,
}

impl Space {
    /// Marks sharp colour changes, including faint text and control edges.
    /// Pooling every pixel (rather than sampling) keeps thin strokes.
    pub fn pixels(frame: &Frame) -> Self {
        let (w, h) = (frame.width, frame.height);
        let (cols, rows) = (w.div_ceil(CELL), h.div_ceil(CELL));
        let mut cells = vec![false; cols * rows];
        let pixels = frame.rgb.as_chunks::<3>().0;
        for y in 0..h {
            for x in 0..w {
                let p = pixels[y * w + x];
                let differs = |q: [u8; 3]| p.iter().zip(q).any(|(&a, b)| a.abs_diff(b) >= 12);
                if (x > 0 && differs(pixels[y * w + x - 1]))
                    || (y > 0 && differs(pixels[(y - 1) * w + x]))
                {
                    cells[y / CELL * cols + x / CELL] = true;
                }
            }
        }
        Self::build(Rect::new(0, 0, w as i32, h as i32), cols, rows, &cells)
    }

    /// Builds the same map from captured ink and protected controls, in
    /// global coordinates; negative monitor origins are valid.
    pub fn rectangles(area: Rect, obstacles: &[Rect]) -> Self {
        let (cols, rows) = (
            (area.w.max(0) as usize).div_ceil(CELL),
            (area.h.max(0) as usize).div_ceil(CELL),
        );
        let mut cells = vec![false; cols * rows];
        for &r in obstacles {
            let Some(r) = r.intersect(area) else {
                continue;
            };
            let x0 = (r.x - area.x) as usize / CELL;
            let y0 = (r.y - area.y) as usize / CELL;
            let x1 = ((r.right() - area.x) as usize).div_ceil(CELL).min(cols);
            let y1 = ((r.bottom() - area.y) as usize).div_ceil(CELL).min(rows);
            for y in y0..y1 {
                cells[y * cols + x0..y * cols + x1].fill(true);
            }
        }
        Self::build(area, cols, rows, &cells)
    }

    fn build(area: Rect, cols: usize, rows: usize, cells: &[bool]) -> Self {
        let stride = cols + 1;
        let mut sums = vec![0; stride * (rows + 1)];
        for y in 0..rows {
            let mut row = 0;
            for x in 0..cols {
                row += u32::from(cells[y * cols + x]);
                sums[(y + 1) * stride + x + 1] = sums[y * stride + x + 1] + row;
            }
        }
        Self {
            area,
            cols,
            rows,
            sums,
        }
    }

    /// Occupied cells in `rect`; anything outside the known area is blocked.
    pub fn ink(&self, rect: Rect) -> u32 {
        if rect.is_empty() || rect.intersect(self.area) != Some(rect) {
            return u32::MAX;
        }
        let (x0, y0) = (
            (rect.x - self.area.x) as usize / CELL,
            (rect.y - self.area.y) as usize / CELL,
        );
        let (x1, y1) = (
            ((rect.right() - self.area.x) as usize)
                .div_ceil(CELL)
                .min(self.cols),
            ((rect.bottom() - self.area.y) as usize)
                .div_ceil(CELL)
                .min(self.rows),
        );
        let stride = self.cols + 1;
        self.sums[y1 * stride + x1] + self.sums[y0 * stride + x0]
            - self.sums[y0 * stride + x1]
            - self.sums[y1 * stride + x0]
    }

    /// Whether a label-sized clear patch exists at all. Used once per
    /// window/size to avoid repeating a failed search in dense windows.
    pub fn fits(&self, bounds: Rect, width: i32, height: i32) -> bool {
        let Some(bounds) = bounds.intersect(self.area) else {
            return false;
        };
        let x0 = self.area.x + (bounds.x - self.area.x + 3) / 4 * 4;
        let y0 = self.area.y + (bounds.y - self.area.y + 3) / 4 * 4;
        for y in (y0..=bounds.bottom() - height).step_by(CELL) {
            for x in (x0..=bounds.right() - width).step_by(CELL) {
                if self.ink(Rect::new(x, y, width, height)) == 0 {
                    return true;
                }
            }
        }
        false
    }

    /// Where a body's one tag goes (a terminal, a sheet, a page): the
    /// `w`x`h` patch inside `area` with the least ink, nearest `aim` among
    /// equals, a step down counting `down` steps across. Corners every
    /// 4 px; rows go bottom to top, so an exact tie takes the lower patch.
    /// `None` when the patch doesn't fit.
    pub fn spot(&self, area: Rect, (w, h): (i32, i32), aim: Point, down: i32) -> Option<Rect> {
        let corners = Rect::new(area.x, area.y, area.w - w, area.h - h);
        if corners.w < 0 || corners.h < 0 {
            return None;
        }
        let cost = |r: Rect| {
            let c = r.center();
            (c.x - aim.x).abs() + down * (c.y - aim.y).abs()
        };
        let mut best = None;
        for y in (corners.y..=corners.bottom()).rev().step_by(CELL) {
            for x in (corners.x..=corners.right()).step_by(CELL) {
                let r = Rect::new(x, y, w, h);
                let score = (self.ink(r), cost(r));
                if best.as_ref().is_none_or(|(old, _)| score < *old) {
                    best = Some((score, r));
                }
            }
        }
        best.map(|(_, r)| r)
    }

    /// Horizontal occupied runs for snapshot replay, coalesced vertically.
    pub fn obstacles(&self) -> Vec<Rect> {
        let mut out: Vec<Rect> = Vec::new();
        let mut previous: std::collections::HashMap<(usize, usize), usize> =
            std::collections::HashMap::new();
        for y in 0..self.rows {
            let mut current = std::collections::HashMap::new();
            let mut x = 0;
            while x < self.cols {
                let occupied = |x| {
                    let stride = self.cols + 1;
                    self.sums[(y + 1) * stride + x + 1] + self.sums[y * stride + x]
                        > self.sums[y * stride + x + 1] + self.sums[(y + 1) * stride + x]
                };
                if !occupied(x) {
                    x += 1;
                    continue;
                }
                let start = x;
                x += 1;
                while x < self.cols && occupied(x) {
                    x += 1;
                }
                let key = (start, x);
                let index = if let Some(&index) = previous.get(&key) {
                    out[index].h = ((y + 1) * CELL).min(self.area.h as usize) as i32
                        - (out[index].y - self.area.y);
                    index
                } else {
                    out.push(Rect::new(
                        self.area.x + (start * CELL) as i32,
                        self.area.y + (y * CELL) as i32,
                        ((x * CELL).min(self.area.w as usize) - start * CELL) as i32,
                        CELL.min(self.area.h as usize - y * CELL) as i32,
                    ));
                    out.len() - 1
                };
                current.insert(key, index);
            }
            previous = current;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clear_space_preserves_thin_text_and_negative_origins() {
        let area = Rect::new(-120, -20, 100, 80);
        let map = Space::rectangles(area, &[Rect::new(-99, -10, 1, 30)]);
        assert_eq!(map.ink(Rect::new(-116, -16, 12, 40)), 0);
        assert!(map.ink(Rect::new(-100, -12, 8, 32)) > 0);
        assert_eq!(map.ink(Rect::new(-121, -20, 4, 4)), u32::MAX);
        let again = Space::rectangles(area, &map.obstacles());
        for y in -20..57 {
            for x in -120..-23 {
                assert_eq!(
                    map.ink(Rect::new(x, y, 3, 3)),
                    again.ink(Rect::new(x, y, 3, 3))
                );
            }
        }
    }
}
