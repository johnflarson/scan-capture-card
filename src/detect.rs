//! Find cards on the table without a vision LLM.
//!
//! A card's black border makes a long straight edge against the table on
//! every side, dark on the card's side. The detector:
//! 1. finds straight edges (a simplified LSD line-segment detector: pixels
//!    with a strong brightness gradient, grown into regions of one gradient
//!    direction, each fitted with a segment) and remembers which side of each
//!    is dark;
//! 2. pairs edges that face each other (parallel, dark sides towards each
//!    other, side by side) a card's width or height apart, at any rotation,
//!    and places a 63:88 rectangle on them;
//! 3. checks the rectangle: along at least three sides the border just
//!    inside must be clearly darker than the table just outside (glare can
//!    wash out the fourth);
//! 4. keeps the best rectangle where several overlap.
//!
//! On a dark mat the border merges into the mat. What shows instead is the
//! border's inside edge: a light rectangle (the card's frame) surrounded by
//! dark. A second pass looks for exactly that, the same way with light and
//! dark swapped and the frame's proportions, and grows each frame it finds by
//! the border to the whole card. Both passes' rectangles compete in step 4.
//!
//! Brightness is the brightest channel, so a red or blue mat counts as light
//! and only a black border (or a dark line) as dark. Contrast is relative, so
//! a border greyed by glare still counts. A playmat's line art is curved or
//! thin, so it rarely gives two long facing edges with a border-wide dark band
//! along three sides of a card-shaped rectangle.

use std::ops::{Add, Mul, Sub};

use image::{Rgb, RgbImage, imageops};

use crate::llm::NormBox;

/// The photo is analysed at this width (a card is then ~40-300 px wide).
const WORK_WIDTH: u32 = 1280;
const CARD_ASPECT: f32 = 63.0 / 88.0;
/// Lengths tried for the sides along a pair of edges, relative to 63:88,
/// nearest first.
const STRETCH: [f32; 9] = [1.0, 0.98, 1.02, 0.96, 1.04, 0.94, 1.06, 0.92, 1.08];
/// Card widths accepted, as a share of the frame's short side.
const MIN_WIDTH: f32 = 0.05;
const MAX_WIDTH: f32 = 0.45;
/// The black border's thickness as a share of the card's width.
const BORDER: f32 = 0.045;
/// The frame inside the border: its width as a share of the card's, and its
/// width to height.
const FRAME_WIDTH: f32 = 1.0 - 2.0 * BORDER;
const FRAME_ASPECT: f32 = FRAME_WIDTH / (1.0 / CARD_ASPECT - 2.0 * BORDER);
/// Gradient (brightness levels per pixel) for a pixel to be on an edge.
const MIN_GRADIENT: f32 = 6.0;
/// A pixel joins an edge while its gradient points within this of the edge's.
const ANGLE_TOLERANCE: f32 = 22.5 * std::f32::consts::PI / 180.0;
/// Shortest edge kept, in work pixels.
const MIN_EDGE: f32 = 12.0;
/// Pieces of one edge are joined when parallel within this, this many work
/// pixels off each other's line, and at most this far apart along it.
const JOIN_ANGLE: f32 = 4.0 * std::f32::consts::PI / 180.0;
const JOIN_OFFSET: f32 = 2.0;
const JOIN_GAP: f32 = 12.0;
/// Cards turned further than this from upright or sideways aren't looked for.
const MAX_TILT: f32 = 25.0 * std::f32::consts::PI / 180.0;
/// Opposite sides of a card are parallel within this.
const PARALLEL: f32 = 6.0 * std::f32::consts::PI / 180.0;
/// A point on a side shows a border when the table just outside is brighter
/// than the border just inside by this many levels and this share.
const MIN_CONTRAST: f32 = 18.0;
const RELATIVE_CONTRAST: f32 = 0.2;
/// The edge must lie within this many work pixels of a side…
const EDGE_REACH: f32 = 2.0;
/// …and be at least this sharp: its gradient a share of the contrast
/// across it.
const EDGE_SHARPNESS: f32 = 0.25;
/// Points checked along each side (its middle 80%).
const SAMPLES: usize = 20;
/// Share of a side's points that must show a border, on three sides.
const SIDE_HIT: f32 = 0.6;
/// The inside is checked on a grid this many points across and down…
const INSIDE_GRID: usize = 8;
/// …where at least this share of points must be light: at least this share
/// of the table's brightness.
const MIN_LIGHT_INSIDE: f32 = 0.15;
const LIGHT_INSIDE: f32 = 0.7;
/// A rectangle inside one at least this many times its area is part of that
/// card (its art window), not a card, when the larger one's score (sides'
/// border shares added up, at most 4) is at most this much lower.
const NESTED: f32 = 1.4;
const NESTED_SCORE: f32 = 0.3;
/// Cards in one photo are this close to the typical card's width.
const SAME_SIZE: f32 = 0.12;

/// Card boxes in `frame`, normalised 0..1, in reading order (rows, then left
/// to right). A tilted card's box is the upright box around it.
pub fn find_cards(frame: &RgbImage) -> Vec<NormBox> {
    analyse(frame).boxes
}

/// What [`find_cards`] saw, for `mtg-scan detect-dump`.
pub struct Analysis {
    pub boxes: Vec<NormBox>,
    /// The photo at working size with the straight edges (yellow) and the
    /// cards (green) drawn on it.
    pub debug: RgbImage,
}

pub fn analyse(frame: &RgbImage) -> Analysis {
    let (w0, h0) = frame.dimensions();
    if w0 < 16 || h0 < 16 {
        return Analysis {
            boxes: Vec::new(),
            debug: RgbImage::new(1, 1),
        };
    }
    let scale = (WORK_WIDTH as f32 / w0 as f32).min(1.0);
    let (w, h) = (
        ((w0 as f32 * scale).round() as u32).max(1),
        ((h0 as f32 * scale).round() as u32).max(1),
    );
    let small = imageops::resize(frame, w, h, imageops::FilterType::Triangle);
    let scene = Scene::new(&small);
    let edges = joined(edges(&scene));
    let mut found = cards(&scene, &edges, Look::Border);
    found.extend(
        cards(&scene, &edges, Look::Frame)
            .into_iter()
            .map(Card::around_frame),
    );
    let cards = keep_best(found);
    let boxes = cards.iter().map(|c| c.bounds(w, h)).collect();
    let debug = draw_debug(small, &edges, &cards);
    Analysis { boxes, debug }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct V {
    x: f32,
    y: f32,
}

impl V {
    fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
    fn dot(self, o: V) -> f32 {
        self.x * o.x + self.y * o.y
    }
    fn unit(self) -> V {
        let len = self.dot(self).sqrt().max(f32::EPSILON);
        V::new(self.x / len, self.y / len)
    }
    /// Turned a quarter: (x, y) -> (-y, x).
    fn perp(self) -> V {
        V::new(-self.y, self.x)
    }
}

impl Add for V {
    type Output = V;
    fn add(self, o: V) -> V {
        V::new(self.x + o.x, self.y + o.y)
    }
}

impl Sub for V {
    type Output = V;
    fn sub(self, o: V) -> V {
        V::new(self.x - o.x, self.y - o.y)
    }
}

impl Mul<f32> for V {
    type Output = V;
    fn mul(self, k: f32) -> V {
        V::new(self.x * k, self.y * k)
    }
}

/// A one-channel float image.
struct Plane {
    w: usize,
    h: usize,
    px: Vec<f32>,
}

impl Plane {
    /// Brightest channel, lightly blurred (1-2-1 in both directions).
    fn brightness(img: &RgbImage) -> Self {
        let (w, h) = (img.width() as usize, img.height() as usize);
        let raw: Vec<f32> = img
            .pixels()
            .map(|p| p.0[0].max(p.0[1]).max(p.0[2]) as f32)
            .collect();
        let blur = |src: &[f32], dx: usize, dy: usize| -> Vec<f32> {
            let mut out = src.to_vec();
            for y in dy..h - dy {
                for x in dx..w - dx {
                    let i = y * w + x;
                    let step = dy * w + dx;
                    out[i] = (src[i - step] + 2.0 * src[i] + src[i + step]) / 4.0;
                }
            }
            out
        };
        let px = blur(&blur(&raw, 1, 0), 0, 1);
        Self { w, h, px }
    }

    fn at(&self, x: usize, y: usize) -> f32 {
        self.px[y * self.w + x]
    }

    /// Bilinear sample, or `None` outside the image.
    fn sample(&self, p: V) -> Option<f32> {
        if p.x < 0.0 || p.y < 0.0 {
            return None;
        }
        let (x0, y0) = (p.x.floor() as usize, p.y.floor() as usize);
        if x0 + 1 >= self.w || y0 + 1 >= self.h {
            return None;
        }
        let (fx, fy) = (p.x - x0 as f32, p.y - y0 as f32);
        let top = self.at(x0, y0) * (1.0 - fx) + self.at(x0 + 1, y0) * fx;
        let bottom = self.at(x0, y0 + 1) * (1.0 - fx) + self.at(x0 + 1, y0 + 1) * fx;
        Some(top * (1.0 - fy) + bottom * fy)
    }
}

/// The photo's brightness and its gradient (Sobel, in levels per pixel,
/// pointing to the lighter side).
struct Scene {
    light: Plane,
    gx: Plane,
    gy: Plane,
}

impl Scene {
    fn new(img: &RgbImage) -> Self {
        let light = Plane::brightness(img);
        let (w, h) = (light.w, light.h);
        let mut gx = vec![0f32; w * h];
        let mut gy = vec![0f32; w * h];
        for y in 1..h - 1 {
            for x in 1..w - 1 {
                let p = |dx: isize, dy: isize| {
                    light.at((x as isize + dx) as usize, (y as isize + dy) as usize)
                };
                let i = y * w + x;
                gx[i] =
                    (p(1, -1) + 2.0 * p(1, 0) + p(1, 1) - p(-1, -1) - 2.0 * p(-1, 0) - p(-1, 1))
                        / 8.0;
                gy[i] =
                    (p(-1, 1) + 2.0 * p(0, 1) + p(1, 1) - p(-1, -1) - 2.0 * p(0, -1) - p(1, -1))
                        / 8.0;
            }
        }
        Self {
            light,
            gx: Plane { w, h, px: gx },
            gy: Plane { w, h, px: gy },
        }
    }

    /// The strongest gradient towards `out` within `reach` pixels of `p`
    /// along it: how sharply it gets lighter going out across an edge there.
    fn edge_towards(&self, p: V, out: V, reach: f32) -> f32 {
        let mut best = f32::MIN;
        let mut d = -reach;
        while d <= reach {
            let q = p + out * d;
            if let (Some(x), Some(y)) = (self.gx.sample(q), self.gy.sample(q)) {
                best = best.max(x * out.x + y * out.y);
            }
            d += 0.5;
        }
        best
    }
}

/// A straight edge: its middle, direction, half length, and the unit normal
/// pointing to its light side.
#[derive(Debug, Clone, Copy)]
struct Edge {
    c: V,
    d: V,
    n: V,
    half: f32,
}

/// Difference between two angles, 0..=π.
fn angle_diff(a: f32, b: f32) -> f32 {
    let d = (a - b).rem_euclid(std::f32::consts::TAU);
    d.min(std::f32::consts::TAU - d)
}

/// Straight edges: strong-gradient pixels grown into regions whose gradient
/// points the same way, each kept when it fits a line closely.
fn edges(scene: &Scene) -> Vec<Edge> {
    let (w, h) = (scene.light.w, scene.light.h);
    let (gx, gy) = (&scene.gx.px, &scene.gy.px);
    let mag: Vec<f32> = gx.iter().zip(gy).map(|(x, y)| x.hypot(*y)).collect();
    let ang: Vec<f32> = gx.iter().zip(gy).map(|(x, y)| y.atan2(*x)).collect();
    // Strongest pixels seed regions first.
    let mut order: Vec<u32> = (0..w * h)
        .filter(|&i| mag[i] >= MIN_GRADIENT)
        .map(|i| i as u32)
        .collect();
    order.sort_unstable_by(|&a, &b| mag[b as usize].total_cmp(&mag[a as usize]));

    let mut used = vec![false; w * h];
    let mut region: Vec<usize> = Vec::new();
    let mut found = Vec::new();
    for &seed in &order {
        let seed = seed as usize;
        if used[seed] {
            continue;
        }
        used[seed] = true;
        region.clear();
        region.push(seed);
        let (mut sx, mut sy) = (ang[seed].cos(), ang[seed].sin());
        let mut theta = ang[seed];
        let mut next = 0;
        while next < region.len() {
            let p = region[next];
            next += 1;
            let (px, py) = (p % w, p / w);
            for ny in py - 1..=py + 1 {
                for nx in px - 1..=px + 1 {
                    if nx < 1 || ny < 1 || nx >= w - 1 || ny >= h - 1 {
                        continue;
                    }
                    let q = ny * w + nx;
                    if used[q]
                        || mag[q] < MIN_GRADIENT
                        || angle_diff(ang[q], theta) > ANGLE_TOLERANCE
                    {
                        continue;
                    }
                    used[q] = true;
                    region.push(q);
                    sx += ang[q].cos();
                    sy += ang[q].sin();
                    theta = sy.atan2(sx);
                }
            }
        }
        if region.len() as f32 >= MIN_EDGE
            && let Some(e) = fit(&region, &mag, w, theta)
        {
            found.push(e);
        }
    }
    found
}

/// The edges plus, where the table's artwork or glare breaks a card's edge
/// into pieces, the pieces joined up. The pieces stay too: two cards side by
/// side have edges in line, and joining those would make an edge longer than
/// either card.
fn joined(mut edges: Vec<Edge>) -> Vec<Edge> {
    edges.sort_by(|a, b| b.half.total_cmp(&a.half));
    let parallel = JOIN_ANGLE.cos();
    let mut merged = Vec::new();
    let mut absorbed = vec![false; edges.len()];
    for i in 0..edges.len() {
        if absorbed[i] {
            continue;
        }
        let e = edges[i];
        // The joined edge's extent along `e`, relative to its middle.
        let (mut lo, mut hi) = (-e.half, e.half);
        let mut grew = true;
        let mut any = false;
        while grew {
            grew = false;
            for j in i + 1..edges.len() {
                let f = edges[j];
                if absorbed[j] || e.n.dot(f.n) < parallel {
                    continue;
                }
                let off = f.c - e.c;
                if off.dot(e.n).abs() > JOIN_OFFSET {
                    continue;
                }
                let at = off.dot(e.d);
                let (f0, f1) = (at - f.half, at + f.half);
                let gap = (f0 - hi).max(lo - f1);
                if gap <= JOIN_GAP {
                    lo = lo.min(f0);
                    hi = hi.max(f1);
                    absorbed[j] = true;
                    grew = true;
                    any = true;
                }
            }
        }
        if any {
            merged.push(Edge {
                c: e.c + e.d * ((lo + hi) / 2.0),
                d: e.d,
                n: e.n,
                half: (hi - lo) / 2.0,
            });
        }
    }
    edges.extend(merged);
    edges
}

/// The segment through a region whose gradient points along `theta`, if the
/// region is long and straight.
fn fit(region: &[usize], mag: &[f32], w: usize, theta: f32) -> Option<Edge> {
    let n = V::new(theta.cos(), theta.sin());
    let d = n.perp();
    let pos = |p: usize| V::new((p % w) as f32, (p / w) as f32);
    let total: f32 = region.iter().map(|&p| mag[p]).sum();
    let c = region
        .iter()
        .fold(V::new(0.0, 0.0), |acc, &p| acc + pos(p) * mag[p])
        * (1.0 / total);
    let (mut t0, mut t1, mut spread) = (f32::MAX, f32::MIN, 0.0);
    for &p in region {
        let q = pos(p) - c;
        let t = q.dot(d);
        t0 = t0.min(t);
        t1 = t1.max(t);
        spread += mag[p] * q.dot(n).powi(2);
    }
    let len = t1 - t0;
    // A straight edge is a band ~2 px across; a curve spreads wider.
    if len < MIN_EDGE || (spread / total).sqrt() > 1.0 + 0.01 * len {
        return None;
    }
    Some(Edge {
        c: c + d * ((t0 + t1) / 2.0),
        d,
        n,
        half: len / 2.0,
    })
}

/// What marks a card's outline.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Look {
    /// Its black border, darker than the table around it.
    Border,
    /// On a dark mat: its frame, lighter than the border (and mat) around it.
    Frame,
}

impl Look {
    /// Width to height of the rectangle looked for.
    fn aspect(self) -> f32 {
        match self {
            Look::Border => CARD_ASPECT,
            Look::Frame => FRAME_ASPECT,
        }
    }

    /// +1 when the outline is darker inside (the border), -1 when lighter.
    fn sign(self) -> f32 {
        match self {
            Look::Border => 1.0,
            Look::Frame => -1.0,
        }
    }
}

/// A card-shaped rectangle: centre, unit axes along its width and height,
/// size, and how well each side (top, bottom, left, right) shows a border.
#[derive(Debug, Clone, Copy)]
struct Card {
    c: V,
    ex: V,
    ey: V,
    w: f32,
    h: f32,
    sides: [f32; 4],
    score: f32,
}

impl Card {
    fn corners(&self) -> [V; 4] {
        let (x, y) = (self.ex * (self.w / 2.0), self.ey * (self.h / 2.0));
        [
            self.c - x - y,
            self.c + x - y,
            self.c + x + y,
            self.c - x + y,
        ]
    }

    /// Upright bounding box, in `[x0, y0, x1, y1]` work pixels.
    fn aabb(&self) -> [f32; 4] {
        self.corners().iter().fold(
            [f32::MAX, f32::MAX, f32::MIN, f32::MIN],
            |[x0, y0, x1, y1], p| [x0.min(p.x), y0.min(p.y), x1.max(p.x), y1.max(p.y)],
        )
    }

    fn bounds(&self, w: u32, h: u32) -> NormBox {
        let [x0, y0, x1, y1] = self.aabb();
        NormBox {
            x1: (x0 / w as f32).clamp(0.0, 1.0),
            y1: (y0 / h as f32).clamp(0.0, 1.0),
            x2: (x1 / w as f32).clamp(0.0, 1.0),
            y2: (y1 / h as f32).clamp(0.0, 1.0),
        }
    }

    /// The whole card around a frame found on a dark mat: the border added
    /// on every side.
    fn around_frame(self) -> Card {
        let w = self.w / FRAME_WIDTH;
        Card {
            w,
            h: self.h + 2.0 * BORDER * w,
            ..self
        }
    }

    /// Overlap of the upright boxes, as a share of the smaller one.
    fn overlap(&self, o: &Card) -> f32 {
        let ([a0, a1, a2, a3], [b0, b1, b2, b3]) = (self.aabb(), o.aabb());
        let ix = (a2.min(b2) - a0.max(b0)).max(0.0);
        let iy = (a3.min(b3) - a1.max(b1)).max(0.0);
        let area = ((a2 - a0) * (a3 - a1)).min((b2 - b0) * (b3 - b1));
        ix * iy / area.max(f32::EPSILON)
    }
}

/// Rectangles on pairs of facing edges that pass [`check`]: card-sized for
/// `Look::Border`, frame-sized for `Look::Frame`.
fn cards(scene: &Scene, edges: &[Edge], look: Look) -> Vec<Card> {
    let short = scene.light.w.min(scene.light.h) as f32;
    let scale = if look == Look::Frame {
        FRAME_WIDTH
    } else {
        1.0
    };
    let (min_w, max_w) = (short * MIN_WIDTH * scale, short * MAX_WIDTH * scale);
    let aspect = look.aspect();
    let max_h = max_w / aspect;
    let parallel = PARALLEL.cos();
    let mut found = Vec::new();
    for (i, a) in edges.iter().enumerate() {
        for b in &edges[i + 1..] {
            // Antiparallel normals: the light sides face away from each other
            // (a border), or towards each other (a frame).
            if a.n.dot(b.n) > -parallel {
                continue;
            }
            // `u` runs across the pair from b to a; the band between them
            // (dark for a border, light for a frame) means a lies further
            // along it.
            let u = ((a.n - b.n) * look.sign()).unit();
            // Cards laid out for a scan sit near upright or sideways; steep
            // angles are mostly the table's artwork.
            if u.x.abs().min(u.y.abs()) > MAX_TILT.sin() {
                continue;
            }
            let gap = (a.c - b.c).dot(u);
            if gap < min_w || gap > max_h {
                continue;
            }
            let v = u.perp();
            let (a0, a1) = (a.c.dot(v) - a.half, a.c.dot(v) + a.half);
            let (b0, b1) = (b.c.dot(v) - b.half, b.c.dot(v) + b.half);
            let shorter = a.half.min(b.half) * 2.0;
            if a1.min(b1) - a0.max(b0) < 0.5 * shorter {
                continue;
            }
            let (lo, hi) = (a0.min(b0), a1.max(b1));
            let mid_u = (a.c.dot(u) + b.c.dot(u)) / 2.0;
            // The pair is either the card's left and right sides (gap = its
            // width) or its top and bottom (gap = its height).
            for wide in [true, false] {
                let (ex, ey) = if wide { (u, v) } else { (v, u) };
                let mut best: Option<Card> = None;
                // A camera at a slight angle squashes the card a little, so
                // the paired sides' length may differ from 63:88; nearest
                // to it wins a tie.
                for stretch in STRETCH {
                    let (cw, ch) = if wide {
                        (gap, gap / aspect * stretch)
                    } else {
                        (gap * aspect * stretch, gap)
                    };
                    if cw < min_w || cw > max_w {
                        continue;
                    }
                    // How long the paired sides are.
                    let side = if wide { ch } else { cw };
                    if hi - lo > side * 1.05 || shorter < 0.25 * side {
                        continue;
                    }
                    // Slide the card along the pair (it must cover both
                    // edges); keep the best place.
                    let (first, last) = (hi - side / 2.0, lo + side / 2.0);
                    let steps = ((last - first) / 2.0).ceil().max(0.0) as usize;
                    for k in 0..=steps {
                        let along = if steps == 0 {
                            (first + last) / 2.0
                        } else {
                            first + (last - first) * k as f32 / steps as f32
                        };
                        let card = Card {
                            c: u * mid_u + v * along,
                            ex,
                            ey,
                            w: cw,
                            h: ch,
                            sides: [0.0; 4],
                            score: 0.0,
                        };
                        if let Some(c) = check(scene, card, look)
                            && best.is_none_or(|b| c.score > b.score)
                        {
                            best = Some(c);
                        }
                    }
                }
                found.extend(best);
            }
        }
    }
    found
}

/// The card with its side scores, if at least three sides show its outline
/// (see [`Look`]).
fn check(scene: &Scene, mut card: Card, look: Look) -> Option<Card> {
    let card_w = if look == Look::Frame {
        card.w / FRAME_WIDTH
    } else {
        card.w
    };
    let border = (card_w * BORDER).max(1.5);
    // Outward normal, direction along the side, distance from the centre,
    // half length: top, bottom, left, right.
    let sides = [
        (card.ey * -1.0, card.ex, card.h / 2.0, card.w / 2.0),
        (card.ey, card.ex, card.h / 2.0, card.w / 2.0),
        (card.ex * -1.0, card.ey, card.w / 2.0, card.h / 2.0),
        (card.ex, card.ey, card.w / 2.0, card.h / 2.0),
    ];
    // The light side's brightness (the table, or the frame), for the inside
    // check below.
    let (mut table_sum, mut table_n) = (0.0, 0);
    let mut weak = 0;
    for (k, &(out, along, dist, half)) in sides.iter().enumerate() {
        let mut hits = 0;
        for i in 0..SAMPLES {
            let t = -0.8 + 1.6 * (i as f32 + 0.5) / SAMPLES as f32;
            let p = card.c + out * dist + along * (t * half);
            // Towards the light side: out of a border, into a frame. The
            // dark sample stays within the border's thickness.
            let to_light = out * look.sign();
            let (Some(table), Some(rim)) = (
                scene.light.sample(p + to_light * (0.7 * border)),
                scene.light.sample(p - to_light * (0.5 * border)),
            ) else {
                continue;
            };
            // The outline here: the light side clearly lighter than the dark
            // one, and the edge between them on the side, not just near it.
            if table - rim >= MIN_CONTRAST.max(RELATIVE_CONTRAST * table)
                && scene.edge_towards(p, to_light, EDGE_REACH)
                    >= MIN_GRADIENT.max(EDGE_SHARPNESS * (table - rim))
            {
                hits += 1;
                table_sum += table;
                table_n += 1;
            }
        }
        card.sides[k] = hits as f32 / SAMPLES as f32;
        if card.sides[k] < SIDE_HIT {
            weak += 1;
            // Glare can wash out one side, not two.
            if weak == 2 {
                return None;
            }
        }
    }
    // Every card has light areas inside (title bar, text box); a solid dark
    // object (a mouse, a phone) doesn't.
    let table = table_sum / table_n.max(1) as f32;
    let (mut light_inside, mut inside) = (0, 0);
    for gy in 0..INSIDE_GRID {
        for gx in 0..INSIDE_GRID {
            let fx = -0.7 + 1.4 * (gx as f32 + 0.5) / INSIDE_GRID as f32;
            let fy = -0.7 + 1.4 * (gy as f32 + 0.5) / INSIDE_GRID as f32;
            let p = card.c + card.ex * (fx * card.w / 2.0) + card.ey * (fy * card.h / 2.0);
            if let Some(v) = scene.light.sample(p) {
                inside += 1;
                light_inside += usize::from(v >= LIGHT_INSIDE * table);
            }
        }
    }
    if (light_inside as f32) < MIN_LIGHT_INSIDE * inside.max(1) as f32 {
        return None;
    }
    card.score = card.sides.iter().sum();
    Some(card)
}

/// The best card where several overlap, in reading order (rows half a card's
/// height apart, then left to right).
fn keep_best(found: Vec<Card>) -> Vec<Card> {
    // A rectangle inside a clearly larger one that shows its border about
    // as well is part of that card (its art window, or title and art).
    let area = |c: &Card| c.w * c.h;
    let mut found: Vec<Card> = found
        .iter()
        .filter(|c| {
            !found.iter().any(|o| {
                area(o) >= NESTED * area(c)
                    && c.overlap(o) >= 0.8
                    && o.score >= c.score - NESTED_SCORE
            })
        })
        .copied()
        .collect();
    found.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| (b.w * b.h).total_cmp(&(a.w * a.h)))
    });
    let separate = |found: &[Card]| {
        let mut kept: Vec<Card> = Vec::new();
        for c in found {
            if kept.iter().all(|k| k.overlap(c) < 0.3) {
                kept.push(*c);
            }
        }
        kept
    };
    // The camera is at one height, so every card on the table is about the
    // same size; much smaller or larger rectangles are the table's artwork
    // or a card plus part of its neighbour. Drop them before choosing
    // between overlapping rectangles, so they can't hide a card.
    if let Some(typical) = median(separate(&found).iter().map(|c| c.w).collect()) {
        found.retain(|c| (c.w / typical - 1.0).abs() <= SAME_SIZE);
    }
    let mut kept = separate(&found);
    let row = kept
        .iter()
        .map(|c| c.aabb()[3] - c.aabb()[1])
        .fold(1.0, f32::max)
        * 0.5;
    kept.sort_by(|a, b| {
        let (pa, pb) = (a.aabb(), b.aabb());
        ((pa[1] / row) as u32)
            .cmp(&((pb[1] / row) as u32))
            .then(pa[0].total_cmp(&pb[0]))
    });
    kept
}

fn median(mut values: Vec<f32>) -> Option<f32> {
    values.sort_by(f32::total_cmp);
    values.get(values.len() / 2).copied()
}

fn draw_debug(mut img: RgbImage, edges: &[Edge], cards: &[Card]) -> RgbImage {
    for e in edges {
        line(
            &mut img,
            e.c - e.d * e.half,
            e.c + e.d * e.half,
            [250, 220, 40],
        );
    }
    for c in cards {
        let p = c.corners();
        for i in 0..4 {
            line(&mut img, p[i], p[(i + 1) % 4], [40, 220, 120]);
        }
    }
    img
}

fn line(img: &mut RgbImage, from: V, to: V, color: [u8; 3]) {
    let steps = (to - from).dot(to - from).sqrt().ceil().max(1.0) as usize;
    for i in 0..=steps {
        let p = from + (to - from) * (i as f32 / steps as f32);
        let (x, y) = (p.x.round(), p.y.round());
        if x >= 0.0 && y >= 0.0 && (x as u32) < img.width() && (y as u32) < img.height() {
            img.put_pixel(x as u32, y as u32, Rgb(color));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TABLE: Rgb<u8> = Rgb([230, 225, 215]);

    /// A card centred at (cx, cy), `cw` x `ch` px, turned `deg` degrees:
    /// a black border (`rim` colour) around a coloured inside.
    fn draw_card(img: &mut RgbImage, cx: f32, cy: f32, cw: f32, ch: f32, deg: f32, rim: Rgb<u8>) {
        let (s, c) = deg.to_radians().sin_cos();
        let border = cw.min(ch) * 0.05;
        for y in 0..img.height() {
            for x in 0..img.width() {
                let (dx, dy) = (x as f32 - cx, y as f32 - cy);
                let (lx, ly) = (dx * c + dy * s, -dx * s + dy * c);
                let (ax, ay) = (lx.abs(), ly.abs());
                if ax > cw / 2.0 || ay > ch / 2.0 {
                    continue;
                }
                let edge = ax > cw / 2.0 - border || ay > ch / 2.0 - border;
                img.put_pixel(x, y, if edge { rim } else { Rgb([60, 110, 190]) });
            }
        }
    }

    /// A light table with line art (2 px lines, a wavy one too) and a solid
    /// dark "mouse".
    fn table() -> RgbImage {
        let mut img = RgbImage::from_pixel(1280, 720, TABLE);
        let ink = Rgb([20, 20, 20]);
        for x in 0..1280u32 {
            for t in 0..2 {
                img.put_pixel(x, 300 + t, ink);
                img.put_pixel(x, (x / 3 + 50 + t).min(719), ink);
                let wave = 620.0 + 30.0 * (x as f32 / 40.0).sin();
                img.put_pixel(x, wave as u32 + t, ink);
            }
        }
        for y in 500..600 {
            for x in 1100..1200 {
                let (dx, dy) = (x as f32 - 1150.0, y as f32 - 550.0);
                if dx * dx / 2500.0 + dy * dy / 2500.0 <= 1.0 {
                    img.put_pixel(x, y, Rgb([15, 15, 15]));
                }
            }
        }
        img
    }

    fn assert_near(b: &NormBox, (x0, y0, x1, y1): (f32, f32, f32, f32)) {
        let tol = 6.0;
        let got = (b.x1 * 1280.0, b.y1 * 720.0, b.x2 * 1280.0, b.y2 * 720.0);
        assert!(
            (got.0 - x0).abs() <= tol
                && (got.1 - y0).abs() <= tol
                && (got.2 - x1).abs() <= tol
                && (got.3 - y1).abs() <= tol,
            "got {got:?}, expected {:?}",
            (x0, y0, x1, y1)
        );
    }

    #[test]
    fn finds_bordered_cards_and_ignores_lines_and_blobs() {
        let mut img = table();
        let black = Rgb([10, 10, 10]);
        draw_card(&mut img, 163.0, 188.0, 126.0, 176.0, 0.0, black);
        draw_card(&mut img, 363.0, 208.0, 126.0, 176.0, 0.0, black);
        draw_card(&mut img, 563.0, 468.0, 126.0, 176.0, 0.0, black);
        // Sideways.
        draw_card(&mut img, 838.0, 463.0, 176.0, 126.0, 0.0, black);
        let found = find_cards(&img);
        assert_eq!(found.len(), 4, "{found:?}");
        assert_near(&found[0], (100.0, 100.0, 226.0, 276.0));
        assert_near(&found[1], (300.0, 120.0, 426.0, 296.0));
        assert_near(&found[2], (500.0, 380.0, 626.0, 556.0));
        assert_near(&found[3], (750.0, 400.0, 926.0, 526.0));
    }

    #[test]
    fn finds_tilted_and_glare_cards() {
        let mut img = table();
        draw_card(&mut img, 300.0, 200.0, 126.0, 176.0, 9.0, Rgb([10, 10, 10]));
        // Under glare the border is grey, not black.
        draw_card(
            &mut img,
            700.0,
            450.0,
            126.0,
            176.0,
            -4.0,
            Rgb([165, 165, 170]),
        );
        let found = find_cards(&img);
        assert_eq!(found.len(), 2, "{found:?}");
        // The upright box around a card turned 9°.
        let (s, c) = 9f32.to_radians().sin_cos();
        let (hw, hh) = ((126.0 * c + 176.0 * s) / 2.0, (126.0 * s + 176.0 * c) / 2.0);
        assert_near(&found[0], (300.0 - hw, 200.0 - hh, 300.0 + hw, 200.0 + hh));
    }

    #[test]
    fn empty_table_has_no_cards() {
        assert!(find_cards(&table()).is_empty());
    }

    #[test]
    fn finds_cards_on_a_dark_mat() {
        // The border is as dark as the mat, so only the light frame inside
        // it shows; the boxes are still the whole cards.
        let mat = Rgb([22, 22, 24]);
        let mut img = RgbImage::from_pixel(1280, 720, mat);
        draw_card(&mut img, 300.0, 250.0, 126.0, 176.0, 0.0, Rgb([16, 16, 16]));
        draw_card(&mut img, 600.0, 300.0, 126.0, 176.0, 8.0, Rgb([16, 16, 16]));
        draw_card(&mut img, 900.0, 260.0, 126.0, 176.0, 0.0, Rgb([16, 16, 16]));
        let found = find_cards(&img);
        assert_eq!(found.len(), 3, "{found:?}");
        assert_near(&found[0], (237.0, 162.0, 363.0, 338.0));
        // Reading order: the tilted card sits lower, so it comes last.
        assert_near(&found[1], (837.0, 172.0, 963.0, 348.0));
        assert!(find_cards(&RgbImage::from_pixel(1280, 720, mat)).is_empty());
    }
}
