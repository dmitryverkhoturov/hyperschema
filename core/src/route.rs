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
    // Obstacles: siblings of either end and of their ancestors, minus the
    // ends' own subtrees (a parent's children are not in its way).
    let mut obs: Vec<Bbox> = Vec::new();
    for p in near.iter() {
        if p.as_deref().is_some_and(|p| p == a.id || p == b.id) {
            continue;
        }
        let kids: Vec<&str> = match p {
            None => board.roots_ref(),
            Some(p) => board.children(p).iter().map(String::as_str).collect(),
        };
        for id in kids {
            if !own.contains(id) {
                obs.push(inflate(board.node(id).bbox(), m));
            }
        }
    }
    let raw = route(a.center(), b.center(), &obs);
    let path = smooth(&raw, m * 2.5);
    clip(&path, &a.bbox(), &b.bbox())
}

/// Is this connection drawn? A link between a concept and something nested
/// in it (at any depth) is hidden unless `nested` asks for it.
pub fn shown(board: &Board, e: &crate::board::Edge, nested: bool) -> bool {
    e.kind == Kind::Link && (nested || !(board.is_ancestor(&e.from, &e.to) || board.is_ancestor(&e.to, &e.from)))
}

/// Routes of every visible connection, with overlapping ones pulled apart.
pub fn route_all(board: &Board, nested: bool) -> HashMap<String, Vec<Pt>> {
    finish(board, &raw_routes(board, nested, None, &HashMap::new()))
}

/// Routes before lanes. With `only`, just those connections are routed
/// again; the rest come from `prev`.
pub fn raw_routes(board: &Board, nested: bool, only: Option<&HashSet<String>>, prev: &HashMap<String, Vec<Pt>>) -> HashMap<String, Vec<Pt>> {
    let mut out = HashMap::new();
    for e in board.edges.iter().filter(|e| shown(board, e, nested)) {
        let (Some(a), Some(b)) = (board.get(&e.from), board.get(&e.to)) else { continue };
        let keep = only.is_some_and(|o| !o.contains(&e.id));
        let path = match prev.get(&e.id) {
            Some(p) if keep => p.clone(),
            _ => route_between(board, a, b),
        };
        out.insert(e.id.clone(), path);
    }
    out
}

/// Lanes for connections that run on top of each other.
pub fn finish(board: &Board, raw: &HashMap<String, Vec<Pt>>) -> HashMap<String, Vec<Pt>> {
    let mut ids = Vec::new();
    let mut paths = Vec::new();
    let mut gaps = Vec::new();
    let mut ends = Vec::new();
    for e in &board.edges {
        let (Some(p), Some(a), Some(b)) = (raw.get(&e.id), board.get(&e.from), board.get(&e.to)) else { continue };
        ids.push(e.id.clone());
        paths.push(p.clone());
        gaps.push(a.font.min(b.font) * 0.6);
        ends.push((a.bbox(), b.bbox()));
    }
    separate(&mut paths, &gaps, &ends);
    ids.into_iter().zip(paths).collect()
}

/// Bounding box of a path.
pub fn path_box(p: &[Pt]) -> Bbox {
    let mut b = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
    for q in p {
        b = [b[0].min(q.0), b[1].min(q.1), b[2].max(q.0), b[3].max(q.1)];
    }
    b
}

/// Connections that run on top of each other get lanes: each shifts
/// sideways by its lane times `gaps[i]`, then is cut at its ends' borders
/// again. Neighbours in a conflict never share a lane on the same side.
fn separate(paths: &mut [Vec<Pt>], gaps: &[f32], ends: &[(Bbox, Bbox)]) {
    let n = paths.len();
    if n < 2 {
        return;
    }
    // Segments in coarse grids, one per scale (powers of two of the gap,
    // each segment in its own and the next): only segments sharing a cell
    // are compared, and a board's big frames do not make its small notes'
    // cells huge.
    let mut grid: HashMap<(i32, i32, i32), Vec<(usize, usize)>> = HashMap::new();
    for (i, p) in paths.iter().enumerate() {
        let class = gaps[i].max(1e-3).log2().floor() as i32;
        for c in [class, class + 1] {
            let cell = 2f32.powi(c + 1) * 64.0;
            for k in 0..p.len().saturating_sub(1) {
                let (a, b) = (p[k], p[k + 1]);
                let (x0, x1) = ((a.0.min(b.0) / cell).floor() as i32, (a.0.max(b.0) / cell).floor() as i32);
                let (y0, y1) = ((a.1.min(b.1) / cell).floor() as i32, (a.1.max(b.1) / cell).floor() as i32);
                if (x1 as i64 - x0 as i64 + 1) * (y1 as i64 - y0 as i64 + 1) > 4096 {
                    continue;
                }
                for gx in x0..=x1 {
                    for gy in y0..=y1 {
                        grid.entry((c, gx, gy)).or_default().push((i, k));
                    }
                }
            }
        }
    }
    // Conflicts: (other route, same direction?).
    let mut conf: Vec<HashMap<usize, bool>> = vec![HashMap::new(); n];
    for list in grid.values() {
        for (x, &(i, k)) in list.iter().enumerate() {
            for &(j, m) in &list[x + 1..] {
                if i == j || conf[i].contains_key(&j) {
                    continue;
                }
                let tol = gaps[i].min(gaps[j]) * 0.6;
                if let Some(same) = overlap((paths[i][k], paths[i][k + 1]), (paths[j][m], paths[j][m + 1]), tol) {
                    conf[i].insert(j, same);
                    conf[j].insert(i, same);
                }
            }
        }
    }
    if conf.iter().all(|c| c.is_empty()) {
        return;
    }
    // Lanes 0, 1, -1, 2, -2...: the first free one, seen from this route.
    let mut lane: Vec<Option<i32>> = vec![None; n];
    let mut order: Vec<usize> = (0..n).filter(|&i| !conf[i].is_empty()).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(conf[i].len()));
    for &i in &order {
        let used: HashSet<i32> = conf[i].iter().filter_map(|(&j, &same)| lane[j].map(|l| if same { l } else { -l })).collect();
        let l = (0..).map(|k: i32| if k % 2 == 1 { (k + 1) / 2 } else { -(k / 2) }).find(|l| !used.contains(l)).unwrap();
        lane[i] = Some(l);
    }
    for i in 0..n {
        let Some(l) = lane[i].filter(|&l| l != 0) else { continue };
        let shifted = offset(&paths[i], l as f32 * gaps[i]);
        let (a, b) = ends[i];
        paths[i] = reclip(&shifted, &a, &b);
    }
}

/// Some(same direction) if two segments run along each other closer than
/// `tol` for more than `tol`.
fn overlap(s: (Pt, Pt), t: (Pt, Pt), tol: f32) -> Option<bool> {
    let (la, lb) = (dist(s.0, s.1), dist(t.0, t.1));
    if la < tol || lb < tol {
        return None;
    }
    let u = ((s.1 .0 - s.0 .0) / la, (s.1 .1 - s.0 .1) / la);
    let v = ((t.1 .0 - t.0 .0) / lb, (t.1 .1 - t.0 .1) / lb);
    if (u.0 * v.1 - u.1 * v.0).abs() > 0.06 {
        return None;
    }
    let side = |p: Pt| ((p.0 - s.0 .0) * u.1 - (p.1 - s.0 .1) * u.0).abs();
    if side(t.0) > tol || side(t.1) > tol {
        return None;
    }
    let along = |p: Pt| (p.0 - s.0 .0) * u.0 + (p.1 - s.0 .1) * u.1;
    let (p0, p1) = (along(t.0).min(along(t.1)), along(t.0).max(along(t.1)));
    (p1.min(la) - p0.max(0.0) > tol).then_some(u.0 * v.0 + u.1 * v.1 > 0.0)
}

/// The polyline moved sideways by `d` (left of its direction), each point
/// along the average normal of its two segments.
fn offset(p: &[Pt], d: f32) -> Vec<Pt> {
    let normal = |a: Pt, b: Pt| {
        let l = dist(a, b).max(1e-6);
        (-(b.1 - a.1) / l, (b.0 - a.0) / l)
    };
    (0..p.len())
        .map(|i| {
            let n0 = if i > 0 { normal(p[i - 1], p[i]) } else { normal(p[0], p[1]) };
            let n1 = if i + 1 < p.len() { normal(p[i], p[i + 1]) } else { n0 };
            let (nx, ny) = (n0.0 + n1.0, n0.1 + n1.1);
            let l = nx.hypot(ny).max(1e-6);
            // Keep the offset distance on turns (miter), capped for sharp ones.
            let k = d * (2.0 / l).min(2.0);
            (p[i].0 + nx / l * k, p[i].1 + ny / l * k)
        })
        .collect()
}

/// Extends both ends back into their boxes and cuts at the borders again,
/// so a shifted connection still touches its concepts.
fn reclip(p: &[Pt], a: &Bbox, b: &Bbox) -> Vec<Pt> {
    if p.len() < 2 {
        return p.to_vec();
    }
    let ext = |from: Pt, towards: Pt, r: &Bbox| {
        let l = dist(from, towards).max(1e-6);
        let k = ((r[2] - r[0]).min(r[3] - r[1]) * 0.45) / l;
        (from.0 - (towards.0 - from.0) * k, from.1 - (towards.1 - from.1) * k)
    };
    let mut q = Vec::with_capacity(p.len() + 2);
    q.push(ext(p[0], p[1], a));
    q.extend_from_slice(p);
    q.push(ext(p[p.len() - 1], p[p.len() - 2], b));
    clip(&q, a, b)
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
    fn parallel_connections_get_apart() {
        let a = [-10.0, -10.0, 10.0, 10.0];
        let b = [90.0, -10.0, 110.0, 10.0];
        let mut paths = vec![vec![(10.0, 0.0), (90.0, 0.0)], vec![(90.0, 0.0), (10.0, 0.0)]];
        separate(&mut paths, &[4.0, 4.0], &[(a, b), (b, a)]);
        let y = |p: &Vec<Pt>| p[p.len() / 2].1;
        assert!((y(&paths[0]) - y(&paths[1])).abs() > 3.0, "{paths:?}");
        assert!(paths.iter().all(|p| p.len() >= 2));
    }

    #[test]
    fn clip_to_borders() {
        let p = clip(&[(0.0, 0.0), (100.0, 0.0)], &[-10.0, -10.0, 10.0, 10.0], &[90.0, -10.0, 110.0, 10.0]);
        assert_eq!(p, vec![(10.0, 0.0), (90.0, 0.0)]);
    }
}
