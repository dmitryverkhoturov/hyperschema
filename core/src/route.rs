//! Connection routing: centre to centre, straight when nothing is in the
//! way, otherwise the shortest way around the boxes in between (a
//! visibility graph over their corners), with rounded corners, then cut
//! at the borders of both ends.

use crate::board::{Bbox, Board, Kind, Node};
use std::collections::{HashMap, HashSet};

pub type Pt = (f32, f32);

/// Does segment a-b pass through the interior of `r`?
fn crosses(a: Pt, b: Pt, r: &Bbox) -> bool {
    let (mut t0, mut t1) = (0f32, 1f32);
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    for (p, q) in [(-dx, a.0 - r[0]), (dx, r[2] - a.0), (-dy, a.1 - r[1]), (dy, r[3] - a.1)] {
        if p == 0.0 {
            if q <= 0.0 {
                return false;
            }
        } else {
            let t = q / p;
            if p < 0.0 {
                t0 = t0.max(t);
            } else {
                t1 = t1.min(t);
            }
        }
    }
    t1 - t0 > 1e-4
}

fn inside(p: Pt, r: &Bbox) -> bool {
    p.0 > r[0] && p.0 < r[2] && p.1 > r[1] && p.1 < r[3]
}

fn dist(a: Pt, b: Pt) -> f32 {
    (a.0 - b.0).hypot(a.1 - b.1)
}

/// Shortest polyline from p0 to p1 avoiding `obs` (already inflated).
pub fn route(p0: Pt, p1: Pt, obs: &[Bbox]) -> Vec<Pt> {
    let obs: Vec<Bbox> = obs.iter().copied().filter(|r| !inside(p0, r) && !inside(p1, r)).collect();
    let mut active: Vec<usize> = (0..obs.len()).filter(|&i| crosses(p0, p1, &obs[i])).collect();
    if active.is_empty() {
        return vec![p0, p1];
    }
    for _ in 0..8 {
        let mut pts = vec![p0, p1];
        for &i in &active {
            let r = obs[i];
            let e = ((r[2] - r[0]) + (r[3] - r[1])) * 0.002 + 1e-3;
            for c in [(r[0] - e, r[1] - e), (r[2] + e, r[1] - e), (r[2] + e, r[3] + e), (r[0] - e, r[3] + e)] {
                if !active.iter().any(|&j| inside(c, &obs[j])) {
                    pts.push(c);
                }
            }
        }
        // Dijkstra over the visibility graph (small: a few boxes).
        let n = pts.len();
        let mut d = vec![f32::MAX; n];
        let mut prev = vec![usize::MAX; n];
        let mut done = vec![false; n];
        d[0] = 0.0;
        for _ in 0..n {
            let Some(u) = (0..n).filter(|&i| !done[i] && d[i] < f32::MAX).min_by(|&a, &b| d[a].partial_cmp(&d[b]).unwrap()) else { break };
            done[u] = true;
            if u == 1 {
                break;
            }
            for v in 0..n {
                if done[v] || v == u {
                    continue;
                }
                let nd = d[u] + dist(pts[u], pts[v]);
                if nd < d[v] && !active.iter().any(|&j| crosses(pts[u], pts[v], &obs[j])) {
                    d[v] = nd;
                    prev[v] = u;
                }
            }
        }
        if prev[1] == usize::MAX {
            return vec![p0, p1];
        }
        let mut path = vec![p1];
        let mut cur = 1;
        while cur != 0 {
            cur = prev[cur];
            path.push(pts[cur]);
        }
        path.reverse();
        let more: Vec<usize> = (0..obs.len()).filter(|i| !active.contains(i)).filter(|&i| path.windows(2).any(|w| crosses(w[0], w[1], &obs[i]))).collect();
        if more.is_empty() {
            return path;
        }
        active.extend(more);
    }
    vec![p0, p1]
}

/// Replaces corners with quadratic curves of radius up to `r`.
pub fn smooth(p: &[Pt], r: f32) -> Vec<Pt> {
    if p.len() < 3 {
        return p.to_vec();
    }
    let mut out = vec![p[0]];
    for i in 1..p.len() - 1 {
        let (a, b, c) = (p[i - 1], p[i], p[i + 1]);
        let rr = r.min(dist(a, b) * 0.5).min(dist(b, c) * 0.5);
        let towards = |from: Pt, to: Pt, l: f32| {
            let d = dist(from, to).max(1e-6);
            (from.0 + (to.0 - from.0) * l / d, from.1 + (to.1 - from.1) * l / d)
        };
        let s = towards(b, a, rr);
        let e = towards(b, c, rr);
        for k in 0..=6 {
            let t = k as f32 / 6.0;
            let u = 1.0 - t;
            out.push((u * u * s.0 + 2.0 * u * t * b.0 + t * t * e.0, u * u * s.1 + 2.0 * u * t * b.1 + t * t * e.1));
        }
    }
    out.push(p[p.len() - 1]);
    out
}

/// Where segment a (inside r) -> b (outside) crosses the border of r.
fn exit_point(a: Pt, b: Pt, r: &Bbox) -> Pt {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let mut t = 1f32;
    if dx > 0.0 {
        t = t.min((r[2] - a.0) / dx);
    }
    if dx < 0.0 {
        t = t.min((r[0] - a.0) / dx);
    }
    if dy > 0.0 {
        t = t.min((r[3] - a.1) / dy);
    }
    if dy < 0.0 {
        t = t.min((r[1] - a.1) / dy);
    }
    let t = t.clamp(0.0, 1.0);
    (a.0 + dx * t, a.1 + dy * t)
}

/// Cuts the path where it leaves box `a` and where it enters box `b`.
pub fn clip(path: &[Pt], a: &Bbox, b: &Bbox) -> Vec<Pt> {
    let within = |p: Pt, r: &Bbox| p.0 >= r[0] && p.0 <= r[2] && p.1 >= r[1] && p.1 <= r[3];
    let mut p = path.to_vec();
    if let Some(k) = (1..p.len()).find(|&k| !within(p[k], a)) {
        let q = exit_point(p[k - 1], p[k], a);
        p.drain(..k);
        p.insert(0, q);
    }
    let n = p.len();
    if let Some(k) = (0..n - 1).rev().find(|&k| !within(p[k], b)) {
        let q = exit_point(p[k + 1], p[k], b);
        p.truncate(k + 1);
        p.push(q);
    }
    if p.len() < 2 {
        return path.to_vec();
    }
    p
}

pub fn length(p: &[Pt]) -> f32 {
    p.windows(2).map(|w| dist(w[0], w[1])).sum()
}

/// The point halfway along the path.
pub fn midpoint(p: &[Pt]) -> Pt {
    let half = length(p) * 0.5;
    let mut acc = 0.0;
    for w in p.windows(2) {
        let l = dist(w[0], w[1]);
        if acc + l >= half && l > 0.0 {
            let t = (half - acc) / l;
            return (w[0].0 + (w[1].0 - w[0].0) * t, w[0].1 + (w[1].1 - w[0].1) * t);
        }
        acc += l;
    }
    p.first().copied().unwrap_or((0.0, 0.0))
}

pub fn seg_dist(p: Pt, a: Pt, b: Pt) -> f32 {
    let (abx, aby) = (b.0 - a.0, b.1 - a.1);
    let t = (((p.0 - a.0) * abx + (p.1 - a.1) * aby) / (abx * abx + aby * aby).max(1e-9)).clamp(0.0, 1.0);
    (p.0 - a.0 - abx * t).hypot(p.1 - a.1 - aby * t)
}

fn inflate(b: Bbox, m: f32) -> Bbox {
    [b[0] - m, b[1] - m, b[2] + m, b[3] + m]
}

/// Route of one link between nodes a and b.
pub fn route_between(board: &Board, a: &Node, b: &Node) -> Vec<Pt> {
    let mut near: HashSet<Option<String>> = HashSet::new();
    near.insert(None);
    let mut own: HashSet<String> = HashSet::new();
    for n in [a, b] {
        near.insert(Some(n.id.clone()));
        own.insert(n.id.clone());
        for p in board.ancestors(&n.id) {
            own.insert(p.clone());
            near.insert(Some(p));
        }
    }
    let m = a.font.min(b.font) * 1.1;
    let obs: Vec<Bbox> = board
        .nodes
        .iter()
        .filter(|n| !own.contains(&n.id))
        .filter(|n| near.contains(&board.parent(&n.id).map(String::from)))
        .filter(|n| !board.is_ancestor(&a.id, &n.id) && !board.is_ancestor(&b.id, &n.id))
        .map(|n| inflate(n.bbox(), m))
        .collect();
    let raw = route(a.center(), b.center(), &obs);
    let path = smooth(&raw, m * 2.5);
    clip(&path, &a.bbox(), &b.bbox())
}

/// Routes of every visible connection.
pub fn route_all(board: &Board) -> HashMap<String, Vec<Pt>> {
    board
        .edges
        .iter()
        .filter(|e| e.kind == Kind::Link)
        .filter_map(|e| Some((e.id.clone(), route_between(board, board.get(&e.from)?, board.get(&e.to)?))))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn straight_when_clear_around_when_blocked() {
        assert_eq!(route((0.0, 0.0), (100.0, 0.0), &[[40.0, 20.0, 60.0, 40.0]]).len(), 2);
        let p = route((0.0, 0.0), (100.0, 0.0), &[[40.0, -10.0, 60.0, 40.0]]);
        assert!(p.len() > 2);
        assert!(p.iter().all(|q| !inside(*q, &[40.0, -10.0, 60.0, 40.0])));
        assert!(p.windows(2).all(|w| !crosses(w[0], w[1], &[40.0, -10.0, 60.0, 40.0])));
    }

    #[test]
    fn clip_to_borders() {
        let p = clip(&[(0.0, 0.0), (100.0, 0.0)], &[-10.0, -10.0, 10.0, 10.0], &[90.0, -10.0, 110.0, 10.0]);
        assert_eq!(p, vec![(10.0, 0.0), (90.0, 0.0)]);
    }
}
