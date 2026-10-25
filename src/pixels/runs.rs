//! 8-connected blobs of a mask, as row runs labelled with union-find (as
//! Neru's segment detector labels them). Both passes find their blobs this
//! way: a run is a row's stretch of marked pixels, and joins the runs above
//! it that touch it, corners included.

/// One horizontal stretch of marked pixels. Bounds are inclusive.
#[derive(Clone, Copy)]
pub struct Run {
    pub x0: i32,
    pub x1: i32,
    /// The blob's number.
    pub label: usize,
}

/// Every run, row by row, and how many blobs they make. Blobs are numbered
/// in raster order of their first run, which is raster order of their
/// first pixel.
pub struct Runs {
    list: Vec<Run>,
    /// Row `y` is `list[starts[y]..starts[y + 1]]`.
    starts: Vec<usize>,
    pub count: usize,
}

impl Runs {
    pub fn row(&self, y: usize) -> &[Run] {
        &self.list[self.starts[y]..self.starts[y + 1]]
    }
}

/// The runs of `mask`, a `w` x `h` frame's pixels row by row.
pub fn label(mask: &[bool], w: usize, h: usize) -> Runs {
    let mut runs: Vec<Run> = Vec::with_capacity(w * h / 16);
    let mut starts = vec![0; h + 1];
    let mut parent: Vec<usize> = Vec::with_capacity(1024);
    let find = |parent: &mut Vec<usize>, mut a: usize| {
        while parent[a] != a {
            parent[a] = parent[parent[a]];
            a = parent[a];
        }
        a
    };

    let (mut prev0, mut prev1) = (0, 0);
    for y in 0..h {
        starts[y] = runs.len();
        let row = &mask[y * w..(y + 1) * w];
        let mut x = 0;
        while let Some(start) = row[x..].iter().position(|&m| m) {
            let x0 = x + start;
            x = row[x0..].iter().position(|&m| !m).map_or(w, |end| x0 + end);
            let (x0, x1) = (x0 as i32, x as i32 - 1);
            let mut label: Option<usize> = None;
            // Runs in a row are in order and apart, so the previous row's
            // runs ending too far left for this one are done with for good.
            while prev0 < prev1 && runs[prev0].x1 + 1 < x0 {
                prev0 += 1;
            }
            for above in &runs[prev0..prev1] {
                if above.x0 > x1 + 1 {
                    break;
                }
                let root = find(&mut parent, above.label);
                label = Some(match label {
                    None => root,
                    Some(own) if own != root => {
                        let (lo, hi) = (root.min(own), root.max(own));
                        parent[hi] = lo;
                        lo
                    }
                    Some(own) => own,
                });
            }
            let label = label.unwrap_or_else(|| {
                parent.push(parent.len());
                parent.len() - 1
            });
            runs.push(Run { x0, x1, label });
        }
        (prev0, prev1) = (starts[y], runs.len());
    }
    starts[h] = runs.len();

    // The runs are stored in raster order: number the blobs as their first
    // runs come.
    let mut number = vec![usize::MAX; parent.len()];
    let mut count = 0;
    for run in &mut runs {
        let root = find(&mut parent, run.label);
        if number[root] == usize::MAX {
            number[root] = count;
            count += 1;
        }
        run.label = number[root];
    }
    Runs {
        list: runs,
        starts,
        count,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs_join_below_and_across_corners_numbered_by_first_run() {
        // A U whose arms join two rows down, a pixel touching it only at a
        // corner, and a blob of its own that starts after the U's arms.
        let rows = ["X.X..", "X.X.X", "XXX..", "...X."];
        let mask: Vec<bool> = rows.concat().chars().map(|c| c == 'X').collect();
        let runs = label(&mask, 5, rows.len());
        let labels = |y| runs.row(y).iter().map(|r| r.label).collect::<Vec<_>>();
        assert_eq!(runs.count, 2);
        assert_eq!(labels(0), [0, 0]);
        assert_eq!(labels(1), [0, 0, 1]);
        assert_eq!(labels(2), [0]);
        assert_eq!(labels(3), [0]);
        assert_eq!((runs.row(2)[0].x0, runs.row(2)[0].x1), (0, 2));
    }
}
