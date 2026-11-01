//! Path mode: walking the board along its connections. Neighbours of the
//! current concept slide out next to it, each on the side where it lies;
//! a tap steps to one of them. The board itself never moves: the walk keeps
//! an offset per concept moved out (its subtree follows) and the editor
//! shows a copy of the board with the offsets applied.

use crate::board::{overlaps, Bbox, Board};
use std::collections::{HashMap, HashSet};
use std::time::Instant;

pub type Off = (f32, f32);

/// How long concepts take to slide out or back.
pub const EASE_MS: f64 = 250.0;

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
    /// Moved by hand or created here: they stay out until the walk ends.
    pub touched: HashSet<String>,
    /// Where concepts are going, as offsets from their place on the board.
    off: HashMap<String, Off>,
    /// Offsets drawn now, easing toward `off`.
    shown: HashMap<String, Off>,
    from: HashMap<String, Off>,
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

fn shift(b: Bbox, o: Off) -> Bbox {
    [b[0] + o.0, b[1] + o.1, b[2] + o.0, b[3] + o.1]
}

fn grow(b: Bbox, d: f32) -> Bbox {
    [b[0] - d, b[1] - d, b[2] + d, b[3] + d]
}

impl Trail {
    pub fn new(b: &Board, links: Vec<String>, undo_len: usize) -> Trail {
        let mut t = Trail {
            marks: vec![undo_len; links.len()],
            links,
            open: Vec::new(),
            expanded: true,
            touched: HashSet::new(),
            off: HashMap::new(),
            shown: HashMap::new(),
            from: HashMap::new(),
            start: None,
            known: b.nodes.iter().map(|n| n.id.clone()).collect(),
            leaving: false,
        };
        // A remembered path: its links come back where they were laid out,
        // each beside the one before it.
        for i in 1..t.links.len() {
            let (c, n) = (t.links[i - 1].clone(), t.links[i].clone());
            let taken = t.taken(b, &n);
            t.place(b, &c, &n, &taken);
        }
        t.reopen(b);
        t.settle(b);
        t
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

    /// What is drawn bright: what is out and everything nested in it. A
    /// frame around the walk (the current concept's parent) lights up
    /// alone, not with all it holds.
    pub fn bright(&self, b: &Board) -> HashSet<String> {
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

    /// Offset of `id` where it is going: its own or its nearest moved ancestor's.
    pub fn target_of(&self, b: &Board, id: &str) -> Off {
        if let Some(o) = self.off.get(id) {
            return *o;
        }
        b.ancestors(id).iter().find_map(|a| self.off.get(a).copied()).unwrap_or((0.0, 0.0))
    }

    fn shown_of(&self, b: &Board, id: &str) -> Off {
        if let Some(o) = self.shown.get(id) {
            return *o;
        }
        b.ancestors(id).iter().find_map(|a| self.shown.get(a).copied()).unwrap_or((0.0, 0.0))
    }

    /// The board as drawn: every concept at its offset.
    pub fn display(&self, b: &Board) -> Board {
        let mut d = b.clone();
        if self.shown.is_empty() {
            return d;
        }
        let offs: Vec<Off> = d.nodes.iter().map(|n| self.shown_of(b, &n.id)).collect();
        for (n, o) in d.nodes.iter_mut().zip(offs) {
            n.x += o.0;
            n.y += o.1;
        }
        d
    }

    /// Concepts whose drawn place differs from the board (their connections
    /// need routing while they slide).
    pub fn moving(&self) -> HashSet<String> {
        self.shown.keys().chain(self.from.keys()).cloned().collect()
    }

    /// Where the other out concepts are going (what a new one must avoid);
    /// frames around the walk do not count, it lies inside them anyway.
    fn taken(&self, b: &Board, except: &str) -> Vec<Bbox> {
        let walk: Vec<&String> = self.links.iter().chain(self.touched.iter()).collect();
        self.out()
            .iter()
            .filter(|id| id.as_str() != except && !walk.iter().any(|w| b.is_ancestor(id, w)) && !b.is_ancestor(id, except))
            .filter_map(|id| b.get(id).map(|n| shift(n.bbox(), self.target_of(b, id))))
            .collect()
    }

    /// Lays `n` out beside `c`, on the side where it lies on the board.
    /// Nesting relatives stay put (the child is inside, the parent around),
    /// and so does a concept holding something else that is out.
    fn place(&mut self, b: &Board, c: &str, n: &str, taken: &[Bbox]) {
        let (Some(cn), Some(nn)) = (b.get(c), b.get(n)) else { return };
        if b.is_ancestor(c, n) || b.is_ancestor(n, c) {
            return;
        }
        let holds_out = self.links.iter().chain(self.touched.iter()).any(|o| o != n && b.is_ancestor(n, o));
        if holds_out {
            return;
        }
        let cb = shift(cn.bbox(), self.target_of(b, c));
        let nb = nn.bbox();
        let (nw, nh) = (nb[2] - nb[0], nb[3] - nb[1]);
        let (cw, ch) = (cb[2] - cb[0], cb[3] - cb[1]);
        let gap = cn.font.max(nn.font) * 2.0;
        let (hx, hy) = (nn.center().0 - cn.center().0, nn.center().1 - cn.center().1);
        let base = if hx.hypot(hy) < 1e-3 { 0.0 } else { hy.atan2(hx) };
        let step = nw.max(nh) * 0.6 + gap;
        let cc = ((cb[0] + cb[2]) * 0.5, (cb[1] + cb[3]) * 0.5);
        let mut best = None;
        'search: for ring in 0..14 {
            for da in [0.0f32, 0.3, -0.3, 0.6, -0.6, 0.9, -0.9, 1.2, -1.2] {
                let a = base + da;
                let (dx, dy) = (a.cos(), a.sin());
                // Distance along the ray where the two boxes stop overlapping.
                let ex = (cw + nw) * 0.5 + gap;
                let ey = (ch + nh) * 0.5 + gap;
                let t = (if dx.abs() > 1e-4 { ex / dx.abs() } else { f32::MAX }).min(if dy.abs() > 1e-4 { ey / dy.abs() } else { f32::MAX }) + ring as f32 * step;
                let (x, y) = (cc.0 + dx * t - nw * 0.5, cc.1 + dy * t - nh * 0.5);
                let cand = [x, y, x + nw, y + nh];
                if best.is_none() {
                    best = Some(cand);
                }
                if !taken.iter().any(|o| overlaps(o, &grow(cand, gap * 0.4))) && !overlaps(&cb, &grow(cand, gap * 0.4)) {
                    best = Some(cand);
                    break 'search;
                }
            }
        }
        if let Some(c) = best {
            self.off.insert(n.to_string(), (c[0] - nb[0], c[1] - nb[1]));
        }
    }

    /// Opens the current concept's neighbours that are not out yet.
    fn reopen(&mut self, b: &Board) {
        let out = self.out();
        self.open = neighbors(b, self.current()).into_iter().filter(|n| !out.contains(n)).collect();
        self.expanded = true;
    }

    /// Lays out what is newly out, sends back what is not, starts easing.
    fn settle(&mut self, b: &Board) {
        let out = self.out();
        self.off.retain(|id, _| out.contains(id));
        let cur = self.current().to_string();
        for n in self.open.clone() {
            if !self.off.contains_key(&n) {
                let taken = self.taken(b, &n);
                self.place(b, &cur, &n, &taken);
            }
        }
        self.ease();
    }

    fn ease(&mut self) {
        let mut from = HashMap::new();
        for id in self.shown.keys().chain(self.off.keys()) {
            let s = self.shown.get(id).copied().unwrap_or((0.0, 0.0));
            if self.off.get(id).copied().unwrap_or((0.0, 0.0)) != s {
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
            self.shown.entry(id.clone()).or_insert((0.0, 0.0));
        }
        self.start = Some(Instant::now());
    }

    /// Moves the drawn offsets along; true while something slides.
    pub fn tick(&mut self) -> bool {
        let Some(start) = self.start else { return false };
        let t = (start.elapsed().as_secs_f64() * 1000.0 / EASE_MS).min(1.0) as f32;
        let e = 1.0 - (1.0 - t).powi(3);
        for (id, f) in &self.from {
            let to = self.off.get(id).copied().unwrap_or((0.0, 0.0));
            self.shown.insert(id.clone(), (f.0 + (to.0 - f.0) * e, f.1 + (to.1 - f.1) * e));
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

    /// The walk ends: everything slides back to its place.
    pub fn leave(&mut self) {
        self.leaving = true;
        self.links.clear();
        self.open.clear();
        self.touched.clear();
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
            let taken = self.taken(b, n);
            self.place(b, &cur, n, &taken);
        }
        self.links.push(n.to_string());
        self.marks.push(undo_len);
        self.touched.remove(n);
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
        let gone: Vec<String> = self.links.drain(k + 1..).collect();
        self.marks.truncate(k + 1);
        // Links that were dragged keep their place; the rest slide back
        // unless they are neighbours of the link returned to.
        for g in gone {
            self.touched.remove(&g);
        }
        self.reopen(b);
        self.settle(b);
    }

    /// Ctrl+Z on the walk: the last step back.
    pub fn pop(&mut self, b: &Board) -> bool {
        if self.links.len() < 2 {
            return false;
        }
        let k = self.links.len() - 2;
        self.back_to(b, k);
        true
    }

    /// A tap on the current concept: folds or unfolds its neighbours.
    pub fn toggle(&mut self, b: &Board) {
        if self.expanded && !self.open.is_empty() {
            self.fold(b);
        } else {
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
        self.off.insert(id.to_string(), o);
        self.shown.insert(id.to_string(), o);
        self.from.remove(id);
    }

    /// After a drag: it stays out until the walk ends.
    pub fn touch(&mut self, id: &str) {
        if !self.links.iter().any(|l| l == id) {
            self.touched.insert(id.to_string());
        }
        self.open.retain(|o| o != id);
    }

    /// Own offset of `id`, if it was moved out itself.
    pub fn own(&self, id: &str) -> Option<Off> {
        self.off.get(id).copied()
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
                // Nested in something already out: it follows its parent.
                if b.ancestors(&id).iter().any(|a| self.off.contains_key(a) || self.links.contains(a)) {
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
                self.off.insert(id.clone(), co);
                self.shown.insert(id.clone(), co);
                self.touched.insert(id);
            }
        }
        if self.expanded {
            let out = self.out();
            for n in neighbors(b, &cur) {
                if !out.contains(&n) {
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
        let mut t = Trail::new(&b, vec!["a".into()], 0);
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
        let mut t = Trail::new(&b, vec!["a".into()], 0);
        t.step(&b, "b", 0);
        t.step(&b, "e", 0);
        settle_all(&mut t);
        assert_eq!(b.serialize(), before);
    }

    #[test]
    fn step_moves_on_and_untouched_go_back() {
        let b = board();
        let mut t = Trail::new(&b, vec!["a".into()], 0);
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
        let mut t = Trail::new(&b, vec!["a".into()], 0);
        t.drag("d", (500.0, -2000.0));
        t.touch("d");
        t.step(&b, "b", 0);
        settle_all(&mut t);
        assert_eq!(at(&t, &b, "d"), (550.0, 1025.0));
    }

    #[test]
    fn back_and_pop_shorten_the_path() {
        let b = board();
        let mut t = Trail::new(&b, vec!["a".into()], 0);
        t.step(&b, "b", 0);
        t.step(&b, "e", 0);
        assert!(t.pop(&b));
        assert_eq!(t.links, ["a", "b"]);
        t.back_to(&b, 0);
        assert_eq!(t.links, ["a"]);
        assert!(!t.pop(&b));
        // Tapping a link of the path goes back to it.
        t.step(&b, "b", 0);
        t.step(&b, "a", 0);
        assert_eq!(t.links, ["a"]);
    }

    #[test]
    fn fold_and_toggle() {
        let b = board();
        let mut t = Trail::new(&b, vec!["a".into()], 0);
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
        let mut t = Trail::new(&b, vec!["a".into()], 0);
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
        let mut t = Trail::new(&b, vec!["a".into()], 0);
        settle_all(&mut t);
        assert!(t.open.contains(&"p".to_string()));
        assert_eq!(t.own("p"), None);
        assert!(t.own("x").is_some());
    }
}
