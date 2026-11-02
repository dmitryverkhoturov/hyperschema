//! The board: concepts (nodes) and connections (edges) in our own JSON
//! format. Nesting is an edge too (`kind: "contains"`, parent -> child):
//! a hidden connection that turns into a visible one when the child
//! leaves its parent. Field names follow JSON Canvas where they can.
//!
//! Coordinates are absolute world units; a parent's children lie inside
//! its box, and moving or scaling the parent carries them along.

use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};

pub type Bbox = [f32; 4];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Shape {
    Box,
    Ellipse,
    Diamond,
    /// Free text, no outline.
    Text,
}

impl Shape {
    fn name(self) -> &'static str {
        match self {
            Shape::Box => "box",
            Shape::Ellipse => "ellipse",
            Shape::Diamond => "diamond",
            Shape::Text => "text",
        }
    }
    fn parse(s: &str) -> Shape {
        match s {
            "ellipse" => Shape::Ellipse,
            "diamond" => Shape::Diamond,
            "text" => Shape::Text,
            _ => Shape::Box,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    pub id: String,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub text: String,
    /// Font size in world units; scales with the concept.
    pub font: f32,
    pub shape: Shape,
    pub color: Option<String>,
    pub fill: Option<String>,
    /// Fields we do not understand, kept as they were.
    pub extra: Map<String, Value>,
}

impl Node {
    pub fn bbox(&self) -> Bbox {
        [self.x, self.y, self.x + self.w, self.y + self.h]
    }
    pub fn center(&self) -> (f32, f32) {
        (self.x + self.w * 0.5, self.y + self.h * 0.5)
    }
    pub fn pad(&self) -> f32 {
        self.font * 0.5
    }
    /// Margin kept around a parent's content: half a line of its font.
    pub fn gap(&self) -> f32 {
        self.font * 0.5
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Link,
    /// from = parent, to = child.
    Contains,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Edge {
    pub id: String,
    pub from: String,
    pub to: String,
    pub kind: Kind,
    pub arrow_from: bool,
    pub arrow_to: bool,
    pub label: String,
    pub extra: Map<String, Value>,
}

#[derive(Default, Clone)]
pub struct Board {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    /// Top-level fields we keep untouched (dimensions and anything newer).
    pub extra: Map<String, Value>,
    idx: HashMap<String, usize>,
    parent: HashMap<String, String>,
    children: HashMap<String, Vec<String>>,
    rng: u64,
}

fn take_f(m: &mut Map<String, Value>, k: &str, d: f32) -> f32 {
    m.remove(k).and_then(|v| v.as_f64()).map_or(d, |v| v as f32)
}
fn take_s(m: &mut Map<String, Value>, k: &str) -> Option<String> {
    m.remove(k).and_then(|v| v.as_str().map(String::from))
}

pub fn node_from_json(v: &Value) -> Option<Node> {
    let mut m = v.as_object()?.clone();
    let id = take_s(&mut m, "id")?;
    m.remove("type");
    Some(Node {
        id,
        x: take_f(&mut m, "x", 0.0),
        y: take_f(&mut m, "y", 0.0),
        w: take_f(&mut m, "width", 160.0),
        h: take_f(&mut m, "height", 80.0),
        text: take_s(&mut m, "text").unwrap_or_default(),
        font: take_f(&mut m, "fontSize", 20.0),
        shape: Shape::parse(&take_s(&mut m, "shape").unwrap_or_default()),
        color: take_s(&mut m, "color"),
        fill: take_s(&mut m, "fill"),
        extra: m,
    })
}

pub fn node_to_json(n: &Node) -> Value {
    let mut m = Map::new();
    m.insert("id".into(), json!(n.id));
    m.insert("type".into(), json!("text"));
    m.insert("x".into(), json!(round(n.x)));
    m.insert("y".into(), json!(round(n.y)));
    m.insert("width".into(), json!(round(n.w)));
    m.insert("height".into(), json!(round(n.h)));
    m.insert("text".into(), json!(n.text));
    m.insert("fontSize".into(), json!(round(n.font)));
    if n.shape != Shape::Box {
        m.insert("shape".into(), json!(n.shape.name()));
    }
    if let Some(c) = &n.color {
        m.insert("color".into(), json!(c));
    }
    if let Some(c) = &n.fill {
        m.insert("fill".into(), json!(c));
    }
    for (k, v) in &n.extra {
        m.insert(k.clone(), v.clone());
    }
    Value::Object(m)
}

pub fn edge_from_json(v: &Value) -> Option<Edge> {
    let mut m = v.as_object()?.clone();
    let end = |m: &mut Map<String, Value>, k: &str| take_s(m, k).as_deref() == Some("arrow");
    Some(Edge {
        id: take_s(&mut m, "id")?,
        from: take_s(&mut m, "fromNode")?,
        to: take_s(&mut m, "toNode")?,
        kind: if take_s(&mut m, "kind").as_deref() == Some("contains") { Kind::Contains } else { Kind::Link },
        arrow_from: end(&mut m, "fromEnd"),
        arrow_to: end(&mut m, "toEnd"),
        label: take_s(&mut m, "label").unwrap_or_default(),
        extra: m,
    })
}

pub fn edge_to_json(e: &Edge) -> Value {
    let mut m = Map::new();
    m.insert("id".into(), json!(e.id));
    m.insert("fromNode".into(), json!(e.from));
    m.insert("toNode".into(), json!(e.to));
    if e.kind == Kind::Contains {
        m.insert("kind".into(), json!("contains"));
    }
    m.insert("fromEnd".into(), json!(if e.arrow_from { "arrow" } else { "none" }));
    m.insert("toEnd".into(), json!(if e.arrow_to { "arrow" } else { "none" }));
    if !e.label.is_empty() {
        m.insert("label".into(), json!(e.label));
    }
    for (k, v) in &e.extra {
        m.insert(k.clone(), v.clone());
    }
    Value::Object(m)
}

/// Two decimals are plenty and keep files and diffs small.
fn round(v: f32) -> f64 {
    (v as f64 * 100.0).round() / 100.0
}

pub fn contains(outer: &Bbox, inner: &Bbox) -> bool {
    inner[0] >= outer[0] && inner[1] >= outer[1] && inner[2] <= outer[2] && inner[3] <= outer[3]
}

pub fn overlaps(a: &Bbox, b: &Bbox) -> bool {
    a[0] < b[2] && a[2] > b[0] && a[1] < b[3] && a[3] > b[1]
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

impl Board {
    pub fn new() -> Board {
        let mut b = Board { rng: now_ms() | 1, ..Default::default() };
        b.extra.insert("dimensions".into(), json!([{ "id": "main", "name": "Main", "nest": "contains" }]));
        b
    }

    pub fn parse(text: &str) -> Result<Board, String> {
        let v: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
        let mut m = v.as_object().ok_or("not a JSON object")?.clone();
        let nodes = m.remove("nodes").and_then(|v| v.as_array().cloned()).unwrap_or_default();
        let edges = m.remove("edges").and_then(|v| v.as_array().cloned()).unwrap_or_default();
        let mut b = Board { rng: now_ms() | 1, extra: m, ..Default::default() };
        b.nodes = nodes.iter().filter_map(node_from_json).collect();
        b.edges = edges.iter().filter_map(edge_from_json).collect();
        b.reindex();
        Ok(b)
    }

    pub fn serialize(&self) -> String {
        let mut m = Map::new();
        m.insert("hyperschema".into(), json!(1));
        m.insert("nodes".into(), Value::Array(self.nodes.iter().map(node_to_json).collect()));
        m.insert("edges".into(), Value::Array(self.edges.iter().map(edge_to_json).collect()));
        for (k, v) in &self.extra {
            if k != "hyperschema" {
                m.insert(k.clone(), v.clone());
            }
        }
        // One element per line: readable, and line-based sync merges stay sane.
        let mut s = String::from("{\n");
        let n = m.len();
        for (i, (k, v)) in m.iter().enumerate() {
            s.push_str(&format!("  {}: ", json!(k)));
            match v {
                Value::Array(a) if k == "nodes" || k == "edges" => {
                    s.push_str("[\n");
                    for (j, e) in a.iter().enumerate() {
                        s.push_str("    ");
                        s.push_str(&e.to_string());
                        s.push_str(if j + 1 < a.len() { ",\n" } else { "\n" });
                    }
                    s.push_str("  ]");
                }
                _ => s.push_str(&v.to_string()),
            }
            s.push_str(if i + 1 < n { ",\n" } else { "\n" });
        }
        s.push_str("}\n");
        s
    }

    /// Rebuilds lookups after any structural change.
    pub fn reindex(&mut self) {
        self.idx = self.nodes.iter().enumerate().map(|(i, n)| (n.id.clone(), i)).collect();
        let idx = &self.idx;
        self.edges.retain(|e| idx.contains_key(&e.from) && idx.contains_key(&e.to) && e.from != e.to);
        self.parent.clear();
        self.children.clear();
        for e in &self.edges {
            if e.kind == Kind::Contains && !self.parent.contains_key(&e.to) {
                self.parent.insert(e.to.clone(), e.from.clone());
            }
        }
        // Break accidental cycles.
        let ids: Vec<String> = self.parent.keys().cloned().collect();
        for id in ids {
            let mut seen = HashSet::new();
            let mut cur = id.clone();
            while let Some(p) = self.parent.get(&cur).cloned() {
                if !seen.insert(cur.clone()) || p == id {
                    self.parent.remove(&id);
                    break;
                }
                cur = p;
            }
        }
        for n in &self.nodes {
            if let Some(p) = self.parent.get(&n.id) {
                self.children.entry(p.clone()).or_default().push(n.id.clone());
            }
        }
    }

    pub fn new_id(&mut self) -> String {
        const A: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
        loop {
            let id: String = (0..10)
                .map(|_| {
                    self.rng ^= self.rng << 13;
                    self.rng ^= self.rng >> 7;
                    self.rng ^= self.rng << 17;
                    A[(self.rng % A.len() as u64) as usize] as char
                })
                .collect();
            if !self.idx.contains_key(&id) && !self.edges.iter().any(|e| e.id == id) {
                return id;
            }
        }
    }

    // ------------------------------------------------------------ queries

    pub fn get(&self, id: &str) -> Option<&Node> {
        self.idx.get(id).map(|&i| &self.nodes[i])
    }
    pub fn get_mut(&mut self, id: &str) -> Option<&mut Node> {
        self.idx.get(id).map(|&i| &mut self.nodes[i])
    }
    pub fn node(&self, id: &str) -> &Node {
        &self.nodes[self.idx[id]]
    }
    pub fn edge(&self, id: &str) -> Option<&Edge> {
        self.edges.iter().find(|e| e.id == id)
    }
    pub fn edge_mut(&mut self, id: &str) -> Option<&mut Edge> {
        self.edges.iter_mut().find(|e| e.id == id)
    }
    pub fn parent(&self, id: &str) -> Option<&str> {
        self.parent.get(id).map(String::as_str)
    }
    pub fn children(&self, id: &str) -> &[String] {
        self.children.get(id).map_or(&[], Vec::as_slice)
    }
    pub fn has_children(&self, id: &str) -> bool {
        !self.children(id).is_empty()
    }
    /// Top-level concepts, borrowed.
    pub fn roots_ref(&self) -> Vec<&str> {
        self.nodes.iter().filter(|n| !self.parent.contains_key(&n.id)).map(|n| n.id.as_str()).collect()
    }

    pub fn roots(&self) -> Vec<String> {
        self.nodes.iter().filter(|n| !self.parent.contains_key(&n.id)).map(|n| n.id.clone()).collect()
    }
    /// `id` and everything nested in it, parents before children.
    pub fn subtree(&self, id: &str) -> Vec<String> {
        let mut out = vec![id.to_string()];
        let mut i = 0;
        while i < out.len() {
            out.extend(self.children(&out[i]).iter().cloned());
            i += 1;
        }
        out
    }
    pub fn ancestors(&self, id: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut cur = id;
        while let Some(p) = self.parent(cur) {
            out.push(p.to_string());
            cur = p;
        }
        out
    }
    pub fn is_ancestor(&self, a: &str, of: &str) -> bool {
        self.ancestors(of).iter().any(|x| x == a)
    }
    pub fn depth(&self, id: &str) -> usize {
        self.ancestors(id).len()
    }
    /// Draw order: parents before children, siblings in file order.
    pub fn draw_order(&self) -> Vec<String> {
        let mut out = Vec::with_capacity(self.nodes.len());
        let mut stack: Vec<String> = self.roots().into_iter().rev().collect();
        while let Some(id) = stack.pop() {
            stack.extend(self.children(&id).iter().rev().cloned());
            out.push(id);
        }
        out
    }
    /// Height of the title strip of a parent (single line of its font + padding).
    pub fn title_h(&self, n: &Node, lines: usize) -> f32 {
        lines.max(1) as f32 * n.font * crate::text::LINE_H + n.pad() * 2.0
    }
    /// The area children may occupy.
    pub fn content_area(&self, n: &Node, title_lines: usize) -> Bbox {
        let g = n.gap();
        [n.x + g, n.y + self.title_h(n, title_lines), n.x + n.w - g, n.y + n.h - g]
    }
    /// Union of the boxes of `ids`.
    pub fn bounds_of<'a>(&self, ids: impl IntoIterator<Item = &'a String>) -> Option<Bbox> {
        let mut b: Option<Bbox> = None;
        for id in ids {
            let nb = self.node(id).bbox();
            b = Some(match b {
                None => nb,
                Some(o) => [o[0].min(nb[0]), o[1].min(nb[1]), o[2].max(nb[2]), o[3].max(nb[3])],
            });
        }
        b
    }
    pub fn bounds(&self) -> Bbox {
        let ids: Vec<String> = self.nodes.iter().map(|n| n.id.clone()).collect();
        self.bounds_of(&ids).unwrap_or([0.0, 0.0, 1000.0, 1000.0])
    }

    /// Smallest node containing (x, y), skipping `skip` and their subtrees.
    pub fn deepest_at(&self, x: f32, y: f32, skip: &HashSet<String>) -> Option<String> {
        let mut best: Option<(f32, &str)> = None;
        for n in &self.nodes {
            if n.shape == Shape::Text || skip.contains(&n.id) || self.ancestors(&n.id).iter().any(|a| skip.contains(a)) {
                continue;
            }
            let b = n.bbox();
            if x >= b[0] && x <= b[2] && y >= b[1] && y <= b[3] {
                let area = n.w * n.h;
                if best.map_or(true, |(a, _)| area < a) {
                    best = Some((area, &n.id));
                }
            }
        }
        best.map(|b| b.1.to_string())
    }

    // ------------------------------------------------------------ edits

    pub fn add_node(&mut self, n: Node) {
        self.nodes.push(n);
        self.reindex();
    }

    /// Adds another board's concepts and connections, shifted by (dx, dy),
    /// under fresh ids so nothing here is replaced. Returns the new ids of
    /// its top-level concepts.
    pub fn merge(&mut self, other: Board, dx: f32, dy: f32) -> Vec<String> {
        let roots: HashSet<String> = other.roots().into_iter().collect();
        let mut map: HashMap<String, String> = HashMap::new();
        let mut new_roots = Vec::new();
        for mut n in other.nodes {
            let id = self.new_id();
            if roots.contains(&n.id) {
                new_roots.push(id.clone());
            }
            map.insert(std::mem::replace(&mut n.id, id), n.id.clone());
            n.x += dx;
            n.y += dy;
            self.idx.insert(n.id.clone(), usize::MAX);
            self.nodes.push(n);
        }
        for mut e in other.edges {
            let (Some(f), Some(t)) = (map.get(&e.from), map.get(&e.to)) else { continue };
            e.from = f.clone();
            e.to = t.clone();
            e.id = self.new_id();
            self.edges.push(e);
        }
        self.reindex();
        new_roots
    }

    pub fn add_edge(&mut self, from: &str, to: &str, kind: Kind) -> String {
        let id = self.new_id();
        self.edges.push(Edge {
            id: id.clone(),
            from: from.into(),
            to: to.into(),
            kind,
            arrow_from: false,
            arrow_to: false,
            label: String::new(),
            extra: Map::new(),
        });
        self.reindex();
        id
    }

    /// An existing edge between a and b in either direction.
    pub fn edge_between(&self, a: &str, b: &str) -> Option<usize> {
        self.edges.iter().position(|e| (e.from == a && e.to == b) || (e.from == b && e.to == a))
    }

    /// Moves `id` (with its subtree) under `parent` (None = top level).
    /// Leaving a parent leaves a visible link behind; an explicit link to
    /// the new parent is absorbed into the nesting.
    pub fn set_parent(&mut self, id: &str, parent: Option<&str>) {
        let old = self.parent(id).map(String::from);
        if old.as_deref() == parent {
            return;
        }
        if let Some(p) = parent {
            if p == id || self.is_ancestor(id, p) {
                return;
            }
        }
        if let Some(o) = &old {
            if let Some(e) = self.edges.iter_mut().find(|e| e.kind == Kind::Contains && &e.from == o && e.to == id) {
                e.kind = Kind::Link;
            }
        }
        if let Some(p) = parent {
            match self.edges.iter().position(|e| e.kind == Kind::Link && ((e.from == p && e.to == id) || (e.from == id && e.to == p))) {
                Some(k) => {
                    let e = &mut self.edges[k];
                    // Keep label and arrows; nesting reads parent -> child.
                    if e.from != p {
                        std::mem::swap(&mut e.from, &mut e.to);
                        std::mem::swap(&mut e.arrow_from, &mut e.arrow_to);
                    }
                    e.kind = Kind::Contains;
                }
                None => {
                    self.add_edge(p, id, Kind::Contains);
                }
            }
        }
        self.reindex();
    }

    pub fn move_tree(&mut self, id: &str, dx: f32, dy: f32) {
        for k in self.subtree(id) {
            let n = self.get_mut(&k).unwrap();
            n.x += dx;
            n.y += dy;
        }
    }

    /// Scales the subtree of `id` by `k` about (ax, ay); fonts scale too.
    pub fn scale_tree(&mut self, id: &str, k: f32, ax: f32, ay: f32) {
        for c in self.subtree(id) {
            let n = self.get_mut(&c).unwrap();
            n.x = ax + (n.x - ax) * k;
            n.y = ay + (n.y - ay) * k;
            n.w *= k;
            n.h *= k;
            n.font *= k;
        }
    }

    /// Scales only what is inside `id`.
    pub fn scale_children(&mut self, id: &str, k: f32, ax: f32, ay: f32) {
        for c in self.children(id).to_vec() {
            self.scale_tree(&c, k, ax, ay);
        }
    }

    /// Grows every ancestor of `id` so the child fits inside its content area.
    pub fn fit_ancestors(&mut self, id: &str, title_lines: &dyn Fn(&Node) -> usize) {
        let mut cur = id.to_string();
        while let Some(p) = self.parent(&cur).map(String::from) {
            let cb = self.node(&cur).bbox();
            let pn = self.node(&p).clone();
            let area = self.content_area(&pn, title_lines(&pn));
            let n = self.get_mut(&p).unwrap();
            if cb[0] < area[0] {
                let d = area[0] - cb[0];
                n.x -= d;
                n.w += d;
            }
            if cb[1] < area[1] {
                let d = area[1] - cb[1];
                n.y -= d;
                n.h += d;
            }
            if cb[2] > area[2] {
                n.w += cb[2] - area[2];
            }
            if cb[3] > area[3] {
                n.h += cb[3] - area[3];
            }
            cur = p;
        }
    }

    /// Deletes nodes. Without `deep`, children move up a level.
    pub fn delete(&mut self, ids: &[String], deep: bool) {
        let mut gone: HashSet<String> = HashSet::new();
        for id in ids {
            if deep {
                gone.extend(self.subtree(id));
            } else {
                gone.insert(id.clone());
            }
        }
        if !deep {
            for id in ids {
                let up = self.parent(id).map(String::from);
                // Climb past ancestors that are being deleted too.
                let mut up = up;
                while let Some(u) = up.clone() {
                    if !gone.contains(&u) {
                        break;
                    }
                    up = self.parent(&u).map(String::from);
                }
                for c in self.children(id).to_vec() {
                    if gone.contains(&c) {
                        continue;
                    }
                    self.edges.retain(|e| !(e.kind == Kind::Contains && e.to == c));
                    if let Some(u) = &up {
                        let eid = self.new_id();
                        self.edges.push(Edge {
                            id: eid,
                            from: u.clone(),
                            to: c.clone(),
                            kind: Kind::Contains,
                            arrow_from: false,
                            arrow_to: false,
                            label: String::new(),
                            extra: Map::new(),
                        });
                    }
                }
            }
        }
        self.nodes.retain(|n| !gone.contains(&n.id));
        self.edges.retain(|e| !gone.contains(&e.from) && !gone.contains(&e.to));
        self.reindex();
    }

    pub fn delete_edge(&mut self, id: &str) {
        self.edges.retain(|e| e.id != id);
        self.reindex();
    }

    /// Pushes `id` out of siblings it half-overlaps (nesting is the only
    /// legal overlap).
    pub fn resolve_overlap(&mut self, id: &str) {
        let parent = self.parent(id).map(String::from);
        for _ in 0..12 {
            let me = self.node(id).clone();
            let mb = me.bbox();
            let gap = me.font * 0.75;
            let hit = self
                .nodes
                .iter()
                .filter(|n| n.id != me.id && self.parent(&n.id).map(String::from) == parent && n.shape != Shape::Text && me.shape != Shape::Text)
                .find(|n| overlaps(&mb, &n.bbox()))
                .map(|n| n.bbox());
            let Some(o) = hit else { return };
            // Smallest push that clears it.
            let pushes = [(o[2] + gap - mb[0], 0.0), (o[0] - gap - mb[2], 0.0), (0.0, o[3] + gap - mb[1]), (0.0, o[1] - gap - mb[3])];
            let (dx, dy) = pushes.iter().copied().min_by(|a, b| (a.0.abs() + a.1.abs()).partial_cmp(&(b.0.abs() + b.1.abs())).unwrap()).unwrap();
            self.move_tree(id, dx, dy);
        }
    }

    // ------------------------------------------------------------ diffs

    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            nodes: self.nodes.iter().map(|n| (n.id.clone(), node_to_json(n))).collect(),
            edges: self.edges.iter().map(|e| (e.id.clone(), edge_to_json(e))).collect(),
        }
    }

    /// What changed since `before`; None if nothing did.
    pub fn diff(&self, before: &Snapshot) -> Option<Change> {
        let after = self.snapshot();
        let side = |b: &Vec<(String, Value)>, a: &Vec<(String, Value)>| {
            let bm: HashMap<&String, &Value> = b.iter().map(|(k, v)| (k, v)).collect();
            let am: HashMap<&String, &Value> = a.iter().map(|(k, v)| (k, v)).collect();
            let mut out = Vec::new();
            for (k, v) in b {
                match am.get(k) {
                    Some(nv) if *nv == v => {}
                    Some(nv) => out.push((k.clone(), Some(v.clone()), Some((*nv).clone()))),
                    None => out.push((k.clone(), Some(v.clone()), None)),
                }
            }
            for (k, v) in a {
                if !bm.contains_key(k) {
                    out.push((k.clone(), None, Some(v.clone())));
                }
            }
            out
        };
        let c = Change { t: now_ms(), dev: String::new(), nodes: side(&before.nodes, &after.nodes), edges: side(&before.edges, &after.edges) };
        (!c.nodes.is_empty() || !c.edges.is_empty()).then_some(c)
    }

    /// Applies one side of a change: `forward` takes `after`, else `before`.
    pub fn apply(&mut self, c: &Change, forward: bool) {
        for (id, b, a) in &c.nodes {
            let v = if forward { a } else { b };
            match (v, self.idx.get(id).copied()) {
                (Some(v), Some(i)) => {
                    if let Some(n) = node_from_json(v) {
                        self.nodes[i] = n;
                    }
                }
                (Some(v), None) => {
                    if let Some(n) = node_from_json(v) {
                        self.idx.insert(n.id.clone(), self.nodes.len());
                        self.nodes.push(n);
                    }
                }
                (None, Some(_)) => {
                    self.nodes.retain(|n| &n.id != id);
                    self.idx = self.nodes.iter().enumerate().map(|(i, n)| (n.id.clone(), i)).collect();
                }
                (None, None) => {}
            }
        }
        for (id, b, a) in &c.edges {
            let v = if forward { a } else { b };
            let pos = self.edges.iter().position(|e| &e.id == id);
            match (v.as_ref().and_then(edge_from_json), pos) {
                (Some(e), Some(i)) => self.edges[i] = e,
                (Some(e), None) => self.edges.push(e),
                (None, Some(i)) => {
                    self.edges.remove(i);
                }
                (None, None) => {}
            }
        }
        self.reindex();
    }
}

#[derive(Clone, Default)]
pub struct Snapshot {
    nodes: Vec<(String, Value)>,
    edges: Vec<(String, Value)>,
}

/// One step of history: per element its JSON before and after
/// (None = did not exist). Both sides are kept so a step can be walked
/// either way and neighbouring steps can be merged.
#[derive(Clone, Debug, PartialEq)]
pub struct Change {
    pub t: u64,
    pub dev: String,
    pub nodes: Vec<(String, Option<Value>, Option<Value>)>,
    pub edges: Vec<(String, Option<Value>, Option<Value>)>,
}

impl Change {
    /// `self` then `next` as one step.
    pub fn then(&self, next: &Change) -> Change {
        let merge = |a: &Vec<(String, Option<Value>, Option<Value>)>, b: &Vec<(String, Option<Value>, Option<Value>)>| {
            let mut out: Vec<(String, Option<Value>, Option<Value>)> = a.clone();
            for (id, bb, ba) in b {
                match out.iter_mut().find(|x| &x.0 == id) {
                    Some(x) => x.2 = ba.clone(),
                    None => out.push((id.clone(), bb.clone(), ba.clone())),
                }
            }
            out.retain(|x| x.1 != x.2);
            out
        };
        Change { t: next.t, dev: next.dev.clone(), nodes: merge(&self.nodes, &next.nodes), edges: merge(&self.edges, &next.edges) }
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty() && self.edges.is_empty()
    }

    pub fn to_json(&self) -> Value {
        let side = |v: &Vec<(String, Option<Value>, Option<Value>)>| Value::Array(v.iter().map(|(id, b, a)| json!([id, b, a])).collect());
        json!({ "t": self.t, "dev": self.dev, "n": side(&self.nodes), "e": side(&self.edges) })
    }

    pub fn from_json(v: &Value) -> Option<Change> {
        let side = |k: &str| -> Option<Vec<(String, Option<Value>, Option<Value>)>> {
            v[k].as_array()?
                .iter()
                .map(|x| {
                    let opt = |y: &Value| if y.is_null() { None } else { Some(y.clone()) };
                    Some((x[0].as_str()?.to_string(), opt(&x[1]), opt(&x[2])))
                })
                .collect()
        };
        Some(Change { t: v["t"].as_u64()?, dev: v["dev"].as_str().unwrap_or("").to_string(), nodes: side("n")?, edges: side("e")? })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(b: &mut Board, x: f32, y: f32, w: f32, h: f32) -> String {
        let id = b.new_id();
        b.add_node(Node { id: id.clone(), x, y, w, h, text: id.clone(), font: 20.0, shape: Shape::Box, color: None, fill: None, extra: Map::new() });
        id
    }

    #[test]
    fn nesting_flips_edges() {
        let mut b = Board::new();
        let p = node(&mut b, 0.0, 0.0, 400.0, 300.0);
        let c = node(&mut b, 50.0, 80.0, 100.0, 50.0);
        let q = node(&mut b, 600.0, 0.0, 400.0, 300.0);
        b.set_parent(&c, Some(&p));
        assert_eq!(b.parent(&c), Some(p.as_str()));
        // Leaving p leaves a link; entering q nests.
        b.set_parent(&c, Some(&q));
        assert_eq!(b.parent(&c), Some(q.as_str()));
        let link = b.edge_between(&p, &c).unwrap();
        assert_eq!(b.edges[link].kind, Kind::Link);
        // Coming back absorbs that link.
        b.set_parent(&c, Some(&p));
        assert_eq!(b.edges.iter().filter(|e| e.kind == Kind::Link).count(), 1); // the one to q now
        assert_eq!(b.edges[b.edge_between(&p, &c).unwrap()].kind, Kind::Contains);
    }

    #[test]
    fn delete_parent_keeps_children() {
        let mut b = Board::new();
        let g = node(&mut b, 0.0, 0.0, 1000.0, 1000.0);
        let p = node(&mut b, 10.0, 60.0, 400.0, 300.0);
        let c = node(&mut b, 50.0, 120.0, 100.0, 50.0);
        b.set_parent(&p, Some(&g));
        b.set_parent(&c, Some(&p));
        b.delete(&[p.clone()], false);
        assert_eq!(b.parent(&c), Some(g.as_str()));
        b.delete(&[g.clone()], true);
        assert!(b.nodes.is_empty());
    }

    #[test]
    fn diff_apply_and_merge() {
        let mut b = Board::new();
        let s0 = b.snapshot();
        let a = node(&mut b, 0.0, 0.0, 10.0, 10.0);
        let c1 = b.diff(&s0).unwrap();
        let s1 = b.snapshot();
        b.get_mut(&a).unwrap().x = 50.0;
        let c2 = b.diff(&s1).unwrap();
        let both = c1.then(&c2);
        let mut fresh = Board::new();
        fresh.apply(&both, true);
        assert_eq!(fresh.node(&a).x, 50.0);
        fresh.apply(&both, false);
        assert!(fresh.nodes.is_empty());
        let back = Change::from_json(&both.to_json()).unwrap();
        assert_eq!(back, both);
        let text = b.serialize();
        let again = Board::parse(&text).unwrap();
        assert_eq!(again.nodes, b.nodes);
    }
}
