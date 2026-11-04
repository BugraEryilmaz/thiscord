//! Placement of call media tiles (screen shares and cameras).
//!
//! Tiles keep their stream's aspect ratio. The grid tries every way of
//! splitting the ordered tiles into rows and keeps the arrangement that shows
//! the most video while keeping rows a similar height, so two 16:9 shares stack
//! in a squarish area but sit side by side in a wide one. Focus puts one tile
//! on the stage and the others in a strip beside or below it.

/// A tile's box in CSS pixels, relative to the media area.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}
impl Rect {
    pub fn area(&self) -> f64 {
        self.w * self.h
    }
}

/// Where tiles go, and how tall the content is (taller than the area when it scrolls).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Placement {
    pub tiles: Vec<Rect>,
    /// The focused tile's index into `tiles`, when focus is active.
    pub stage: Option<usize>,
    pub height: f64,
    pub scrolls: bool,
}

/// Width/height of a stream before its first frame arrives.
pub const DEFAULT_ASPECT: f64 = 16.0 / 9.0;
/// When tiles average less than this square (by area) they are unusable and
/// the grid scrolls instead. A single narrow portrait tile does not count alone.
pub const MIN_TILE_SIDE: f64 = 140.0;
/// Narrowest column when the grid scrolls.
const MIN_COLUMN: f64 = 320.0;
/// Arrangements are searched exhaustively up to this many tiles.
const EXHAUSTIVE: usize = 12;

/// Clamps an aspect ratio to something drawable (bad metadata, 1-pixel streams).
pub fn sane_aspect(aspect: f64) -> f64 {
    if aspect.is_finite() && aspect > 0.0 {
        aspect.clamp(0.25, 4.0)
    } else {
        DEFAULT_ASPECT
    }
}

/// Lays out `aspects` (width/height, in order) inside `width` × `height`.
pub fn grid(width: f64, height: f64, gap: f64, aspects: &[f64]) -> Placement {
    let aspects: Vec<f64> = aspects.iter().map(|a| sane_aspect(*a)).collect();
    if aspects.is_empty() || width <= 0.0 || height <= 0.0 {
        return Placement {
            height: height.max(0.0),
            ..Default::default()
        };
    }
    let best = best_rows(width, height, gap, &aspects);
    let floor = MIN_TILE_SIDE.min(width.min(height) * 0.5);
    let typical = best.iter().map(|r| r.area().sqrt()).sum::<f64>() / best.len() as f64;
    if typical >= floor && best.iter().all(|r| r.w.max(r.h) >= floor * 0.75) {
        return Placement {
            tiles: best,
            stage: None,
            height,
            scrolls: false,
        };
    }
    column(width, height, gap, &aspects, MIN_COLUMN)
}

/// Too many tiles for the area: columns at least `min_column` wide that scroll.
fn column(width: f64, height: f64, gap: f64, aspects: &[f64], min_column: f64) -> Placement {
    let columns = (((width + gap) / (min_column + gap)).floor() as usize).max(1);
    let tile_width = (width - gap * (columns as f64 - 1.0)) / columns as f64;
    // Portrait streams would be taller than the screen; cap them.
    let cap = (height * 0.8).max(160.0);
    let mut tiles = Vec::with_capacity(aspects.len());
    let mut y = 0.0;
    for row in aspects.chunks(columns) {
        let row_height = row
            .iter()
            .map(|a| (tile_width / a).min(cap))
            .fold(0.0, f64::max);
        for (i, aspect) in row.iter().enumerate() {
            let h = (tile_width / aspect).min(cap);
            let w = h * aspect;
            let cell_x = i as f64 * (tile_width + gap);
            tiles.push(Rect {
                x: cell_x + (tile_width - w) / 2.0,
                y: y + (row_height - h) / 2.0,
                w,
                h,
            });
        }
        y += row_height + gap;
    }
    let content = (y - gap).max(0.0);
    Placement {
        tiles,
        stage: None,
        height: content.max(height),
        scrolls: content > height + 0.5,
    }
}

/// The best row arrangement that fits the area, without scrolling.
fn best_rows(width: f64, height: f64, gap: f64, aspects: &[f64]) -> Vec<Rect> {
    let n = aspects.len();
    let mut best: Option<(f64, Vec<Rect>)> = None;
    let mut consider = |rows: &[usize]| {
        for uniform in [false, true] {
            let tiles = place_rows(width, height, gap, aspects, rows, uniform);
            let score = score(&tiles, rows);
            if best.as_ref().is_none_or(|(s, _)| score > *s + 1e-6) {
                best = Some((score, tiles));
            }
        }
    };
    if n <= EXHAUSTIVE {
        // Each bit between two neighbours decides whether a new row starts there.
        for mask in 0u32..(1 << (n - 1)) {
            let mut rows = Vec::new();
            let mut len = 1;
            for i in 0..n - 1 {
                if mask & (1 << i) != 0 {
                    rows.push(len);
                    len = 1;
                } else {
                    len += 1;
                }
            }
            rows.push(len);
            consider(&rows);
        }
    } else {
        for count in 1..=n {
            consider(&balanced(aspects, count));
        }
    }
    best.map(|(_, tiles)| tiles).unwrap_or_default()
}

/// Splits tiles into `count` rows of similar total aspect (greedy, in order).
fn balanced(aspects: &[f64], count: usize) -> Vec<usize> {
    let total: f64 = aspects.iter().sum();
    let target = total / count as f64;
    let mut rows = Vec::new();
    let mut len = 0;
    let mut sum = 0.0;
    for (i, aspect) in aspects.iter().enumerate() {
        let remaining_tiles = aspects.len() - i;
        let remaining_rows = count - rows.len();
        if len > 0
            && remaining_rows > 1
            && (sum + aspect / 2.0 > target || remaining_tiles < remaining_rows)
        {
            rows.push(len);
            len = 0;
            sum = 0.0;
        }
        len += 1;
        sum += aspect;
    }
    rows.push(len);
    rows
}

/// Places rows of tiles. Justified rows fill the width at their own heights;
/// uniform rows share one height. Both shrink to fit and are centred.
fn place_rows(
    width: f64,
    height: f64,
    gap: f64,
    aspects: &[f64],
    rows: &[usize],
    uniform: bool,
) -> Vec<Rect> {
    let mut heights = Vec::with_capacity(rows.len());
    let mut start = 0;
    for len in rows {
        let sum: f64 = aspects[start..start + len].iter().sum();
        heights.push(((width - gap * (*len as f64 - 1.0)) / sum).max(0.0));
        start += len;
    }
    let available = (height - gap * (rows.len() as f64 - 1.0)).max(0.0);
    if uniform {
        let shared = heights
            .iter()
            .copied()
            .fold(f64::INFINITY, f64::min)
            .min(available / rows.len() as f64);
        heights.iter_mut().for_each(|h| *h = shared);
    } else {
        let natural: f64 = heights.iter().sum();
        if natural > available {
            let scale = available / natural;
            heights.iter_mut().for_each(|h| *h *= scale);
        }
    }
    let content = heights.iter().sum::<f64>() + gap * (rows.len() as f64 - 1.0);
    let mut y = ((height - content) / 2.0).max(0.0);
    let mut tiles = Vec::with_capacity(aspects.len());
    let mut start = 0;
    for (len, h) in rows.iter().zip(&heights) {
        let row = &aspects[start..start + len];
        let row_width = row.iter().map(|a| a * h).sum::<f64>() + gap * (*len as f64 - 1.0);
        let mut x = ((width - row_width) / 2.0).max(0.0);
        for aspect in row {
            let w = aspect * h;
            tiles.push(Rect { x, y, w, h: *h });
            x += w + gap;
        }
        y += h + gap;
        start += len;
    }
    tiles
}

/// Visible video area, discounted when rows differ in height so that no stream
/// becomes a thumbnail just to enlarge another.
fn score(tiles: &[Rect], rows: &[usize]) -> f64 {
    let area: f64 = tiles.iter().map(Rect::area).sum();
    let mut start = 0;
    let (mut low, mut high) = (f64::INFINITY, 0.0f64);
    for len in rows {
        let h = tiles[start].h;
        low = low.min(h);
        high = high.max(h);
        start += len;
    }
    if high <= 0.0 { 0.0 } else { area * low / high }
}

/// One tile on the stage and the rest in a strip on the side that leaves the
/// stage larger. `focused` indexes `aspects`.
pub fn focus(width: f64, height: f64, gap: f64, aspects: &[f64], focused: usize) -> Placement {
    if focused >= aspects.len() {
        return grid(width, height, gap, aspects);
    }
    let main = sane_aspect(aspects[focused]);
    let others: Vec<f64> = aspects
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != focused)
        .map(|(_, a)| sane_aspect(*a))
        .collect();
    if others.is_empty() || width <= 0.0 || height <= 0.0 {
        let mut placement = grid(width, height, gap, &[main]);
        placement.stage = (!placement.tiles.is_empty()).then_some(0);
        return placement;
    }
    let side = (width * 0.22).clamp(150.0, 300.0);
    let below = (height * 0.24).clamp(96.0, 190.0);
    let fit = |w: f64, h: f64| {
        let (fw, fh) = if w / h > main {
            (h * main, h)
        } else {
            (w, w / main)
        };
        Rect {
            x: (w - fw) / 2.0,
            y: (h - fh) / 2.0,
            w: fw,
            h: fh,
        }
    };
    let right_stage = fit((width - side - gap).max(0.0), height);
    let bottom_stage = fit(width, (height - below - gap).max(0.0));
    // A side strip needs room for a usable stage; phones use the bottom strip.
    let use_side = width - side - gap >= 320.0 && right_stage.area() >= bottom_stage.area();
    if !use_side {
        let strip = best_rows(width, below, gap, &others);
        // Phones: thumbnails this small are unreadable, so the stage sits on
        // top and the others follow in two scrolling columns.
        if strip.iter().any(|r| r.area().sqrt() < 110.0) {
            return stacked_focus(width, height, gap, main, &others, focused);
        }
    }
    let (stage, strip_origin, strip) = if use_side {
        (
            right_stage,
            (width - side, 0.0),
            best_rows(side, height, gap, &others),
        )
    } else {
        (
            bottom_stage,
            (0.0, height - below),
            best_rows(width, below, gap, &others),
        )
    };
    let mut tiles = Vec::with_capacity(aspects.len());
    let mut strip_tiles = strip.into_iter();
    for i in 0..aspects.len() {
        if i == focused {
            tiles.push(stage);
        } else if let Some(r) = strip_tiles.next() {
            tiles.push(Rect {
                x: r.x + strip_origin.0,
                y: r.y + strip_origin.1,
                ..r
            });
        }
    }
    Placement {
        tiles,
        stage: Some(focused),
        height,
        scrolls: false,
    }
}

fn stacked_focus(
    width: f64,
    height: f64,
    gap: f64,
    main: f64,
    others: &[f64],
    focused: usize,
) -> Placement {
    let stage_height = (width / main).min(height * 0.62);
    let stage = Rect {
        x: (width - stage_height * main) / 2.0,
        y: 0.0,
        w: stage_height * main,
        h: stage_height,
    };
    let rest = column(width, height, gap, others, 150.0);
    let top = stage_height + gap;
    let mut rest_tiles = rest.tiles.into_iter().map(|r| Rect { y: r.y + top, ..r });
    let mut tiles = Vec::with_capacity(others.len() + 1);
    for i in 0..=others.len() {
        if i == focused {
            tiles.push(stage);
        } else if let Some(r) = rest_tiles.next() {
            tiles.push(r);
        }
    }
    let content = tiles.iter().map(|r| r.y + r.h).fold(0.0, f64::max);
    Placement {
        tiles,
        stage: Some(focused),
        height: content.max(height),
        scrolls: content > height + 0.5,
    }
}

/// Familiar name for a received resolution: 720p, 1080p, 1440p, 4K; others
/// as their short side, e.g. 540p.
pub fn resolution_label(width: u32, height: u32) -> String {
    let short = width.min(height);
    if short == 0 {
        return String::new();
    }
    for (standard, label) in [
        (2160, "4K"),
        (1440, "1440p"),
        (1080, "1080p"),
        (720, "720p"),
        (480, "480p"),
        (360, "360p"),
    ] {
        if (short as f64 - standard as f64).abs() <= standard as f64 * 0.04 {
            return label.into();
        }
    }
    format!("{short}p")
}

/// Rounds a measured frame rate to the nominal rate it is close to (59.6 → 60).
pub fn nominal_fps(measured: f64) -> u32 {
    if !measured.is_finite() || measured < 0.5 {
        return 0;
    }
    for nominal in [
        5.0, 10.0, 15.0, 24.0, 25.0, 30.0, 48.0, 50.0, 60.0, 90.0, 120.0, 144.0,
    ] {
        if (measured - nominal).abs() <= nominal * 0.06 {
            return nominal as u32;
        }
    }
    measured.round() as u32
}

/// Moves the item at `from` to `to`, shifting the items between.
pub fn move_item<T>(items: &mut Vec<T>, from: usize, to: usize) {
    if from < items.len() && to < items.len() && from != to {
        let item = items.remove(from);
        items.insert(to, item);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const WIDE: f64 = 16.0 / 9.0;

    fn no_overlap(tiles: &[Rect]) {
        for (i, a) in tiles.iter().enumerate() {
            for b in &tiles[i + 1..] {
                let overlap_x = a.x < b.x + b.w - 0.5 && b.x < a.x + a.w - 0.5;
                let overlap_y = a.y < b.y + b.h - 0.5 && b.y < a.y + a.h - 0.5;
                assert!(!(overlap_x && overlap_y), "{a:?} overlaps {b:?}");
            }
        }
    }
    fn inside(p: &Placement, width: f64) {
        for t in &p.tiles {
            assert!(t.x >= -0.5 && t.y >= -0.5, "{t:?}");
            assert!(
                t.x + t.w <= width + 0.5 && t.y + t.h <= p.height + 0.5,
                "{t:?}"
            );
        }
    }

    #[test]
    fn two_wide_shares_stack_in_a_squarish_area_and_sit_side_by_side_in_a_wide_one() {
        let square = grid(1060.0, 760.0, 12.0, &[WIDE, WIDE]);
        assert!(square.tiles[1].y > square.tiles[0].y + square.tiles[0].h - 1.0);
        assert!(square.tiles[0].w > 600.0);
        let wide = grid(1600.0, 600.0, 12.0, &[WIDE, WIDE]);
        assert!((wide.tiles[0].y - wide.tiles[1].y).abs() < 0.5);
        for p in [&square, &wide] {
            no_overlap(&p.tiles);
        }
    }

    #[test]
    fn tiles_keep_their_stream_aspect_ratio() {
        let aspects = [WIDE, 2.39, 0.5625, 4.0 / 3.0, WIDE];
        let p = grid(1100.0, 820.0, 12.0, &aspects);
        assert_eq!(p.tiles.len(), aspects.len());
        assert!(!p.scrolls);
        for (t, a) in p.tiles.iter().zip(aspects) {
            assert!((t.w / t.h - a).abs() < 0.01, "{t:?} vs {a}");
        }
        no_overlap(&p.tiles);
        inside(&p, 1100.0);
    }

    #[test]
    fn mixed_shapes_use_most_of_the_area() {
        let aspects = [WIDE, 2.39, 0.5625, 4.0 / 3.0, WIDE, WIDE, WIDE];
        let p = grid(1100.0, 820.0, 12.0, &aspects);
        let used: f64 = p.tiles.iter().map(Rect::area).sum();
        assert!(used / (1100.0 * 820.0) > 0.55, "{used}");
    }

    #[test]
    fn equal_tiles_get_equal_sizes() {
        let p = grid(1100.0, 800.0, 12.0, &[WIDE; 3]);
        let first = p.tiles[0].h;
        assert!(p.tiles.iter().all(|t| (t.h - first).abs() < 0.5), "{p:?}");
        let p = grid(1100.0, 800.0, 12.0, &[WIDE; 9]);
        assert!(p.tiles.iter().all(|t| (t.h - p.tiles[0].h).abs() < 0.5));
    }

    #[test]
    fn many_tiles_in_a_phone_scroll_in_a_column() {
        let p = grid(366.0, 560.0, 8.0, &[WIDE; 7]);
        assert!(p.scrolls);
        assert!(p.tiles.iter().all(|t| t.w > 360.0));
        no_overlap(&p.tiles);
        inside(&p, 366.0);
        // Portrait streams never exceed the screen.
        let p = grid(366.0, 560.0, 8.0, &[0.5625, WIDE, WIDE, WIDE, WIDE]);
        assert!(p.tiles[0].h <= 560.0 * 0.8 + 0.5);
    }

    #[test]
    fn a_laptop_fits_a_mixed_call_without_scrolling() {
        let aspects = [WIDE, WIDE, 2.39, 0.5625, 4.0 / 3.0, WIDE, WIDE];
        let p = grid(640.0, 640.0, 8.0, &aspects);
        assert!(!p.scrolls, "{p:?}");
        no_overlap(&p.tiles);
        inside(&p, 640.0);
    }

    #[test]
    fn single_tile_fills_the_area() {
        let p = grid(1000.0, 700.0, 12.0, &[WIDE]);
        assert!((p.tiles[0].w - 1000.0).abs() < 0.5);
        let p = grid(1000.0, 700.0, 12.0, &[0.5625]);
        assert!((p.tiles[0].h - 700.0).abs() < 0.5);
    }

    #[test]
    fn large_calls_and_degenerate_input_stay_valid() {
        let aspects: Vec<f64> = (0..25)
            .map(|i| if i % 3 == 0 { 0.75 } else { WIDE })
            .collect();
        let p = grid(1600.0, 1000.0, 12.0, &aspects);
        assert_eq!(p.tiles.len(), 25);
        no_overlap(&p.tiles);
        inside(&p, 1600.0);
        assert!(grid(0.0, 0.0, 12.0, &[WIDE]).tiles.is_empty());
        let p = grid(800.0, 600.0, 12.0, &[f64::NAN, 0.0, 1000.0]);
        assert_eq!(p.tiles.len(), 3);
        assert!(p.tiles.iter().all(|t| t.w.is_finite() && t.h > 0.0));
    }

    #[test]
    fn focus_puts_the_stage_beside_or_above_a_strip() {
        let p = focus(1600.0, 800.0, 12.0, &[WIDE, WIDE, 0.5625, WIDE], 1);
        assert_eq!(p.stage, Some(1));
        let stage = p.tiles[1];
        assert!(
            p.tiles
                .iter()
                .enumerate()
                .all(|(i, t)| i == 1 || t.area() < stage.area() / 4.0)
        );
        no_overlap(&p.tiles);
        inside(&p, 1600.0);
        // A wide area keeps a side strip; on a phone the strip goes below.
        assert!(p.tiles[0].x > stage.x + stage.w - 1.0);
        let phone = focus(366.0, 600.0, 8.0, &[WIDE; 7], 0);
        assert!(phone.tiles[1].y > phone.tiles[0].y + phone.tiles[0].h - 1.0);
        assert!((phone.tiles[0].w - 366.0).abs() < 0.5);
        // The others stay readable: two columns, not thumbnails.
        assert!(phone.tiles[1..].iter().all(|t| t.w > 170.0));
        no_overlap(&phone.tiles);
        inside(&phone, 366.0);
        let alone = focus(800.0, 600.0, 12.0, &[WIDE], 0);
        assert_eq!(alone.stage, Some(0));
        assert_eq!(focus(800.0, 600.0, 12.0, &[WIDE], 3).stage, None);
    }

    #[test]
    fn familiar_quality_labels() {
        assert_eq!(resolution_label(3840, 2160), "4K");
        assert_eq!(resolution_label(1920, 1080), "1080p");
        assert_eq!(resolution_label(1920, 1088), "1080p");
        assert_eq!(resolution_label(3440, 1440), "1440p");
        assert_eq!(resolution_label(1720, 720), "720p");
        assert_eq!(resolution_label(1080, 1920), "1080p");
        assert_eq!(resolution_label(540, 960), "540p");
        assert_eq!(resolution_label(640, 360), "360p");
        assert_eq!(resolution_label(0, 0), "");
        assert_eq!(nominal_fps(59.4), 60);
        assert_eq!(nominal_fps(29.2), 30);
        assert_eq!(nominal_fps(14.6), 15);
        assert_eq!(nominal_fps(41.0), 41);
        assert_eq!(nominal_fps(0.2), 0);
    }

    #[test]
    fn moving_items_shifts_the_rest() {
        let mut v = vec!['a', 'b', 'c', 'd'];
        move_item(&mut v, 0, 2);
        assert_eq!(v, ['b', 'c', 'a', 'd']);
        move_item(&mut v, 3, 0);
        assert_eq!(v, ['d', 'b', 'c', 'a']);
        move_item(&mut v, 9, 0);
        assert_eq!(v, ['d', 'b', 'c', 'a']);
    }
}
