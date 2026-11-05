//! Path mode: walking the board along its connections. Neighbours of the
//! current concept slide out next to it; a tap steps to one of them. The
//! board itself never moves: the walk keeps a pose per concept moved out
//! (an offset, and in the flat layouts a size too) and the editor shows a
//! copy of the board with the poses applied.
//!
//! Layouts: "Карта" keeps real sizes and nesting (a moved parent carries
//! what it holds); the flat ones draw everything out at one scale, the
//! first link's, with nesting as plain parent -> child arrows and a frame
//! out as a compact card with its title only.

use crate::board::{overlaps, Bbox, Board, Kind};
use crate::text::LINE_H;
use std::collections::{HashMap, HashSet};
use std::time::Instant;

pub type Off = (f32, f32);

/// How long concepts take to slide out or back.
pub const EASE_MS: f64 = 650.0;

/// How neighbours are laid out around the current concept.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Layout {
    /// Real sizes, each on the side where it lies on the board.
    Map,
    /// Own side, all at one scale.
    Near,
    /// Incoming on the left, outgoing on the right, one scale.
    Right,
    /// Incoming above, outgoing below, one scale. The default.
    #[default]
    Down,
}

impl Layout {
    pub fn next(self) -> Layout {
        match self {
            Layout::Map => Layout::Near,
            Layout::Near => Layout::Right,
            Layout::Right => Layout::Down,
            Layout::Down => Layout::Map,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Layout::Map => "Карта",
            Layout::Near => "Рядом",
            Layout::Right => "Дерево →",
            Layout::Down => "Дерево ↓",
        }
    }

    /// Name kept on disk (not shown).
    pub fn key(self) -> &'static str {
        match self {
            Layout::Map => "map",
            Layout::Near => "near",
            Layout::Right => "right",
            Layout::Down => "down",
        }
    }

    pub fn from_key(k: &str) -> Option<Layout> {
        [Layout::Map, Layout::Near, Layout::Right, Layout::Down].into_iter().find(|l| l.key() == k.trim())
    }

    /// One scale, nesting drawn as arrows.
    pub fn flat(self) -> bool {
        self != Layout::Map
    }
}

/// Where a concept is drawn, relative to the board: offset and size change.
#[derive(Clone, Copy, PartialEq, Default, Debug)]
pub struct Pose {
    pub dx: f32,
    pub dy: f32,
    pub dw: f32,
    pub dh: f32,
    pub df: f32,
}

impl Pose {
    fn at(o: Off) -> Pose {
        Pose { dx: o.0, dy: o.1, ..Pose::default() }
    }

    fn off(self) -> Off {
        (self.dx, self.dy)
    }

    fn lerp(a: Pose, b: Pose, e: f32) -> Pose {
        let l = |x: f32, y: f32| x + (y - x) * e;
        Pose { dx: l(a.dx, b.dx), dy: l(a.dy, b.dy), dw: l(a.dw, b.dw), dh: l(a.dh, b.dh), df: l(a.df, b.df) }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Side {
    Left,
    Right,
    Up,
    Down,
}

pub struct Trail {
    /// The path walked; the last one is the current concept.
    pub links: Vec<String>,
    /// Length of the undo stack when each link was added (Ctrl+Z first
    /// undoes edits made after the last step, then the step itself).
    pub marks: Vec<usize>,
    /// Neighbours of the current concept slid out but not touched.
    pub open: Vec<String>,
    /// Whether the current concept's neighbours are shown (a tap on it or
    /// on empty canvas folds them).
    pub expanded: bool,
    /// Stepped into, moved by hand or created here: they stay out until
    /// the walk ends, whichever way it goes on (or until closed by hand).
    pub touched: HashSet<String>,
    /// Moved by hand (undoing a step keeps those out).
    dragged: HashSet<String>,
    /// Closed by hand: they do not come back as neighbours of the current
    /// concept until the walk moves on or it is tapped.
    closed: HashSet<String>,
    /// Concepts made in the walk, with the concept each was made from:
    /// their place on the board follows where they are put here.
    born: HashMap<String, String>,
    pub layout: Layout,
    /// Where concepts are going, relative to their place on the board.
    off: HashMap<String, Pose>,
    /// Poses drawn now, easing toward `off`.
    shown: HashMap<String, Pose>,
    from: HashMap<String, Pose>,
    start: Option<Instant>,
    /// Concepts that existed at the last sync (new ones get adopted).
    known: HashSet<String>,
    /// The walk is over: everything slides back, then the editor drops it.
    pub leaving: bool,
}

/// Concepts one step from `id`: connections either way, parent, children.
pub fn neighbors(b: &Board, id: &str) -> Vec<String> {
    let mut v: Vec<String> = Vec::new();
    for e in &b.edges {
        let other = if e.from == id {
            &e.to
        } else if e.to == id {
            &e.from
        } else {
            continue;
        };
        if other != id && b.get(other).is_some() && !v.contains(other) {
            v.push(other.clone());
        }
    }
    v
}

/// Whether `n` continues the thought from `c`: a child, a connection whose
/// head points at `n`, or one without heads. A parent or a connection
/// pointing only at `c` comes before it.
pub fn outgoing(b: &Board, c: &str, n: &str) -> bool {
    let mut any = false;
    for e in &b.edges {
        if !((e.from == c && e.to == n) || (e.from == n && e.to == c)) {
            continue;
        }
        any = true;
        if e.kind == Kind::Contains {
            if e.from == c {
                return true;
            }
            continue;
        }
        let at_n = (e.to == n && e.arrow_to) || (e.from == n && e.arrow_from);
        let at_c = (e.to == c && e.arrow_to) || (e.from == c && e.arrow_from);
        if at_n || !at_c {
            return true;
        }
    }
    !any
}

fn grow(b: Bbox, d: f32) -> Bbox {
    [b[0] - d, b[1] - d, b[2] + d, b[3] + d]
}

/// A free spot for a `w`x`h` box beside `cb`, along the ray at angle
/// `base` (turning a little and stepping further out when it is taken).
fn spot(cb: Bbox, w: f32, h: f32, base: f32, gap: f32, taken: &[Bbox]) -> Bbox {
    let (cw, ch) = (cb[2] - cb[0], cb[3] - cb[1]);
    let step = w.max(h) * 0.6 + gap;
    let cc = ((cb[0] + cb[2]) * 0.5, (cb[1] + cb[3]) * 0.5);
    let mut best = None;
    for ring in 0..14 {
        for da in [0.0f32, 0.3, -0.3, 0.6, -0.6, 0.9, -0.9, 1.2, -1.2] {
            let a = base + da;
            let (dx, dy) = (a.cos(), a.sin());
            // Distance along the ray where the two boxes stop overlapping.
            let ex = (cw + w) * 0.5 + gap;
            let ey = (ch + h) * 0.5 + gap;
            let t = (if dx.abs() > 1e-4 { ex / dx.abs() } else { f32::MAX }).min(if dy.abs() > 1e-4 { ey / dy.abs() } else { f32::MAX }) + ring as f32 * step;
            let (x, y) = (cc.0 + dx * t - w * 0.5, cc.1 + dy * t - h * 0.5);
            let cand = [x, y, x + w, y + h];
            if best.is_none() {
                best = Some(cand);
            }
            if !taken.iter().any(|o| overlaps(o, &grow(cand, gap * 0.4))) && !overlaps(&cb, &grow(cand, gap * 0.4)) {
                return cand;
            }
        }
    }
    best.unwrap_or(cb)
}

impl Trail {
    /// A walk along `links`; `kept` are concepts left out last time, where
    /// they were.
    pub fn new(b: &Board, links: Vec<String>, kept: &[(String, Pose)], undo_len: usize, layout: Layout) -> Trail {
        let touched = kept.iter().filter(|k| b.get(&k.0).is_some() && !links.contains(&k.0)).map(|k| k.0.clone()).collect();
        let mut t = Trail {
            marks: vec![undo_len; links.len()],
            links,
            open: Vec::new(),
            expanded: true,
            touched,
            dragged: HashSet::new(),
            closed: HashSet::new(),
            born: HashMap::new(),
            layout,
            off: HashMap::new(),
            shown: HashMap::new(),
            from: HashMap::new(),
            start: None,
            known: b.nodes.iter().map(|n| n.id.clone()).collect(),
            leaving: false,
        };
        for (id, p) in kept {
            if t.touched.contains(id) {
                t.off.insert(id.clone(), Pose::at(p.off()));
            }
        }
        t.reopen(b);
        t.relayout(b);
        // Everything left out comes back exactly where it was, the path
        // too (laid out afresh it would land elsewhere).
        if kept.iter().any(|k| t.links.contains(&k.0)) {
            for (id, p) in kept {
                if b.get(id).is_some() && t.off.contains_key(id) {
                    t.off.insert(id.clone(), *p);
                }
            }
            // Untouched neighbours go out again around what came back.
            for id in t.open.clone() {
                if !t.touched.contains(&id) && !t.links.contains(&id) {
                    t.off.remove(&id);
                }
            }
            t.settle(b);
        }
        t
    }

    /// What stays out besides the path, to bring back on the next walk.
    pub fn kept(&self) -> Vec<(String, Pose)> {
        self.touched.iter().chain(self.links.iter()).filter_map(|id| self.off.get(id).map(|p| (id.clone(), *p))).collect()
    }

    /// Out concepts that can be closed by hand: everything out but the path.
    pub fn closable(&self) -> Vec<String> {
        self.touched.iter().filter(|id| !self.links.contains(id)).cloned().collect()
    }

    /// Closes `id` by hand: it goes back to its place on the board.
    pub fn close(&mut self, b: &Board, id: &str) -> bool {
        if self.links.iter().any(|l| l == id) || !(self.touched.contains(id) || self.open.iter().any(|o| o == id)) {
            return false;
        }
        self.touched.remove(id);
        self.dragged.remove(id);
        self.open.retain(|o| o != id);
        self.closed.insert(id.to_string());
        self.settle(b);
        true
    }

    pub fn current(&self) -> &str {
        self.links.last().map_or("", String::as_str)
    }

    /// Everything out: the path, touched concepts, open neighbours.
    pub fn out(&self) -> HashSet<String> {
        let mut s: HashSet<String> = self.links.iter().cloned().collect();
        s.extend(self.touched.iter().cloned());
        s.extend(self.open.iter().cloned());
        s
    }

    /// What is drawn bright: what is out and, on the map, everything
    /// nested in it. There a frame around the walk (the current concept's
    /// parent) lights up alone, not with all it holds.
    pub fn bright(&self, b: &Board) -> HashSet<String> {
        if self.layout.flat() {
            return self.out().into_iter().filter(|id| b.get(id).is_some()).collect();
        }
        let mut s = HashSet::new();
        let walk: Vec<&String> = self.links.iter().chain(self.touched.iter()).collect();
        for id in self.out() {
            if b.get(&id).is_none() {
                continue;
            }
            if walk.iter().any(|w| b.is_ancestor(&id, w)) {
                s.insert(id);
            } else {
                s.extend(b.subtree(&id));
            }
        }
        s
    }

    /// Neighbours of `id` not out yet (the "+N" badge).
    pub fn unopened(&self, b: &Board, id: &str) -> usize {
        let out = self.out();
        neighbors(b, id).iter().filter(|n| !out.contains(*n)).count()
    }

    /// Pose of `id` in `m`: its own or, on the map, its nearest moved
    /// ancestor's offset (what it holds moves along, at its size).
    fn pose_in(&self, m: &HashMap<String, Pose>, b: &Board, id: &str) -> Pose {
        if let Some(p) = m.get(id) {
            return *p;
        }
        if self.layout.flat() {
            return Pose::default();
        }
        b.ancestors(id).iter().find_map(|a| m.get(a).map(|p| Pose::at(p.off()))).unwrap_or_default()
    }

    /// Offset of `id` where it is going.
    pub fn target_of(&self, b: &Board, id: &str) -> Off {
        self.pose_in(&self.off, b, id).off()
    }

    /// Box of `id` where it is going.
    pub fn target_box(&self, b: &Board, id: &str) -> Option<Bbox> {
        let n = b.get(id)?;
        let p = self.pose_in(&self.off, b, id);
        Some([n.x + p.dx, n.y + p.dy, n.x + p.dx + n.w + p.dw, n.y + p.dy + n.h + p.dh])
    }

    /// The board as drawn: every concept at its pose. In the flat layouts
    /// nesting that touches something out is undone (it is out on its
    /// own): between two out concepts it becomes a parent -> child arrow.
    pub fn display(&self, b: &Board) -> Board {
        let mut d = b.clone();
        if self.layout.flat() && !self.leaving {
            let out = self.out();
            let mut changed = false;
            d.edges.retain_mut(|e| {
                if e.kind != Kind::Contains || !(out.contains(&e.from) || out.contains(&e.to)) {
                    return true;
                }
                changed = true;
                if out.contains(&e.from) && out.contains(&e.to) {
                    e.kind = Kind::Link;
                    (e.arrow_from, e.arrow_to) = (false, true);
                    return true;
                }
                false
            });
            if changed {
                d.reindex();
            }
        }
        if self.shown.is_empty() {
            return d;
        }
        let poses: Vec<Pose> = d.nodes.iter().map(|n| self.pose_in(&self.shown, b, &n.id)).collect();
        for (n, p) in d.nodes.iter_mut().zip(poses) {
            n.x += p.dx;
            n.y += p.dy;
            n.w += p.dw;
            n.h += p.dh;
            n.font += p.df;
        }
        d
    }

    /// Moves `ids` in `d` (a board as drawn, from `display`) to their poses.
    pub fn pose_all(&self, b: &Board, d: &mut Board, ids: &HashSet<String>) {
        for id in ids {
            let (Some(n), p) = (b.get(id), self.pose_in(&self.shown, b, id)) else { continue };
            let (x, y, w, h, f) = (n.x + p.dx, n.y + p.dy, n.w + p.dw, n.h + p.dh, n.font + p.df);
            if let Some(m) = d.get_mut(id) {
                (m.x, m.y, m.w, m.h, m.font) = (x, y, w, h, f);
            }
        }
    }

    /// Concepts whose drawn place differs from the board (their connections
    /// need routing while they slide).
    pub fn moving(&self) -> HashSet<String> {
        self.shown.keys().chain(self.from.keys()).cloned().collect()
    }

    /// Where the other out concepts are going (what new ones must avoid);
    /// on the map frames around the walk do not count, it lies inside them.
    fn taken(&self, b: &Board, except: &[String]) -> Vec<Bbox> {
        let walk: Vec<&String> = self.links.iter().chain(self.touched.iter()).collect();
        let flat = self.layout.flat();
        self.out()
            .iter()
            .filter(|id| !except.contains(id))
            .filter(|id| flat || (!walk.iter().any(|w| b.is_ancestor(id, w)) && !except.iter().any(|x| b.is_ancestor(id, x))))
            .filter_map(|id| self.target_box(b, id))
            .collect()
    }

    /// Scale of the flat layouts: the first link's font.
    fn unit(&self, b: &Board) -> f32 {
        self.links.first().and_then(|l| b.get(l)).map_or(20.0, |n| n.font)
    }

    /// Size of `id` drawn at font `f`: the concept scaled, a frame as a
    /// compact card with its title (what it holds stays in it).
    fn card(&self, b: &Board, id: &str, f: f32) -> (f32, f32) {
        let n = b.node(id);
        let k = f / n.font.max(1e-3);
        if !b.has_children(id) {
            return (n.w * k, n.h * k);
        }
        let w = (n.w * k).clamp(6.0 * f, 14.0 * f);
        // Roughly half an em per character is close enough for the card.
        let inner = w - f;
        let lines: f32 = n.text.split('\n').map(|l| (l.chars().count() as f32 * 0.56 * f / inner).ceil().max(1.0)).sum::<f32>().max(1.0);
        (w, lines * LINE_H * f + 1.2 * f)
    }

    /// Puts `id`'s card with its top left at (`x`, `y`).
    #[allow(clippy::too_many_arguments)]
    fn put(&mut self, b: &Board, id: &str, x: f32, y: f32, w: f32, h: f32, f: f32) {
        let n = b.node(id);
        self.off.insert(id.to_string(), Pose { dx: x - n.x, dy: y - n.y, dw: w - n.w, dh: h - n.h, df: f - n.font });
    }

    /// Lays out `ids` (neighbours of `c` not laid out yet) by the layout.
    fn lay(&mut self, b: &Board, c: &str, ids: &[String]) {
        match self.layout {
            Layout::Map | Layout::Near => {
                for n in ids {
                    let taken = self.taken(b, std::slice::from_ref(n));
                    self.place(b, c, n, &taken);
                }
            }
            Layout::Right | Layout::Down => {
                let (outs, ins): (Vec<String>, Vec<String>) = ids.iter().cloned().partition(|n| outgoing(b, c, n));
                let (back, fwd) = if self.layout == Layout::Right { (Side::Left, Side::Right) } else { (Side::Up, Side::Down) };
                let taken = self.taken(b, ids);
                self.column(b, c, &ins, back, &taken);
                let taken = self.taken(b, &outs);
                self.column(b, c, &outs, fwd, &taken);
            }
        }
    }

    /// Lays `n` out beside `c`, on the side where it lies on the board. On
    /// the map nesting relatives stay put (the child is inside, the parent
    /// around), and so does a concept holding something else that is out.
    fn place(&mut self, b: &Board, c: &str, n: &str, taken: &[Bbox]) {
        let (Some(cn), Some(nn)) = (b.get(c), b.get(n)) else { return };
        let Some(cb) = self.target_box(b, c) else { return };
        let (hx, hy) = (nn.center().0 - cn.center().0, nn.center().1 - cn.center().1);
        let mut base = if hx.hypot(hy) < 1e-3 { 0.0 } else { hy.atan2(hx) };
        if self.layout.flat() {
            // A parent comes out above: its centre says little about a side.
            if b.is_ancestor(n, c) {
                base = -std::f32::consts::FRAC_PI_2;
            }
            let f = self.unit(b);
            let (w, h) = self.card(b, n, f);
            let s = spot(cb, w, h, base, f * 2.0, taken);
            self.put(b, n, s[0], s[1], w, h, f);
            return;
        }
        if b.is_ancestor(c, n) || b.is_ancestor(n, c) {
            return;
        }
        let holds_out = self.links.iter().chain(self.touched.iter()).any(|o| o != n && b.is_ancestor(n, o));
        if holds_out {
            return;
        }
        let nb = nn.bbox();
        let s = spot(cb, nb[2] - nb[0], nb[3] - nb[1], base, cn.font.max(nn.font) * 2.0, taken);
        self.off.insert(n.to_string(), Pose::at((s[0] - nb[0], s[1] - nb[1])));
    }

    /// Trees: `ids` in a column on `side` of `c`, in the order they lie on
    /// the board; more than ten wrap into further columns. If that runs
    /// into something out, the whole group moves further out.
    fn column(&mut self, b: &Board, c: &str, ids: &[String], side: Side, taken: &[Bbox]) {
        let Some(cb) = self.target_box(b, c) else { return };
        if ids.is_empty() {
            return;
        }
        let f = self.unit(b);
        let across = matches!(side, Side::Left | Side::Right);
        let mut cards: Vec<(String, f32, f32, f32)> = ids
            .iter()
            .filter_map(|id| {
                let n = b.get(id)?;
                let (w, h) = self.card(b, id, f);
                Some((id.clone(), w, h, if across { n.center().1 } else { n.center().0 }))
            })
            .collect();
        cards.sort_by(|a, b| a.3.total_cmp(&b.3));
        let (gap, main) = (f * 1.0, f * 4.0);
        // Each card as (out from the edge, along it, size out, size along).
        let mut boxes: Vec<(String, f32, f32, f32, f32)> = Vec::new();
        let mut depth = 0.0;
        for chunk in cards.chunks(10) {
            let len: f32 = chunk.iter().map(|c| if across { c.2 } else { c.1 }).sum::<f32>() + gap * (chunk.len() - 1) as f32;
            let thick = chunk.iter().map(|c| if across { c.1 } else { c.2 }).fold(0.0f32, f32::max);
            let mut along = -len * 0.5;
            for (id, w, h, _) in chunk {
                let (sd, sa) = if across { (*w, *h) } else { (*h, *w) };
                boxes.push((id.clone(), depth, along, sd, sa));
                along += sa + gap;
            }
            depth += thick + main * 0.6;
        }
        let cc = ((cb[0] + cb[2]) * 0.5, (cb[1] + cb[3]) * 0.5);
        let world = |push: f32, d: f32, a: f32, sd: f32, sa: f32| -> Bbox {
            let d = main + push + d;
            match side {
                Side::Right => [cb[2] + d, cc.1 + a, cb[2] + d + sd, cc.1 + a + sa],
                Side::Left => [cb[0] - d - sd, cc.1 + a, cb[0] - d, cc.1 + a + sa],
                Side::Down => [cc.0 + a, cb[3] + d, cc.0 + a + sa, cb[3] + d + sd],
                Side::Up => [cc.0 + a, cb[1] - d - sd, cc.0 + a + sa, cb[1] - d],
            }
        };
        let mut push = 0.0;
        for _ in 0..10 {
            let clear = boxes.iter().all(|(_, d, a, sd, sa)| {
                let r = world(push, *d, *a, *sd, *sa);
                !taken.iter().any(|o| overlaps(o, &grow(r, gap * 0.4)))
            });
            if clear {
                break;
            }
            push += depth;
        }
        for (id, d, a, sd, sa) in boxes {
            let r = world(push, d, a, sd, sa);
            self.put(b, &id, r[0], r[1], r[2] - r[0], r[3] - r[1], f);
        }
    }

    /// Opens the current concept's neighbours that are not out yet.
    fn reopen(&mut self, b: &Board) {
        let out = self.out();
        self.open = neighbors(b, self.current()).into_iter().filter(|n| !out.contains(n) && !self.closed.contains(n)).collect();
        self.expanded = true;
    }

    /// Lays everything out again (a new walk, another layout): the path
    /// link by link, each beside the one before it, then the neighbours.
    /// Concepts dragged by hand keep their place.
    fn relayout(&mut self, b: &Board) {
        let kept: Vec<(String, Off)> = self.touched.iter().filter_map(|id| self.off.get(id).map(|p| (id.clone(), p.off()))).collect();
        self.off.clear();
        for (id, o) in kept {
            self.off.insert(id, Pose::at(o));
        }
        if self.layout.flat() {
            // The first link takes its card size before anything goes beside it.
            if let Some(first) = self.links.first().cloned() {
                self.fit(b, &first);
            }
        }
        for i in 1..self.links.len() {
            let (c, n) = (self.links[i - 1].clone(), self.links[i].clone());
            if !self.touched.contains(&n) {
                self.lay(b, &c, &[n]);
            }
        }
        self.settle(b);
    }

    /// Flat layouts: `id` at the walk's scale. Without a place yet (the
    /// first link) it keeps its centre.
    fn fit(&mut self, b: &Board, id: &str) {
        let Some(n) = b.get(id) else { return };
        let f = self.unit(b);
        let (w, h) = self.card(b, id, f);
        let p = self.off.entry(id.to_string()).or_insert_with(|| Pose { dx: (n.w - w) * 0.5, dy: (n.h - h) * 0.5, ..Pose::default() });
        (p.dw, p.dh, p.df) = (w - n.w, h - n.h, f - n.font);
    }

    /// Another layout: everything moves over to it.
    pub fn set_layout(&mut self, b: &Board, l: Layout) {
        if self.layout == l {
            return;
        }
        self.layout = l;
        self.relayout(b);
    }

    /// Lays out what is newly out, sends back what is not, starts easing.
    fn settle(&mut self, b: &Board) {
        let out = self.out();
        self.off.retain(|id, _| out.contains(id));
        let cur = self.current().to_string();
        let fresh: Vec<String> = self.open.iter().filter(|n| !self.off.contains_key(*n)).cloned().collect();
        self.lay(b, &cur, &fresh);
        if self.layout.flat() {
            for id in out {
                self.fit(b, &id);
            }
        }
        self.ease();
    }

    fn ease(&mut self) {
        let mut from = HashMap::new();
        for id in self.shown.keys().chain(self.off.keys()) {
            let s = self.shown.get(id).copied().unwrap_or_default();
            if self.off.get(id).copied().unwrap_or_default() != s {
                from.insert(id.clone(), s);
            }
        }
        if from.is_empty() {
            return;
        }
        // Concepts already sliding keep going from where they are.
        for (id, s) in from {
            self.from.insert(id, s);
        }
        for id in self.off.keys() {
            self.shown.entry(id.clone()).or_default();
        }
        self.start = Some(crate::clock::now());
    }

    /// Moves the drawn poses along; true while something slides.
    pub fn tick(&mut self) -> bool {
        let Some(start) = self.start else { return false };
        let t = (crate::clock::since(start).as_secs_f64() * 1000.0 / EASE_MS).min(1.0) as f32;
        // Slow in and out: where each one comes from stays readable.
        let e = if t < 0.5 { 4.0 * t * t * t } else { 1.0 - (-2.0 * t + 2.0).powi(3) / 2.0 };
        for (id, f) in &self.from {
            let to = self.off.get(id).copied().unwrap_or_default();
            self.shown.insert(id.clone(), Pose::lerp(*f, to, e));
        }
        if t >= 1.0 {
            self.from.clear();
            self.start = None;
            self.shown = self.off.clone();
        }
        true
    }

    pub fn animating(&self) -> bool {
        self.start.is_some()
    }

    /// Skips the slide: everything is drawn where it is going.
    pub fn finish(&mut self) {
        self.from.clear();
        self.start = None;
        self.shown = self.off.clone();
    }

    /// The walk ends: everything slides back to its place.
    pub fn leave(&mut self) {
        self.leaving = true;
        self.links.clear();
        self.open.clear();
        self.touched.clear();
        self.dragged.clear();
        self.born.clear();
        self.off.clear();
        self.ease();
    }

    /// A tap on `n`: one step to it (on a link of the path: back to it).
    pub fn step(&mut self, b: &Board, n: &str, undo_len: usize) {
        if let Some(k) = self.links.iter().position(|l| l == n) {
            self.back_to(b, k);
            return;
        }
        if b.get(n).is_none() {
            return;
        }
        // A concept not out yet (a far one tapped through the dim) comes
        // out beside the current one first.
        if !self.out().contains(n) {
            let cur = self.current().to_string();
            self.lay(b, &cur, &[n.to_string()]);
        }
        self.links.push(n.to_string());
        self.marks.push(undo_len);
        self.closed.clear();
        self.open.clear();
        self.reopen(b);
        self.settle(b);
    }

    /// Back to link `k`: the links after it leave the path.
    pub fn back_to(&mut self, b: &Board, k: usize) {
        if k + 1 >= self.links.len() {
            // Already there: just open its neighbours again.
            if !self.expanded {
                self.reopen(b);
                self.settle(b);
            }
            return;
        }
        // The links after it leave the path but stay out where they are:
        // going another way does not take away what was looked at.
        let gone: Vec<String> = self.links.drain(k + 1..).collect();
        self.marks.truncate(k + 1);
        self.touched.extend(gone);
        self.closed.clear();
        self.reopen(b);
        self.settle(b);
    }

    /// Ctrl+Z on the walk: the last step is undone as if never taken (the
    /// concept goes back among the neighbours unless it was moved by hand).
    pub fn pop(&mut self, b: &Board) -> bool {
        if self.links.len() < 2 {
            return false;
        }
        let g = self.links.pop().unwrap_or_default();
        self.marks.pop();
        if !self.dragged.contains(&g) {
            self.touched.remove(&g);
        }
        self.reopen(b);
        self.settle(b);
        true
    }

    /// A tap on the current concept: folds or unfolds its neighbours.
    pub fn toggle(&mut self, b: &Board) {
        if self.expanded && !self.open.is_empty() {
            self.fold(b);
        } else {
            self.closed.clear();
            self.reopen(b);
            self.settle(b);
        }
    }

    /// Untouched neighbours slide back (a tap on empty canvas).
    pub fn fold(&mut self, b: &Board) {
        self.open.clear();
        self.expanded = false;
        self.settle(b);
    }

    /// Dragging `id` by hand: it goes exactly where it is put.
    pub fn drag(&mut self, id: &str, o: Off) {
        let p = self.off.entry(id.to_string()).or_default();
        (p.dx, p.dy) = o;
        let p = *p;
        self.shown.insert(id.to_string(), p);
        self.from.remove(id);
    }

    /// After a drag: it stays out until the walk ends.
    pub fn touch(&mut self, id: &str) {
        self.touched.insert(id.to_string());
        self.dragged.insert(id.to_string());
        self.open.retain(|o| o != id);
    }

    /// Whether `id` was made in this walk.
    pub fn born(&self, id: &str) -> bool {
        self.born.contains_key(id)
    }

    /// Size `id` takes here: one scale in the flat layouts, its own on the map.
    fn dims(&self, b: &Board, id: &str) -> (f32, f32, f32) {
        let n = b.node(id);
        if self.layout.flat() {
            let f = self.unit(b);
            let (w, h) = self.card(b, id, f);
            (w, h, f)
        } else {
            (n.w, n.h, n.font)
        }
    }

    /// A concept just made from `anchor`: out and touched, centred at `at`
    /// or laid out as one more neighbour. Untouched neighbours in its way
    /// make room (in a tree it joins the column of what follows).
    pub fn adopt(&mut self, b: &Board, id: &str, anchor: &str, at: Option<(f32, f32)>) {
        if b.get(id).is_none() {
            return;
        }
        self.known.insert(id.to_string());
        self.touched.insert(id.to_string());
        self.born.insert(id.to_string(), anchor.to_string());
        let (w, h, f) = self.dims(b, id);
        let cur = self.current().to_string();
        let tree = matches!(self.layout, Layout::Right | Layout::Down);
        match at {
            Some(p) => {
                // Dropped onto something that stays out: the nearest free
                // spot away from it (untouched neighbours move instead).
                let open: HashSet<&String> = self.open.iter().collect();
                let keep: Vec<String> = self.out().into_iter().filter(|o| o != id && !open.contains(o)).collect();
                let boxes: Vec<Bbox> = keep.iter().filter_map(|o| self.target_box(b, o)).collect();
                let mine = [p.0 - w * 0.5, p.1 - h * 0.5, p.0 + w * 0.5, p.1 + h * 0.5];
                let r = match boxes.iter().find(|o| overlaps(o, &grow(mine, f * 0.5))) {
                    Some(o) => {
                        let (ox, oy) = ((o[0] + o[2]) * 0.5, (o[1] + o[3]) * 0.5);
                        spot(*o, w, h, (p.1 - oy).atan2(p.0 - ox), f * 1.5, &boxes)
                    }
                    None => mine,
                };
                self.put(b, id, r[0], r[1], w, h, f)
            }
            None if tree && anchor == cur => {
                let mut group: Vec<String> = self.open.iter().filter(|o| outgoing(b, &cur, o)).cloned().collect();
                for g in &group {
                    self.off.remove(g);
                }
                group.push(id.to_string());
                self.lay(b, &cur, &group);
            }
            None => self.lay(b, anchor, &[id.to_string()]),
        }
        if self.layout.flat() {
            self.fit(b, id);
        }
        if let Some(me) = self.target_box(b, id) {
            let room = grow(me, f);
            let clash: Vec<String> = self.open.iter().filter(|o| self.target_box(b, o).is_some_and(|ob| overlaps(&ob, &room))).cloned().collect();
            for c in &clash {
                self.off.remove(c);
            }
            self.lay(b, &cur, &clash);
            if self.layout.flat() {
                for c in &clash {
                    self.fit(b, c);
                }
            }
        }
        self.ease();
    }

    /// Where `id` is drawn when done sliding, with its font.
    pub fn drawn(&self, b: &Board, id: &str) -> Option<(Bbox, f32)> {
        let n = b.get(id)?;
        Some((self.target_box(b, id)?, n.font + self.pose_in(&self.off, b, id).df))
    }

    /// Keeps `id` drawn at `bx` with font `f` after its place on the board
    /// changed.
    pub fn set_drawn(&mut self, b: &Board, id: &str, bx: Bbox, f: f32) {
        let Some(n) = b.get(id) else { return };
        let p = Pose { dx: bx[0] - n.x, dy: bx[1] - n.y, dw: bx[2] - bx[0] - n.w, dh: bx[3] - bx[1] - n.h, df: f - n.font };
        self.off.insert(id.to_string(), p);
        self.shown.insert(id.to_string(), p);
        self.from.remove(id);
    }

    /// Board place of a concept made here, from where it is drawn: around
    /// the concept it came from the same way as here, at that concept's
    /// scale. Beside its card means beside it on the board, so a big frame
    /// drawn as a small card does not end up on top of it.
    /// Returns (x, y, w, h, font).
    pub fn home_for(&self, b: &Board, id: &str) -> Option<(f32, f32, f32, f32, f32)> {
        let anchor = self.born.get(id).filter(|a| b.get(a.as_str()).is_some()).cloned().unwrap_or_else(|| self.current().to_string());
        let a = b.get(&anchor)?;
        let (ab, af) = self.drawn(b, &anchor)?;
        let (nb, nf) = self.drawn(b, id)?;
        let k = (af / a.font.max(1e-3)).max(1e-3);
        let home = a.bbox();
        let axis = |v: f32, lo: f32, hi: f32, hlo: f32, hhi: f32| {
            if v < lo {
                hlo - (lo - v) / k
            } else if v > hi {
                hhi + (v - hi) / k
            } else {
                hlo + (v - lo) / (hi - lo).max(1e-3) * (hhi - hlo)
            }
        };
        let (cx, cy) = ((nb[0] + nb[2]) * 0.5, (nb[1] + nb[3]) * 0.5);
        let (w, h) = ((nb[2] - nb[0]) / k, (nb[3] - nb[1]) / k);
        let hx = axis(cx, ab[0], ab[2], home[0], home[2]);
        let hy = axis(cy, ab[1], ab[3], home[1], home[3]);
        Some((hx - w * 0.5, hy - h * 0.5, w, h, nf / k))
    }

    /// Own offset of `id`, if it was moved out itself.
    pub fn own(&self, id: &str) -> Option<Off> {
        self.off.get(id).map(|p| p.off())
    }

    /// Catches up with edits: deleted concepts leave the walk, new ones
    /// come out (beside the current concept if made next to it), new
    /// connections of the current concept open. A new concept put where
    /// the current one is drawn rather than where it lies must move by the
    /// current offset to lie beside it on the board: returns such moves.
    pub fn sync(&mut self, b: &Board) -> Vec<(String, Off)> {
        let mut moves = Vec::new();
        let exists = |id: &String| b.get(id).is_some();
        let before = self.out();
        self.links.retain(exists);
        self.marks.truncate(self.links.len());
        if self.links.is_empty() {
            self.known = b.nodes.iter().map(|n| n.id.clone()).collect();
            return moves;
        }
        self.touched.retain(|id| b.get(id).is_some());
        self.open.retain(exists);
        let cur = self.current().to_string();
        let co = self.target_of(b, &cur);
        let fresh: Vec<String> = b.nodes.iter().filter(|n| !self.known.contains(&n.id)).map(|n| n.id.clone()).collect();
        if let Some(c) = b.get(&cur) {
            let reach = c.w.max(c.h) * 6.0 + c.font * 20.0;
            let (hc, dc) = (c.center(), (c.center().0 + co.0, c.center().1 + co.1));
            for id in fresh {
                let n = b.node(&id);
                // On the map, nested in something already out: it follows its parent.
                if !self.layout.flat() && b.ancestors(&id).iter().any(|a| self.off.contains_key(a) || self.links.contains(a)) {
                    self.touched.insert(id);
                    continue;
                }
                let p = n.center();
                let (d_home, d_disp) = ((p.0 - hc.0).hypot(p.1 - hc.1), (p.0 - dc.0).hypot(p.1 - dc.1));
                // Made far away: it stays where it was put, but out.
                if d_home.min(d_disp) > reach {
                    self.touched.insert(id);
                    continue;
                }
                if d_disp < d_home && (co.0 != 0.0 || co.1 != 0.0) {
                    moves.push((id.clone(), (-co.0, -co.1)));
                }
                self.off.insert(id.clone(), Pose::at(co));
                self.shown.insert(id.clone(), Pose::at(co));
                self.touched.insert(id);
            }
        }
        if self.expanded {
            let out = self.out();
            for n in neighbors(b, &cur) {
                if !out.contains(&n) && !self.closed.contains(&n) {
                    self.open.push(n);
                }
            }
        }
        self.known = b.nodes.iter().map(|n| n.id.clone()).collect();
        if self.out() != before {
            self.settle(b);
        }
        moves
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::{Kind, Node, Shape};
    use serde_json::Map;

    fn node(id: &str, x: f32, y: f32) -> Node {
        Node { id: id.into(), x, y, w: 100.0, h: 50.0, text: id.into(), font: 20.0, shape: Shape::Box, color: None, fill: None, extra: Map::new() }
    }

    /// a in the middle; b far right, c far left, d far below; b–e chained.
    fn board() -> Board {
        let mut b = Board::new();
        b.add_node(node("a", 0.0, 0.0));
        b.add_node(node("b", 3000.0, 0.0));
        b.add_node(node("c", -3000.0, 0.0));
        b.add_node(node("d", 0.0, 3000.0));
        b.add_node(node("e", 6000.0, 0.0));
        b.add_edge("a", "b", Kind::Link);
        b.add_edge("c", "a", Kind::Link);
        b.add_edge("a", "d", Kind::Link);
        b.add_edge("b", "e", Kind::Link);
        b
    }

    fn settle_all(t: &mut Trail) {
        t.start = None;
        t.from.clear();
        t.shown = t.off.clone();
    }

    fn at(t: &Trail, b: &Board, id: &str) -> (f32, f32) {
        let d = t.display(b);
        let n = d.node(id);
        (n.center().0, n.center().1)
    }

    #[test]
    fn neighbours_slide_out_on_their_own_side() {
        let b = board();
        let mut t = Trail::new(&b, vec!["a".into()], &[], 0, Layout::Map);
        settle_all(&mut t);
        let mut open = t.open.clone();
        open.sort();
        assert_eq!(open, ["b", "c", "d"]);
        let a = at(&t, &b, "a");
        let (pb, pc, pd) = (at(&t, &b, "b"), at(&t, &b, "c"), at(&t, &b, "d"));
        assert!(pb.0 > a.0 && pb.0 < 1000.0, "b right of a and close: {pb:?}");
        assert!(pc.0 < a.0 && pc.0 > -1000.0, "c left of a and close: {pc:?}");
        assert!(pd.1 > a.1 && pd.1 < 1000.0, "d below a and close: {pd:?}");
        assert_eq!(at(&t, &b, "e"), (6050.0, 25.0), "not a neighbour: stays");
    }

    #[test]
    fn board_never_moves() {
        let b = board();
        let before = b.serialize();
        let mut t = Trail::new(&b, vec!["a".into()], &[], 0, Layout::Map);
        t.step(&b, "b", 0);
        t.step(&b, "e", 0);
        settle_all(&mut t);
        assert_eq!(b.serialize(), before);
    }

    #[test]
    fn step_moves_on_and_untouched_go_back() {
        let b = board();
        let mut t = Trail::new(&b, vec!["a".into()], &[], 0, Layout::Map);
        t.step(&b, "b", 0);
        settle_all(&mut t);
        assert_eq!(t.links, ["a", "b"]);
        assert_eq!(t.open, ["e"]);
        // c and d were not touched: back home.
        assert_eq!(at(&t, &b, "c"), (-2950.0, 25.0));
        assert_eq!(at(&t, &b, "d"), (50.0, 3025.0));
        // e slid out next to where b is drawn now.
        let (pb, pe) = (at(&t, &b, "b"), at(&t, &b, "e"));
        assert!(pe.0 > pb.0 && pe.0 - pb.0 < 1000.0, "{pb:?} {pe:?}");
    }

    #[test]
    fn dragged_stays_out_after_step() {
        let b = board();
        let mut t = Trail::new(&b, vec!["a".into()], &[], 0, Layout::Map);
        t.drag("d", (500.0, -2000.0));
        t.touch("d");
        t.step(&b, "b", 0);
        settle_all(&mut t);
        assert_eq!(at(&t, &b, "d"), (550.0, 1025.0));
    }

    #[test]
    fn back_and_pop_shorten_the_path() {
        let b = board();
        let mut t = Trail::new(&b, vec!["a".into()], &[], 0, Layout::Map);
        t.step(&b, "b", 0);
        t.step(&b, "e", 0);
        assert!(t.pop(&b));
        assert_eq!(t.links, ["a", "b"]);
        t.back_to(&b, 0);
        assert_eq!(t.links, ["a"]);
        assert!(!t.pop(&b));
        // Links left by going back stay out; popped ones do not.
        assert!(t.touched.contains("b") && !t.touched.contains("e"));
        // Tapping a link of the path goes back to it.
        t.step(&b, "b", 0);
        t.step(&b, "a", 0);
        assert_eq!(t.links, ["a"]);
    }

    #[test]
    fn fold_and_toggle() {
        let b = board();
        let mut t = Trail::new(&b, vec!["a".into()], &[], 0, Layout::Map);
        t.fold(&b);
        settle_all(&mut t);
        assert!(t.open.is_empty());
        assert_eq!(at(&t, &b, "b"), (3050.0, 25.0));
        t.toggle(&b);
        assert_eq!(t.open.len(), 3);
        t.toggle(&b);
        assert!(t.open.is_empty());
    }

    #[test]
    fn deleted_leave_new_come_out_beside_current() {
        let mut b = board();
        let mut t = Trail::new(&b, vec!["a".into()], &[], 0, Layout::Map);
        t.step(&b, "b", 0);
        settle_all(&mut t);
        let bo = t.target_of(&b, "b");
        assert!(bo.0 < -1000.0, "b came towards a: {bo:?}");
        // Tab-like: a new concept beside b where it lies on the board.
        b.add_node(node("n1", 3200.0, 0.0));
        b.add_edge("b", "n1", Kind::Link);
        // Click-like: a new concept beside b where it is drawn.
        let drawn = t.display(&b).node("b").clone();
        b.add_node(node("n2", drawn.x, drawn.y + 200.0));
        let moves = t.sync(&b);
        assert_eq!(moves, vec![("n2".to_string(), (-bo.0, -bo.1))]);
        for (id, o) in moves {
            b.move_tree(&id, o.0, o.1);
        }
        settle_all(&mut t);
        assert!(t.touched.contains("n1") && t.touched.contains("n2"));
        // Both lie beside b on the board and are drawn beside drawn b.
        assert!((b.node("n2").x - 3000.0).abs() < 1.0);
        let d = t.display(&b);
        assert!((d.node("n2").x - drawn.x).abs() < 1.0);
        assert!((d.node("n1").x - (3200.0 + bo.0)).abs() < 1.0);
        // b is deleted: the path falls back to a.
        b.delete(&["b".to_string()], false);
        t.sync(&b);
        assert_eq!(t.links, ["a"]);
    }

    #[test]
    fn nested_relatives_stay_put() {
        let mut b = Board::new();
        b.add_node(Node { w: 1000.0, h: 1000.0, ..node("p", -500.0, -500.0) });
        b.add_node(node("a", 0.0, 0.0));
        b.add_node(node("x", 5000.0, 0.0));
        b.set_parent("a", Some("p"));
        b.add_edge("a", "x", Kind::Link);
        let mut t = Trail::new(&b, vec!["a".into()], &[], 0, Layout::Map);
        settle_all(&mut t);
        assert!(t.open.contains(&"p".to_string()));
        assert_eq!(t.own("p"), None);
        assert!(t.own("x").is_some());
    }

    fn heads(b: &mut Board, from: &str, to: &str) {
        let k = b.edge_between(from, to).unwrap();
        b.edges[k].arrow_to = true;
    }

    #[test]
    fn near_draws_all_at_one_scale_on_their_side() {
        let mut b = board();
        // b is three times bigger, text and all.
        let n = b.get_mut("b").unwrap();
        (n.w, n.h, n.font) = (300.0, 150.0, 60.0);
        let mut t = Trail::new(&b, vec!["a".into()], &[], 0, Layout::Near);
        settle_all(&mut t);
        let d = t.display(&b);
        let (na, nb) = (d.node("a"), d.node("b"));
        assert_eq!((nb.w, nb.h, nb.font), (100.0, 50.0, 20.0));
        assert_eq!((na.w, na.font), (100.0, 20.0));
        assert!(nb.x > na.x + na.w && nb.x < 1000.0, "b right of a and close: {}", nb.x);
        assert!(d.node("c").x < na.x, "c on its own side");
        // The board keeps its sizes.
        assert_eq!(b.node("b").w, 300.0);
    }

    #[test]
    fn trees_put_incoming_before_and_outgoing_after() {
        let mut b = board();
        heads(&mut b, "c", "a"); // c -> a: comes before a
        heads(&mut b, "a", "b"); // a -> b: after
        // a - d without heads: after, as a continuation.
        let mut t = Trail::new(&b, vec!["a".into()], &[], 0, Layout::Right);
        settle_all(&mut t);
        let d = t.display(&b);
        let a = d.node("a").bbox();
        assert!(d.node("c").bbox()[2] < a[0], "c left of a");
        assert!(d.node("b").x > a[2] && d.node("d").x > a[2], "b and d right of a");
        // Ordered as on the board: b (y 0) above d (y 3000).
        assert!(d.node("b").y < d.node("d").y);
        t.set_layout(&b, Layout::Down);
        settle_all(&mut t);
        let d = t.display(&b);
        let a = d.node("a").bbox();
        assert!(d.node("c").bbox()[3] < a[1], "c above a");
        assert!(d.node("b").y > a[3] && d.node("d").y > a[3], "b and d below a");
        assert!(d.node("b").x != d.node("d").x, "side by side");
    }

    /// p is a big frame holding x and y; a is linked to p and nested in q.
    fn framed() -> Board {
        let mut b = Board::new();
        b.add_node(Node { w: 4000.0, h: 3000.0, font: 80.0, ..node("p", 5000.0, 0.0) });
        b.add_node(node("x", 5500.0, 500.0));
        b.add_node(node("y", 6500.0, 500.0));
        b.set_parent("x", Some("p"));
        b.set_parent("y", Some("p"));
        b.add_node(Node { w: 1000.0, h: 1000.0, ..node("q", -500.0, -500.0) });
        b.add_node(node("a", 0.0, 0.0));
        b.set_parent("a", Some("q"));
        b.add_edge("a", "p", Kind::Link);
        b
    }

    #[test]
    fn a_frame_comes_out_as_a_card_and_opens_when_stepped_into() {
        let b = framed();
        let mut t = Trail::new(&b, vec!["a".into()], &[], 0, Layout::Near);
        settle_all(&mut t);
        let d = t.display(&b);
        let p = d.node("p");
        assert_eq!(p.font, 20.0);
        assert!(p.w <= 14.0 * 20.0 && p.h < 100.0, "compact card: {} x {}", p.w, p.h);
        // What it holds stays home, dimmed, and is no longer inside it.
        assert_eq!(d.node("x").x, 5500.0);
        assert!(d.parent("x").is_none() && !t.bright(&b).contains("x"));
        // The parent of a is out above it, nesting drawn as an arrow.
        assert!(t.open.contains(&"q".to_string()));
        assert!(d.node("q").bbox()[3] <= d.node("a").y);
        let k = d.edge_between("q", "a").unwrap();
        assert!(d.edges[k].kind == Kind::Link && d.edges[k].arrow_to && d.edges[k].from == "q");
        // Into the frame: its children come out beside it.
        t.step(&b, "p", 0);
        settle_all(&mut t);
        let mut open = t.open.clone();
        open.sort();
        assert_eq!(open, ["x", "y"]);
        let d = t.display(&b);
        assert!((d.node("x").x - 5500.0).abs() > 100.0, "x came out");
        assert_eq!(d.node("x").font, 20.0);
        assert!(d.edge_between("p", "x").is_some_and(|k| d.edges[k].kind == Kind::Link));
    }

    #[test]
    fn layouts_cycle_without_touching_the_board() {
        let b = framed();
        let before = b.serialize();
        let mut t = Trail::new(&b, vec!["a".into()], &[], 0, Layout::Near);
        t.step(&b, "p", 0);
        let mut l = t.layout;
        for _ in 0..4 {
            l = l.next();
            t.set_layout(&b, l);
            settle_all(&mut t);
            assert_eq!(t.links, ["a", "p"]);
        }
        assert_eq!(l, Layout::Near);
        // Back on the map nothing is resized.
        t.set_layout(&b, Layout::Map);
        settle_all(&mut t);
        let d = t.display(&b);
        assert_eq!(d.node("p").w, 4000.0);
        assert_eq!(b.serialize(), before);
    }

    #[test]
    fn what_was_looked_at_stays_whichever_way_the_walk_goes() {
        let b = board();
        for layout in [Layout::Map, Layout::Near, Layout::Right] {
            let mut t = Trail::new(&b, vec!["a".into()], &[], 0, layout);
            t.step(&b, "b", 0);
            t.step(&b, "e", 0);
            settle_all(&mut t);
            let (pb, pe) = (at(&t, &b, "b"), at(&t, &b, "e"));
            // Back to a (a tap on the first link), then another way.
            t.back_to(&b, 0);
            t.step(&b, "c", 0);
            settle_all(&mut t);
            assert_eq!(at(&t, &b, "b"), pb, "{layout:?}: b stays where it was");
            assert_eq!(at(&t, &b, "e"), pe, "{layout:?}: e stays too");
            assert!(t.bright(&b).contains("e"));
            // Untouched d slid back home.
            assert_eq!(at(&t, &b, "d"), (50.0, 3025.0), "{layout:?}");
            // Closed by hand: home.
            assert!(t.close(&b, "e"));
            settle_all(&mut t);
            assert_eq!(at(&t, &b, "e"), (6050.0, 25.0));
            assert!(!t.close(&b, "c"), "the current one cannot be closed");
            // A closed neighbour of the current one stays home too.
            t.back_to(&b, 0);
            assert!(t.close(&b, "b"));
            t.sync(&b);
            settle_all(&mut t);
            assert!(!t.out().contains("b"));
            assert_eq!(at(&t, &b, "b"), (3050.0, 25.0));
        }
    }

    #[test]
    fn kept_come_back_on_the_next_walk() {
        let b = board();
        let mut t = Trail::new(&b, vec!["a".into()], &[], 0, Layout::Near);
        t.drag("d", (500.0, -2000.0));
        t.touch("d");
        let kept = t.kept();
        let mut t2 = Trail::new(&b, t.links.clone(), &kept, 0, Layout::Near);
        settle_all(&mut t2);
        assert_eq!(t2.target_of(&b, "d"), (500.0, -2000.0));
    }

    #[test]
    fn a_concept_made_in_the_walk_lands_beside_its_anchor() {
        let mut b = framed();
        let mut t = Trail::new(&b, vec!["a".into()], &[], 0, Layout::Near);
        t.step(&b, "p", 0);
        settle_all(&mut t);
        // Made from the frame p (drawn as a small card), dropped right of it.
        let (pb, _) = t.drawn(&b, "p").unwrap();
        b.add_node(node("n", 0.0, 0.0));
        b.add_edge("p", "n", Kind::Link);
        let drop = (pb[2] + 150.0, (pb[1] + pb[3]) * 0.5);
        t.adopt(&b, "n", "p", Some(drop));
        settle_all(&mut t);
        let (nb, nf) = t.drawn(&b, "n").unwrap();
        assert!(((nb[0] + nb[2]) * 0.5 - drop.0).abs() < 1.0, "drawn where dropped");
        assert_eq!(nf, 20.0);
        let (x, y, w, _, f) = t.home_for(&b, "n").unwrap();
        // On the board: right of the whole frame, not on top of it.
        assert!(x > 9000.0, "x {x}");
        assert!(y > 0.0 && y < 3000.0, "y {y}");
        // At the frame's scale: p's title is 80, drawn at 20.
        assert_eq!(f, 80.0);
        assert!((w - (nb[2] - nb[0]) * 4.0).abs() < 1.0);
        // Neighbours that were in the way moved.
        for o in &t.open {
            let ob = t.target_box(&b, o).unwrap();
            assert!(!overlaps(&ob, &nb), "{o} overlaps the new one");
        }
    }
}
