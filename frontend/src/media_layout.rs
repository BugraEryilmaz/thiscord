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
/// No tile, including a portrait one, should be narrower than this when an
/// arrangement can avoid it (its caption and quality need the room).
pub const MIN_TILE_WIDTH: f64 = 96.0;
/// Focus-strip thumbnails may be smaller than grid tiles, but never thinner
/// than this: below it a side column is used instead.
pub const MIN_THUMB_WIDTH: f64 = 60.0;
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
    let floor = MIN_TILE_SIDE.min(width.min(height) * 0.5);
    let narrowest = MIN_TILE_WIDTH.min(width * 0.5);
    let fits = |tiles: &[Rect]| {
        let typical = tiles.iter().map(|r| r.area().sqrt()).sum::<f64>() / tiles.len() as f64;
        typical >= floor && tiles.iter().all(|r| r.w >= narrowest * 0.6)
    };
    // The soft floor first; if that would scroll, the strict one, before
    // giving up and scrolling.
    for strict in [false, true] {
        let best = search_rows(width, height, gap, &aspects, strict);
        if fits(&best) {
            return Placement {
                tiles: best,
                stage: None,
                height,
                scrolls: false,
            };
        }
    }
    column(width, height, gap, &aspects, MIN_COLUMN)
}

/// Too many tiles for the area: columns at least `min_column` wide that scroll.
fn column(width: f64, height: f64, gap: f64, aspects: &[f64], min_column: f64) -> Placement {
    let columns =
        (((width + gap) / (min_column + gap)).floor() as usize).clamp(1, aspects.len().max(1));
    let tile_width = (width - gap * (columns as f64 - 1.0)) / columns as f64;
    // Portrait streams would be taller than the screen; cap them.
    let cap = (height * 0.8).max(160.0);
    // Each tile joins the shortest column, so mixed shapes leave no holes.
    let mut bottoms = vec![0.0f64; columns];
    let mut tiles = Vec::with_capacity(aspects.len());
    for aspect in aspects {
        let column = (0..columns)
            .min_by(|a, b| bottoms[*a].total_cmp(&bottoms[*b]))
            .unwrap_or(0);
        let h = (tile_width / aspect).min(cap);
        let w = h * aspect;
        tiles.push(Rect {
            x: column as f64 * (tile_width + gap) + (tile_width - w) / 2.0,
            y: bottoms[column],
            w,
            h,
        });
        bottoms[column] += h + gap;
    }
    let content = (bottoms.iter().fold(0.0f64, |a, b| a.max(*b)) - gap).max(0.0);
    Placement {
        tiles,
        stage: None,
        height: content.max(height),
        scrolls: content > height + 0.5,
    }
}

/// The best row arrangement that fits the area, without scrolling.
/// `strict` drops every arrangement with a tile under the width floor (when
/// any arrangement can avoid it) instead of only scoring it lower.
fn search_rows(width: f64, height: f64, gap: f64, aspects: &[f64], strict: bool) -> Vec<Rect> {
    let n = aspects.len();
    let narrowest = MIN_TILE_WIDTH.min(width * 0.65);
    // A tile below the width floor costs score in proportion to the shortfall
    // (to the sixth power): a portrait sliver loses, but a couple of pixels never outweigh
    // badly uneven rows.
    let mut best: Option<(f64, Vec<Rect>)> = None;
    let mut best_strict: Option<(f64, Vec<Rect>)> = None;
    let mut consider = |rows: &[usize]| {
        for mode in [Rows::Justified, Rows::Uniform, Rows::Capped] {
            let tiles = place_rows(width, height, gap, aspects, rows, mode);
            let thinnest = tiles.iter().map(|t| t.w).fold(f64::INFINITY, f64::min);
            let floor = (thinnest / narrowest).min(1.0).powi(6);
            let score = score(&tiles, rows) * floor;
            if strict
                && thinnest >= narrowest
                && best_strict.as_ref().is_none_or(|(s, _)| score > *s + 1e-6)
            {
                best_strict = Some((score, tiles.clone()));
            }
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
    best_strict
        .or(best)
        .map(|(_, tiles)| tiles)
        .unwrap_or_default()
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
#[derive(Clone, Copy, PartialEq)]
enum Rows {
    /// Every row fills the width at its own height.
    Justified,
    /// All rows share one height.
    Uniform,
    /// Justified, but no row taller than twice the lowest one unless a
    /// portrait tile needs the height to stay `MIN_TILE_WIDTH` wide.
    Capped,
}

fn place_rows(
    width: f64,
    height: f64,
    gap: f64,
    aspects: &[f64],
    rows: &[usize],
    mode: Rows,
) -> Vec<Rect> {
    let mut heights = Vec::with_capacity(rows.len());
    let mut start = 0;
    for len in rows {
        let sum: f64 = aspects[start..start + len].iter().sum();
        heights.push(((width - gap * (*len as f64 - 1.0)) / sum).max(0.0));
        start += len;
    }
    let available = (height - gap * (rows.len() as f64 - 1.0)).max(0.0);
    if mode == Rows::Capped {
        let lowest = heights.iter().copied().fold(f64::INFINITY, f64::min);
        let mut start = 0;
        for (len, h) in rows.iter().zip(heights.iter_mut()) {
            let narrowest = aspects[start..start + len]
                .iter()
                .copied()
                .fold(f64::INFINITY, f64::min);
            *h = h.min((2.0 * lowest).max(MIN_TILE_WIDTH / narrowest));
            start += len;
        }
    }
    if mode == Rows::Uniform {
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
    if high <= 0.0 {
        return 0.0;
    }
    // Among similar arrangements, prefer rows of similar width, so no tile
    // sits alone in a mostly empty row.
    let mut start = 0;
    let (mut narrow, mut wide) = (f64::INFINITY, 0.0f64);
    for len in rows {
        let row = &tiles[start..start + len];
        let span = row.iter().map(|r| r.x + r.w).fold(0.0, f64::max)
            - row.iter().map(|r| r.x).fold(f64::INFINITY, f64::min);
        narrow = narrow.min(span);
        wide = wide.max(span);
        start += len;
    }
    let balance = if wide > 0.0 { narrow / wide } else { 1.0 };
    area * low / high * (0.85 + 0.15 * balance)
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
    let typical = |rects: &[Rect]| {
        rects.iter().map(|r| r.area().sqrt()).sum::<f64>() / rects.len().max(1) as f64
    };
    // Try strips of a few sizes beside and below the stage. The stage's area
    // dominates; a larger strip wins only where the stage would not grow (a
    // portrait stage leaves width to spare), and unreadable thumbnails lose.
    let mut best: Option<(f64, bool, Rect, Vec<Rect>)> = None;
    // A short area (a call strip above the chat) cannot scroll usefully, so
    // the strip stays beside the stage even on a phone.
    let short = height < 300.0;
    let (min_stage, min_side) = if short {
        (160.0, 100.0)
    } else {
        (320.0, 110.0)
    };
    // Narrow side strips are tried too, so the stage keeps most of the area.
    for fraction in [0.12, 0.16, 0.2, 0.26, 0.33, 0.4] {
        // Never clamp with min > max: tiny areas (a phone call strip) would panic.
        let side = (width * fraction).max(min_side).min(width * 0.45);
        if width - side - gap < min_stage {
            continue;
        }
        let stage = fit(width - side - gap, height);
        let strip = side_rows(side, height, gap, &others);
        consider_strip(&mut best, true, stage, strip, typical);
    }
    for fraction in [0.22, 0.3, 0.38] {
        let below = (height * fraction).max(90.0).min(height * 0.45);
        if height - below - gap < 160.0 {
            continue;
        }
        let stage = fit(width, height - below - gap);
        let strip = strip_rows(width, below, gap, &others);
        consider_strip(&mut best, false, stage, strip, typical);
    }
    let Some((_, use_side, stage, strip)) = best else {
        return stacked_focus(width, height, gap, main, &others, focused);
    };
    // Phones: thumbnails this small are unreadable, so the stage sits on top
    // and the others follow in two scrolling columns.
    if width < 600.0 && !short && strip.iter().any(|r| r.area().sqrt() < 110.0) {
        return stacked_focus(width, height, gap, main, &others, focused);
    }
    let (mut stage, mut strip) = (stage, strip);
    if use_side && stage.h < height - 1.0 {
        let side = strip.iter().map(|r| r.x + r.w).fold(0.0, f64::max).max(1.0);
        let fitted = side_rows(side, stage.h, gap, &others);
        if fitted
            .iter()
            .all(|t| t.w >= MIN_TILE_WIDTH.min(side * 0.65))
            && typical(&fitted) >= 0.85 * typical(&strip)
        {
            strip = fitted
                .into_iter()
                .map(|r| Rect {
                    y: r.y + stage.y,
                    ..r
                })
                .collect();
        }
    } else if use_side {
        let side = strip.iter().map(|r| r.x + r.w).fold(0.0, f64::max);
        let spare = width - gap - stage.w - side;
        if spare > 4.0 {
            let wider = side_rows((side + spare).min(width * 0.5), height, gap, &others);
            if typical(&wider) > typical(&strip) {
                strip = wider;
            }
        }
    } else {
        let below = strip.iter().map(|r| r.y + r.h).fold(0.0, f64::max);
        let spare = height - gap - stage.h - below;
        if spare > 4.0 {
            let taller = strip_rows(width, below + spare, gap, &others);
            if typical(&taller) > typical(&strip) {
                strip = taller;
            }
        }
    }
    // The stage and its strip are centred together, so no gap opens between them.
    let start = |rects: &[Rect], x: bool| {
        rects
            .iter()
            .map(|r| if x { r.x } else { r.y })
            .fold(f64::INFINITY, f64::min)
    };
    let end = |rects: &[Rect], x: bool| {
        rects
            .iter()
            .map(|r| if x { r.x + r.w } else { r.y + r.h })
            .fold(0.0, f64::max)
    };
    let (strip_start, strip_end) = (start(&strip, use_side), end(&strip, use_side));
    let (stage_size, total) = if use_side {
        (stage.w, width)
    } else {
        (stage.h, height)
    };
    let block = stage_size + gap + (strip_end - strip_start);
    let lead = ((total - block) / 2.0).max(0.0);
    let strip_shift = lead + stage_size + gap - strip_start;
    if use_side {
        stage.x = lead;
        strip.iter_mut().for_each(|r| r.x += strip_shift);
    } else {
        stage.y = lead;
        strip.iter_mut().for_each(|r| r.y += strip_shift);
    }
    let mut tiles = Vec::with_capacity(aspects.len());
    let mut strip_tiles = strip.into_iter();
    for i in 0..aspects.len() {
        if i == focused {
            tiles.push(stage);
        } else if let Some(r) = strip_tiles.next() {
            tiles.push(r);
        }
    }
    Placement {
        tiles,
        stage: Some(focused),
        height,
        scrolls: false,
    }
}

/// A strip beside the stage: one column, or two columns of paired tiles when a
/// single column would not fit; never a ragged mix.
fn side_rows(width: f64, height: f64, gap: f64, aspects: &[f64]) -> Vec<Rect> {
    let n = aspects.len();
    let mut options = vec![vec![1; n]];
    if n >= 4 {
        let mut pairs = vec![2; n / 2];
        if n % 2 == 1 {
            pairs.push(1);
        }
        options.push(pairs);
    }
    let narrowest = MIN_TILE_WIDTH.min(width * 0.65);
    let mut best: Option<(bool, f64, Vec<Rect>)> = None;
    for rows in options {
        for mode in [Rows::Justified, Rows::Uniform] {
            let tiles = place_rows(width, height, gap, aspects, &rows, mode);
            let wide = tiles.iter().all(|t| t.w >= narrowest);
            let area: f64 = tiles.iter().map(Rect::area).sum();
            if best
                .as_ref()
                .is_none_or(|(w, a, _)| (wide, area) > (*w, *a + 1e-6))
            {
                best = Some((wide, area, tiles));
            }
        }
    }
    best.map(|(_, _, tiles)| tiles).unwrap_or_default()
}

/// A strip under the stage: one row, or two rows of balanced length, so no
/// thumbnail sits alone in a row.
fn strip_rows(width: f64, height: f64, gap: f64, aspects: &[f64]) -> Vec<Rect> {
    let mut best: Option<(f64, Vec<Rect>)> = None;
    let mut options = vec![vec![aspects.len()]];
    if aspects.len() >= 4 {
        let first = aspects.len().div_ceil(2);
        options.push(vec![first, aspects.len() - first]);
    }
    for rows in options {
        let tiles = place_rows(width, height, gap, aspects, &rows, Rows::Uniform);
        let area: f64 = tiles.iter().map(Rect::area).sum();
        if best.as_ref().is_none_or(|(a, _)| area > *a + 1e-6) {
            best = Some((area, tiles));
        }
    }
    best.map(|(_, tiles)| tiles).unwrap_or_default()
}

fn consider_strip(
    best: &mut Option<(f64, bool, Rect, Vec<Rect>)>,
    side: bool,
    stage: Rect,
    strip: Vec<Rect>,
    typical: impl Fn(&[Rect]) -> f64,
) {
    // The stage's size comes first; thumbnail readability only weighs the
    // strip's share, so a slightly smaller thumbnail never costs the stage a
    // third of its area.
    let strip_area: f64 = strip.iter().map(Rect::area).sum();
    let thinnest = strip.iter().map(|t| t.w).fold(f64::INFINITY, f64::min);
    let readable =
        (typical(&strip) / 100.0).min(1.0) * (thinnest / MIN_TILE_WIDTH).min(1.0).powi(2);
    let mut score = stage.area() + 0.3 * strip_area * readable;
    // Soft: a thumbnail a pixel or two under the floor costs little; a sliver
    // costs the arrangement.
    score *= (thinnest / MIN_THUMB_WIDTH).min(1.0).powi(6);
    if best.as_ref().is_none_or(|(s, ..)| score > *s + 1e-6) {
        *best = Some((score, side, stage, strip));
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
    let stage_height = (width / main).min(height * if main < 1.0 { 0.85 } else { 0.62 });
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

/// One row at the full height of a short area, scrolling sideways: a phone
/// call strip keeps tiles large enough to read instead of shrinking them.
pub fn row(height: f64, gap: f64, aspects: &[f64]) -> Placement {
    let mut x = 0.0;
    let tiles = aspects
        .iter()
        .map(|a| {
            let w = sane_aspect(*a) * height;
            let tile = Rect {
                x,
                y: 0.0,
                w,
                h: height,
            };
            x += w + gap;
            tile
        })
        .collect();
    Placement {
        tiles,
        stage: None,
        height,
        scrolls: false,
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

/// An order for `aspects` that packs better, used until the user arranges the
/// tiles themselves: grouping similar shapes lets rows fill the width. Returns
/// indices into `aspects`; the given order wins unless another shows clearly
/// more video.
pub fn arrange(width: f64, height: f64, gap: f64, aspects: &[f64]) -> Vec<usize> {
    let identity: Vec<usize> = (0..aspects.len()).collect();
    if aspects.len() < 3 || width <= 0.0 || height <= 0.0 {
        return identity;
    }
    let shown = |order: &[usize]| {
        let ordered: Vec<f64> = order.iter().map(|i| aspects[*i]).collect();
        let p = grid(width, height, gap, &ordered);
        let area: f64 = p.tiles.iter().map(Rect::area).sum();
        if p.scrolls { area / 2.0 } else { area }
    };
    // Tiles of the same shape are interchangeable, so only distinct shape
    // sequences are tried (lexicographic permutations of shape classes).
    let mut by_shape = identity.clone();
    by_shape.sort_by(|a, b| sane_aspect(aspects[*a]).total_cmp(&sane_aspect(aspects[*b])));
    let mut classes = Vec::with_capacity(aspects.len());
    let mut members: Vec<Vec<usize>> = Vec::new();
    let mut previous = f64::NAN;
    for i in &by_shape {
        let shape = (sane_aspect(aspects[*i]) * 100.0).round();
        if shape != previous {
            members.push(Vec::new());
            previous = shape;
        }
        members.last_mut().expect("pushed above").push(*i);
        classes.push(members.len() - 1);
    }
    for group in &mut members {
        group.sort_unstable();
    }
    // Each try searches 2^(n-1) row splits; keep the whole search small.
    let budget = (40_000usize >> (aspects.len() - 1).min(15)).clamp(1, 720);
    let mut best = (shown(&identity) * 1.05, identity);
    for _ in 0..budget {
        let mut next = vec![0; members.len()];
        let order: Vec<usize> = classes
            .iter()
            .map(|c| {
                next[*c] += 1;
                members[*c][next[*c] - 1]
            })
            .collect();
        let area = shown(&order);
        if area > best.0 + 1e-6 {
            best = (area, order);
        }
        if !next_permutation(&mut classes) {
            break;
        }
    }
    best.1
}

/// Where a newly shaped tile fits best among tiles whose order stays fixed.
/// Returns an index into `aspects` (which includes the new tile at `new`).
pub fn insertion(width: f64, height: f64, gap: f64, aspects: &[f64], new: usize) -> usize {
    if new >= aspects.len() || width <= 0.0 || height <= 0.0 {
        return new.min(aspects.len());
    }
    let others: Vec<f64> = aspects
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != new)
        .map(|(_, a)| *a)
        .collect();
    let shown = |at: usize| {
        let mut ordered = others.clone();
        ordered.insert(at, aspects[new]);
        let p = grid(width, height, gap, &ordered);
        let area: f64 = p.tiles.iter().map(Rect::area).sum();
        if p.scrolls { area / 2.0 } else { area }
    };
    // Joining at the end wins unless another place shows clearly more video.
    let end = others.len();
    let mut best = (shown(end) * 1.03, end);
    for at in 0..end {
        let area = shown(at);
        if area > best.0 {
            best = (area, at);
        }
    }
    best.1
}

fn next_permutation(items: &mut [usize]) -> bool {
    let Some(pivot) = (1..items.len()).rev().find(|i| items[i - 1] < items[*i]) else {
        return false;
    };
    let swap = (pivot..items.len())
        .rev()
        .find(|i| items[*i] > items[pivot - 1])
        .expect("a larger item exists after the pivot");
    items.swap(pivot - 1, swap);
    items[pivot..].reverse();
    true
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
        // A wide area keeps a side strip right next to the stage; on a phone
        // the strip goes below.
        assert!(p.tiles[0].x > stage.x + stage.w - 1.0);
        assert!(p.tiles[0].x - (stage.x + stage.w) < 12.5);
        let call = focus(1080.0, 300.0, 8.0, &[WIDE, WIDE, WIDE], 0);
        let (s, t) = (call.tiles[0], call.tiles[1]);
        assert!((t.x - (s.x + s.w) - 8.0).abs() < 0.5, "{call:?}");
        no_overlap(&call.tiles);
        inside(&call, 1080.0);
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
    fn focus_handles_portrait_streams_on_desktop() {
        let call = [WIDE, 0.5625, 2.39, 4.0 / 3.0, WIDE, WIDE, WIDE];
        // A portrait thumbnail never throws a desktop into the phone layout.
        let p = focus(1080.0, 790.0, 12.0, &call, 0);
        assert!(!p.scrolls);
        assert!(p.tiles[0].w > 700.0, "{p:?}");
        // A portrait stage leaves width to spare: the strip takes it, right beside the stage.
        let p = focus(1080.0, 790.0, 12.0, &call, 1);
        let stage = p.tiles[1];
        assert!((stage.h - 790.0).abs() < 0.5);
        let strip_left = p
            .tiles
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != 1)
            .map(|(_, t)| t.x)
            .fold(f64::INFINITY, f64::min);
        assert!(
            (strip_left - (stage.x + stage.w) - 12.0).abs() < 0.5,
            "{p:?}"
        );
        assert!(
            p.tiles
                .iter()
                .enumerate()
                .all(|(i, t)| i == 1 || t.w > 150.0),
            "{p:?}"
        );
        no_overlap(&p.tiles);
        inside(&p, 1080.0);
    }

    #[test]
    fn bottom_strips_never_leave_a_thumbnail_alone_in_a_row() {
        let p = focus(
            1080.0,
            790.0,
            12.0,
            &[2.39, WIDE, WIDE, 4.0 / 3.0, WIDE, WIDE, WIDE],
            0,
        );
        let below = p.tiles[1..]
            .iter()
            .all(|t| t.y >= p.tiles[0].y + p.tiles[0].h - 0.5);
        assert!(below, "an ultrawide stage takes a strip below: {p:?}");
        let mut rows: Vec<f64> = p.tiles[1..].iter().map(|t| t.y.round()).collect();
        rows.dedup();
        let counts: Vec<usize> = rows
            .iter()
            .map(|y| p.tiles[1..].iter().filter(|t| t.y.round() == *y).count())
            .collect();
        assert!(counts.iter().all(|c| *c >= 2), "{counts:?} {p:?}");
    }

    #[test]
    fn automatic_order_packs_mixed_shapes_better_and_keeps_equal_ones() {
        let mixed = [WIDE, WIDE, 2.39, 0.5625, 4.0 / 3.0];
        let order = arrange(1080.0, 790.0, 12.0, &mixed);
        let mut sorted = order.clone();
        sorted.sort();
        assert_eq!(sorted, [0, 1, 2, 3, 4]);
        let area = |o: &[usize]| {
            let a: Vec<f64> = o.iter().map(|i| mixed[*i]).collect();
            grid(1080.0, 790.0, 12.0, &a)
                .tiles
                .iter()
                .map(Rect::area)
                .sum::<f64>()
        };
        assert!(area(&order) >= area(&[0, 1, 2, 3, 4]));
        assert!(area(&order) / (1080.0 * 790.0) > 0.6, "{order:?}");
        assert_eq!(arrange(1080.0, 790.0, 12.0, &[WIDE; 5]), [0, 1, 2, 3, 4]);
    }

    #[test]
    fn a_joining_stream_is_placed_without_moving_the_others() {
        // Wide tiles in a wide area: a portrait newcomer goes where it fits best.
        let aspects = [WIDE, WIDE, WIDE, WIDE, 0.5625];
        let at = insertion(1080.0, 790.0, 12.0, &aspects, 4);
        assert!(at <= 4);
        let mut ordered = aspects[..4].to_vec();
        ordered.insert(at, 0.5625);
        let area = |a: &[f64]| {
            grid(1080.0, 790.0, 12.0, a)
                .tiles
                .iter()
                .map(Rect::area)
                .sum::<f64>()
        };
        assert!(area(&ordered) >= area(&aspects) - 1.0);
        // Same shapes: it simply joins at the end.
        assert_eq!(insertion(1080.0, 790.0, 12.0, &[WIDE; 5], 4), 4);
    }

    #[test]
    fn phone_columns_leave_no_holes() {
        let p = focus(
            366.0,
            560.0,
            8.0,
            &[WIDE, 0.5625, WIDE, WIDE, 2.39, WIDE, 4.0 / 3.0],
            0,
        );
        assert!(p.scrolls);
        no_overlap(&p.tiles);
        // In each column, a tile starts right below the previous one.
        let rest: Vec<Rect> = p.tiles[1..].to_vec();
        for t in &rest {
            let above: Vec<&Rect> = rest
                .iter()
                .filter(|o| (o.x + o.w / 2.0 - (t.x + t.w / 2.0)).abs() < 1.0 && o.y < t.y)
                .collect();
            if let Some(nearest) = above.iter().map(|o| o.y + o.h).reduce(f64::max) {
                assert!(t.y - nearest <= 8.5, "{t:?} {rest:?}");
            }
        }
    }

    #[test]
    fn equally_large_grids_prefer_even_rows() {
        let p = grid(1080.0, 790.0, 12.0, &[WIDE; 6]);
        let mut rows: Vec<f64> = p.tiles.iter().map(|t| t.y.round()).collect();
        rows.dedup();
        let counts: Vec<usize> = rows
            .iter()
            .map(|y| p.tiles.iter().filter(|t| t.y.round() == *y).count())
            .collect();
        assert!(counts.iter().all(|c| *c == counts[0]), "{counts:?}");
    }

    #[test]
    fn tiny_areas_never_panic() {
        for (w, h) in [
            (0.0, 0.0),
            (40.0, 30.0),
            (300.0, 120.0),
            (366.0, 150.0),
            (150.0, 900.0),
            (2000.0, 60.0),
        ] {
            for n in 1..9 {
                let aspects: Vec<f64> = (0..n).map(|i| [WIDE, 0.5625, 2.39, 1.0][i % 4]).collect();
                let _ = grid(w, h, 8.0, &aspects);
                for f in 0..n {
                    let _ = focus(w, h, 8.0, &aspects, f);
                }
                let _ = arrange(w, h, 8.0, &aspects);
                let _ = insertion(w, h, 8.0, &aspects, n - 1);
            }
        }
    }

    #[test]
    fn short_call_strips_keep_every_stream_visible() {
        // A phone call strip: focus keeps the others beside the stage.
        let p = focus(366.0, 190.0, 8.0, &[WIDE, WIDE, WIDE], 0);
        assert!(!p.scrolls);
        inside(&p, 366.0);
        no_overlap(&p.tiles);
        assert!(
            p.tiles.iter().all(|t| t.y + t.h <= 190.5 && t.w > 90.0),
            "{p:?}"
        );
        // Without focus, a row at full height scrolls sideways.
        let r = row(190.0, 8.0, &[WIDE, 0.5625, WIDE]);
        assert!(r.tiles.iter().all(|t| (t.h - 190.0).abs() < 0.5));
        assert!(r.tiles[1].x > r.tiles[0].x + r.tiles[0].w);
        no_overlap(&r.tiles);
    }

    #[test]
    fn portrait_tiles_never_become_slivers() {
        let call = [WIDE, WIDE, 2.39, 0.5625, 4.0 / 3.0, WIDE, WIDE];
        for (w, h) in [(640.0, 640.0), (1080.0, 790.0), (664.0, 610.0)] {
            let p = grid(w, h, 12.0, &call);
            assert!(
                p.tiles.iter().all(|t| t.w >= 0.9 * MIN_TILE_WIDTH),
                "{w}x{h} {p:?}"
            );
            let mut twelve = call.to_vec();
            twelve.extend([WIDE, WIDE, WIDE, 0.5625, WIDE]);
            let p = grid(w, h, 12.0, &twelve);
            assert!(
                p.tiles.iter().all(|t| t.w >= 0.9 * MIN_TILE_WIDTH),
                "{w}x{h} {p:?}"
            );
        }
        // Focus strips are thumbnails: the stage's size comes first, but a
        // portrait thumbnail still never drops to a sliver.
        for f in 0..call.len() {
            let p = focus(1080.0, 790.0, 12.0, &call, f);
            assert!(
                p.tiles.iter().all(|t| t.w >= 0.55 * MIN_TILE_WIDTH),
                "focus {f} {p:?}"
            );
        }
    }

    #[test]
    fn rows_stay_within_twice_each_other_when_the_floor_allows() {
        let mut twelve = vec![WIDE, WIDE, 2.39, 0.5625, 4.0 / 3.0, WIDE, WIDE];
        twelve.extend([WIDE, WIDE, WIDE, WIDE, WIDE]);
        let p = grid(640.0, 620.0, 12.0, &twelve);
        assert!(p.tiles.iter().all(|t| t.w >= 0.9 * MIN_TILE_WIDTH));
        let (low, high) = p.tiles.iter().fold((f64::INFINITY, 0.0f64), |(l, h), t| {
            (l.min(t.h), h.max(t.h))
        });
        assert!(
            high <= 2.0 * low + 1.0 || high <= MIN_TILE_WIDTH / 0.5625 + 1.0,
            "{low} {high} {p:?}"
        );
    }

    #[test]
    fn quality_chip_threshold_stays_below_the_layout_floor() {
        // The stylesheet hides the quality chip below this width; it must not
        // hide it on tiles the layout considers wide enough.
        let css = include_str!("../style.css");
        let rule = css
            .find(".media-quality { display: none; }")
            .expect("quality hide rule");
        let start = css[..rule]
            .rfind("@container (max-width: ")
            .expect("container query")
            + 23;
        let width: f64 = css[start..].split("px").next().unwrap().parse().unwrap();
        assert!(width < MIN_TILE_WIDTH, "{width}");
        // Short but wide strip thumbnails (an ultrawide at ~128x50) keep it too.
        let rule_text = &css[start..rule];
        if let Some(at) = rule_text.find("max-height: ") {
            let height: f64 = rule_text[at + 12..]
                .split("px")
                .next()
                .unwrap()
                .parse()
                .unwrap();
            assert!(height <= 40.0, "{height}");
        }
    }

    #[test]
    fn a_few_pixels_under_the_floor_never_beat_even_rows() {
        // The 7-stream call at 1024 (640x630): a balanced split leaves the
        // portrait just under 96px; it must still win over rows ~3x apart.
        let call = [WIDE, WIDE, 0.5625, WIDE, 2.39, 4.0 / 3.0, WIDE];
        // Laptop-sized areas never fall back to scrolling for this call.
        for h in [560.0, 590.0, 610.0, 630.0, 660.0] {
            for w in [620.0, 640.0, 664.0] {
                assert!(!grid(w, h, 12.0, &call).scrolls, "{w}x{h}");
            }
        }
        let p = grid(640.0, 630.0, 12.0, &call);
        let (low, high) = p.tiles.iter().fold((f64::INFINITY, 0.0f64), |(l, h), t| {
            (l.min(t.h), h.max(t.h))
        });
        assert!(high <= 2.2 * low, "{low} {high} {p:?}");
        assert!(p.tiles.iter().all(|t| t.w >= 0.9 * MIN_TILE_WIDTH), "{p:?}");
    }

    #[test]
    fn focus_keeps_most_of_the_area_for_the_stage() {
        let call = [WIDE, WIDE, 2.39, 0.5625, 4.0 / 3.0, WIDE, WIDE];
        let p = focus(1056.0, 780.0, 12.0, &call, 0);
        assert!(
            p.tiles[0].area() >= 0.6 * 1056.0 * 780.0,
            "{:?}",
            p.tiles[0]
        );
    }

    #[test]
    fn focus_strip_thumbnails_keep_their_own_floor() {
        let call = [WIDE, WIDE, 2.39, 0.5625, 4.0 / 3.0, WIDE, WIDE];
        for (w, h) in [
            (640.0, 610.0),
            (664.0, 620.0),
            (1056.0, 780.0),
            (1080.0, 790.0),
        ] {
            for f in 0..call.len() {
                let p = focus(w, h, 12.0, &call, f);
                assert!(
                    p.tiles.iter().all(|t| t.w >= 0.9 * MIN_THUMB_WIDTH),
                    "{w}x{h} focus {f} {p:?}"
                );
            }
        }
    }

    #[test]
    fn a_portrait_joining_a_fixed_order_keeps_rows_within_twice() {
        // Seven settled streams (the user's order stays), then a portrait joins.
        let settled = [WIDE, 2.39, 4.0 / 3.0, WIDE, WIDE, WIDE, WIDE];
        let mut aspects = settled.to_vec();
        aspects.push(0.5625);
        let at = insertion(640.0, 620.0, 12.0, &aspects, 7);
        let mut ordered = settled.to_vec();
        ordered.insert(at, 0.5625);
        let p = grid(640.0, 620.0, 12.0, &ordered);
        assert!(!p.scrolls);
        let (low, high) = p.tiles.iter().fold((f64::INFINITY, 0.0f64), |(l, h), t| {
            (l.min(t.h), h.max(t.h))
        });
        assert!(high <= 2.0 * low + 1.0, "{low} {high} at {at}: {p:?}");
    }

    #[test]
    fn a_focus_stage_never_pays_a_lot_for_a_few_thumbnail_pixels() {
        let call = [WIDE, WIDE, 2.39, 0.5625, 4.0 / 3.0, WIDE, WIDE];
        let p = focus(1056.0, 780.0, 12.0, &call, 0);
        assert!(
            p.tiles[0].area() >= 0.7 * 1056.0 * 780.0,
            "{:?}",
            p.tiles[0]
        );
        assert!(
            p.tiles.iter().all(|t| t.w >= 0.9 * MIN_THUMB_WIDTH),
            "{p:?}"
        );
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
