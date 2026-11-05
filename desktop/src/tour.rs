//! A camera tour over a board for benchmarks and demo videos: from the
//! whole board down to a note in one project, across the world to a note in
//! another, and back out. Each leg follows van Wijk & Nuij's smooth zoom and
//! pan (zoom out just enough to see both ends, travel, zoom in), so a jump
//! between notes millions of units apart reads as one move.

use hs_core::board::{Bbox, Board};
use std::time::Instant;

/// Centre and visible width in world units.
#[derive(Clone, Copy)]
struct View {
    x: f64,
    y: f64,
    w: f64,
}

struct Leg {
    a: View,
    b: View,
    secs: f64,
}

pub struct Tour {
    legs: Vec<Leg>,
    start: Instant,
}

const RHO: f64 = 1.4;
const TOTAL_SECS: f64 = 14.0;

impl Tour {
    /// Plans a tour of `stops` notes on a `w` x `h` screen.
    pub fn plan(board: &Board, w: u32, h: u32, stops: usize) -> Tour {
        let aspect = w as f64 / h as f64;
        let view = |b: Bbox, fill: f64| View {
            x: (b[0] + b[2]) as f64 * 0.5,
            y: (b[1] + b[3]) as f64 * 0.5,
            w: ((b[2] - b[0]) as f64).max((b[3] - b[1]) as f64 * aspect) / fill,
        };
        let whole = view(board.bounds(), 0.95);
        // Deepest notes with a filled box, spread over the board's top frames.
        let depth_of = |id: &str| board.depth(id);
        let max_depth = board.nodes.iter().map(|n| depth_of(&n.id)).max().unwrap_or(0);
        let mut picks: Vec<&str> = Vec::new();
        let roots = board.roots();
        let mut seed = 0x2545_F491_4F6C_DD1Du64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let notes: Vec<&str> = board
            .nodes
            .iter()
            .filter(|n| !board.has_children(&n.id) && n.fill.is_some() && depth_of(&n.id) + 1 >= max_depth && n.text.len() > 18)
            .map(|n| n.id.as_str())
            .collect();
        for k in 0..stops {
            if notes.is_empty() {
                break;
            }
            // Prefer a note under another top frame than the last one.
            let want = roots.get(k % roots.len().max(1)).cloned();
            let mut pick = notes[(next() % notes.len() as u64) as usize];
            for _ in 0..200 {
                let c = notes[(next() % notes.len() as u64) as usize];
                if want.as_deref().is_some_and(|r| board.is_ancestor(r, c)) {
                    pick = c;
                    break;
                }
            }
            picks.push(pick);
        }
        let mut views = vec![whole];
        for (k, id) in picks.iter().enumerate() {
            let anc = board.ancestors(id);
            // On the first stop the tour goes down level by level.
            if k == 0 {
                for a in anc.iter().rev().skip(1) {
                    views.push(view(board.node(a).bbox(), 0.9));
                }
            } else if let Some(p) = anc.first() {
                views.push(view(board.node(p).bbox(), 0.9));
            }
            views.push(view(board.node(id).bbox(), 0.3));
        }
        views.push(whole);
        let mut legs = Vec::new();
        for pair in views.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            let s = path_len(a, b);
            legs.push(Leg { a, b, secs: (s * 0.85).max(1.6) });
            // A pause on each note.
            if b.w < whole.w * 1e-3 {
                legs.push(Leg { a: b, b, secs: 1.4 });
            }
        }
        // The whole flight takes TOTAL_SECS: a quick look, not a slow drift.
        let sum: f64 = legs.iter().map(|l| l.secs).sum();
        if sum > 0.0 {
            for l in &mut legs {
                l.secs *= TOTAL_SECS / sum;
            }
        }
        Tour { legs, start: hs_core::clock::now() }
    }

    /// The camera (centre, scale) now; None when the tour is over.
    pub fn camera(&self, screen_w: u32) -> Option<(f64, f64, f64)> {
        let mut t = hs_core::clock::since(self.start).as_secs_f64();
        for l in &self.legs {
            if t <= l.secs {
                let k = t / l.secs;
                let e = k * k * (3.0 - 2.0 * k);
                let v = along(l.a, l.b, e);
                return Some((v.x, v.y, screen_w as f64 / v.w));
            }
            t -= l.secs;
        }
        None
    }
}

/// van Wijk & Nuij: length of the optimal path between two views.
fn path_len(a: View, b: View) -> f64 {
    let u = ((b.x - a.x).powi(2) + (b.y - a.y).powi(2)).sqrt();
    if u < 1e-6 * a.w.max(b.w) {
        return (b.w / a.w).ln().abs() / RHO;
    }
    let (r0, r1) = rs(a, b, u);
    (r1 - r0) / RHO
}

fn rs(a: View, b: View, u: f64) -> (f64, f64) {
    let r4 = RHO.powi(4);
    let bi = |i: i32, wi: f64| (b.w * b.w - a.w * a.w + if i == 0 { r4 } else { -r4 } * u * u) / (2.0 * wi * RHO * RHO * u);
    let r = |x: f64| (-x + (x * x + 1.0).sqrt()).ln();
    (r(bi(0, a.w)), r(bi(1, b.w)))
}

/// The view a fraction `e` of the way along the optimal path.
fn along(a: View, b: View, e: f64) -> View {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let u1 = (dx * dx + dy * dy).sqrt();
    if u1 < 1e-6 * a.w.max(b.w) {
        return View { x: a.x + dx * e, y: a.y + dy * e, w: a.w * (b.w / a.w).powf(e) };
    }
    let (r0, r1) = rs(a, b, u1);
    let s = e * (r1 - r0) / RHO;
    let u = a.w / (RHO * RHO) * (r0.cosh() * (RHO * s + r0).tanh() - r0.sinh());
    let w = a.w * r0.cosh() / (RHO * s + r0).cosh();
    View { x: a.x + dx * u / u1, y: a.y + dy * u / u1, w }
}
