use std::{fs::File, path::Path, time::Instant};

use serde::Deserialize;
use serde_json::json;

use super::*;

/// A PNG as a [`Frame`]: RGB as it is, RGBA flattened on black (alpha
/// dropped, as the captures are premultiplied).
fn load(path: &Path) -> Frame {
    let mut decoder = png::Decoder::new(std::io::BufReader::new(File::open(path).unwrap()));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().unwrap();
    let mut buf = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut buf).unwrap();
    let rgb = match info.color_type {
        png::ColorType::Rgb => buf[..info.buffer_size()].to_vec(),
        png::ColorType::Rgba => buf[..info.buffer_size()]
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|&[r, g, b, _]| [r, g, b])
            .collect(),
        other => panic!("{}: {other:?} isn't RGB", path.display()),
    };
    Frame {
        width: info.width as usize,
        height: info.height as usize,
        rgb,
    }
}

fn frame(width: usize, height: usize, paint: impl Fn(usize, usize) -> [u8; 3]) -> Frame {
    let rgb = (0..height)
        .flat_map(|y| (0..width).map(move |x| (x, y)))
        .flat_map(|(x, y)| paint(x, y))
        .collect();
    Frame { width, height, rgb }
}

#[test]
fn a_dense_grid_gets_a_tag_per_square() {
    // The audit's dense frame (2026-10-24), smaller: 9 px dark squares every
    // 16 px. Thousands of boxes, each a target; the all-pairs searches it
    // once took 225 ms on at 2560x1440 now use `Buckets`.
    let f = frame(640, 320, |x, y| {
        if (3..12).contains(&(x % 16)) && (3..12).contains(&(y % 16)) {
            [20; 3]
        } else {
            [230; 3]
        }
    });
    for detector in [Detector::Full, Detector::Fast] {
        let found = find(&f, detector, false);
        assert_eq!(found.direct.len(), 40 * 20, "{detector:?}");
        assert!(found.blocks.is_empty() && found.media.is_empty());
    }
}

#[test]
fn a_blank_frame_has_nothing() {
    for detector in [Detector::Full, Detector::Fast] {
        assert_eq!(
            find(&frame(64, 48, |_, _| [30, 30, 30]), detector, false),
            Found::default()
        );
        assert_eq!(
            find(&frame(0, 0, |_, _| [0; 3]), detector, false),
            Found::default()
        );
    }
}

#[test]
fn a_button_is_one_tag() {
    // A light 80x24 button with a dark border on a mid-grey window.
    let f = frame(200, 100, |x, y| {
        let inside = (60..140).contains(&x) && (40..64).contains(&y);
        let border = inside && (x == 60 || x == 139 || y == 40 || y == 63);
        match (inside, border) {
            (_, true) => [20, 20, 20],
            (true, false) => [230, 230, 230],
            _ => [128, 128, 128],
        }
    });
    for detector in [Detector::Full, Detector::Fast] {
        let found = find(&f, detector, false);
        assert_eq!(found.direct.len(), 1, "{detector:?}: {found:?}");
        let (x, y) = group::centre(found.direct[0]);
        assert!(
            (95..105).contains(&x) && (47..57).contains(&y),
            "{detector:?}: {found:?}"
        );
    }
}

/// One capture of the prototype's corpus, with the word boxes the
/// prototype grouped, in its order.
#[derive(Deserialize)]
struct Case {
    name: String,
    png: String,
    full: Vec<Corners>,
    fast: Vec<Corners>,
    /// Grouped as a terminal's text.
    #[serde(default)]
    terminal: bool,
}

fn found_json(found: &Found) -> serde_json::Value {
    json!({
        "direct": found.direct,
        "blocks": found.blocks.iter().map(|b| json!({"rect": b.rect, "phrases": b.phrases})).collect::<Vec<_>>(),
        "media": found.media,
        "input": found.input,
    })
}

fn median_ms(mut v: Vec<f64>) -> f64 {
    v.sort_unstable_by(f64::total_cmp);
    v[v.len() / 2]
}

/// The port against the prototype: `WAYHINT_PIXEL_CORPUS` names a JSON
/// list of [`Case`]s, the results go to `WAYHINT_PIXEL_OUT`, and a script
/// compares them (`mds/spike-pixels/port/compare.py`). Run in release for
/// the timings:
/// `cargo test --release pixels::tests::corpus -- --ignored --nocapture`.
#[test]
#[ignore = "needs the local pixel corpus"]
fn corpus() {
    let manifest = std::env::var("WAYHINT_PIXEL_CORPUS").expect("WAYHINT_PIXEL_CORPUS");
    let out = std::env::var("WAYHINT_PIXEL_OUT").expect("WAYHINT_PIXEL_OUT");
    let runs: usize = std::env::var("WAYHINT_PIXEL_RUNS").map_or(5, |v| v.parse().unwrap());
    let cases: Vec<Case> = serde_json::from_reader(File::open(manifest).unwrap()).unwrap();
    let mut rows = Vec::new();
    for case in &cases {
        let frame = load(Path::new(&case.png));
        let time = |f: &dyn Fn()| {
            median_ms(
                (0..runs)
                    .map(|_| {
                        let start = Instant::now();
                        f();
                        start.elapsed().as_secs_f64() * 1000.0
                    })
                    .collect(),
            )
        };
        let full_policy = Detector::Full.policy();
        let fast_policy = Detector::Fast.policy();
        let row = json!({
            "name": case.name,
            "contour": contour::boxes(&frame),
            "segment": segment::boxes(&frame),
            "grouped_full": found_json(&group::group(&frame, &case.full, &full_policy)),
            "grouped_fast": found_json(&group::group(&frame, &case.fast, &fast_policy)),
            "find_full": found_json(&find(&frame, Detector::Full, case.terminal)),
            "find_fast": found_json(&find(&frame, Detector::Fast, case.terminal)),
            "ms": {
                "contour": time(&|| drop(contour::boxes(&frame))),
                "segment": time(&|| drop(segment::boxes(&frame))),
                "full": time(&|| drop(find(&frame, Detector::Full, case.terminal))),
                "fast": time(&|| drop(find(&frame, Detector::Fast, case.terminal))),
            },
        });
        eprintln!("{}: {}", case.name, row["ms"]);
        rows.push(row);
    }
    serde_json::to_writer(File::create(out).unwrap(), &rows).unwrap();
}

#[test]
fn a_scaled_frame_is_read_at_logical_size() {
    let checks = frame(5, 4, |x, _| {
        if x % 2 == 0 {
            [0, 0, 0]
        } else {
            [255, 255, 255]
        }
    });
    let (small, left) = shrink(checks, 2);
    assert_eq!((small.width, small.height), (3, 2));
    assert_eq!(
        &small.rgb[..3],
        &[128, 128, 128],
        "a 2x2 block of black and white"
    );
    assert_eq!(
        &small.rgb[6..9],
        &[0, 0, 0],
        "the last column holds black only"
    );
    assert!((left - 1.5).abs() < 1e-9, "3 px for 2 logical px");
    let (same, one) = shrink(frame(10, 4, |_, _| [9; 3]), 10);
    assert_eq!((same.width, one), (10, 1.0));
}

#[test]
fn terminal_nvim_gets_one_focus_hint_on_clear_lower_pixels() {
    let frame = load(Path::new("tests/fixtures/terminal-nvim.png"));
    for detector in [Detector::Fast, Detector::Full] {
        let found = find(&frame, detector, true);
        assert!(found.blocks.is_empty() && found.media.is_empty() && found.input);
        assert_eq!(found.direct.len(), 1);
        let [x, y, r, b] = found.direct[0];
        assert!(y > frame.height as i32 / 2);
        assert_eq!(
            crate::space::Space::pixels(&frame).ink(crate::geometry::Rect::new(x, y, r - x, b - y)),
            0
        );
    }
}

#[test]
fn terminal_focus_is_near_the_lower_center_and_always_one_hint() {
    for (width, height) in [(900, 650), (80, 40), (8, 8)] {
        let frame = frame(width, height, |_, _| [20; 3]);
        let found = find(&frame, Detector::Fast, true);
        assert!(found.input && found.blocks.is_empty() && found.media.is_empty());
        assert_eq!(found.direct.len(), 1);
        let [x, y, r, b] = found.direct[0];
        assert!(x >= 0 && y >= 0 && r <= width as i32 && b <= height as i32);
        if width > 80 {
            assert!((i32::midpoint(x, r) - width as i32 / 2).abs() <= 4);
            assert!((height as i32 - b - 18).abs() <= 4);
        }
    }
}

#[test]
#[ignore = "live TUI captures supplied by WAYHINT_PIXEL_CORPUS"]
fn terminal_tui_corpus() {
    let path = std::env::var("WAYHINT_PIXEL_CORPUS").unwrap();
    let cases: Vec<Case> = serde_json::from_reader(File::open(path).unwrap()).unwrap();
    for case in cases {
        let frame = load(Path::new(&case.png));
        let start = Instant::now();
        let found = find(&frame, Detector::Fast, true);
        assert!(
            found.input && found.blocks.is_empty() && found.media.is_empty(),
            "{}",
            case.name
        );
        assert_eq!(found.direct.len(), 1, "{}", case.name);
        let [x, y, r, b] = found.direct[0];
        assert!(y >= frame.height as i32 / 2, "{}", case.name);
        assert!(
            (i32::midpoint(x, r) - frame.width as i32 / 2).abs() < frame.width as i32 / 4,
            "{}",
            case.name
        );
        assert_eq!(
            crate::space::Space::pixels(&frame).ink(crate::geometry::Rect::new(x, y, r - x, b - y)),
            0,
            "{}",
            case.name
        );
        eprintln!(
            "{}: target={:?}, {:.2}ms",
            case.name,
            found.direct,
            start.elapsed().as_secs_f64() * 1000.0
        );
    }
}
