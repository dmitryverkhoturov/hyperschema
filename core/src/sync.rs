//! A board as an Automerge document: the copy every device keeps and
//! merges. Layout of the document root:
//!
//! - `nodes`: map id -> map of the concept's JSON fields; `text` is an
//!   Automerge text (merges letter by letter), `z` is the creation order;
//! - `edges`: map id -> map of the connection's JSON fields;
//! - `extra`: the board's other top-level fields.
//!
//! Numbers, strings and bools are stored as scalars; arrays and objects
//! (rare, from foreign files) as JSON strings under `@key`.
//!
//! Local edits come in as the editor's `Change`s and touch only the fields
//! that changed, so two devices moving and retyping different things merge
//! cleanly. Remote changes are merged into the document and the board is
//! rebuilt from it.

use crate::board::{edge_from_json, edge_to_json, node_from_json, node_to_json, Board, Change};
use automerge::transaction::{CommitOptions, Transactable};
use automerge::{ActorId, AutoCommit, ChangeHash, ObjId, ObjType, ReadDoc, ScalarValue, Value as AmValue, ROOT};
use serde_json::{json, Map, Value};

pub use automerge::AutomergeError;

pub struct Shared {
    pub doc: AutoCommit,
}

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

fn scalar(v: &Value) -> Option<ScalarValue> {
    Some(match v {
        Value::Null => ScalarValue::Null,
        Value::Bool(b) => ScalarValue::Boolean(*b),
        Value::Number(n) => ScalarValue::F64(n.as_f64()?),
        Value::String(s) => ScalarValue::Str(s.as_str().into()),
        _ => return None,
    })
}

fn from_scalar(s: &ScalarValue) -> Value {
    match s {
        ScalarValue::Boolean(b) => json!(b),
        ScalarValue::F64(f) => {
            // Whole numbers come back as integers, like the file had them.
            if f.fract() == 0.0 && f.abs() < 1e15 {
                json!(*f as i64)
            } else {
                json!(f)
            }
        }
        ScalarValue::Int(i) => json!(i),
        ScalarValue::Uint(u) => json!(u),
        ScalarValue::Str(s) => json!(s.as_str()),
        _ => Value::Null,
    }
}

impl Shared {
    pub fn new(actor: Option<ActorId>) -> Shared {
        let mut doc = AutoCommit::new();
        if let Some(a) = actor {
            doc.set_actor(a);
        }
        Shared { doc }
    }

    /// A fresh document holding `board`, written as one change.
    pub fn from_board(board: &Board, actor: Option<ActorId>) -> Shared {
        let mut s = Shared::new(actor);
        s.ensure_roots();
        for (z, n) in board.nodes.iter().enumerate() {
            let mut v = node_to_json(n);
            v["z"] = json!(z);
            s.write_obj("nodes", &n.id, &v);
        }
        for e in &board.edges {
            s.write_obj("edges", &e.id, &edge_to_json(e));
        }
        let extra = s.root_map("extra");
        for (k, v) in &board.extra {
            s.put_field(&extra, k, v);
        }
        s.commit();
        s
    }

    pub fn load(bytes: &[u8], actor: Option<ActorId>) -> Result<Shared, AutomergeError> {
        let mut doc = AutoCommit::load(bytes)?;
        if let Some(a) = actor {
            doc.set_actor(a);
        }
        Ok(Shared { doc })
    }

    pub fn save(&mut self) -> Vec<u8> {
        self.doc.save()
    }

    pub fn heads(&mut self) -> Vec<ChangeHash> {
        self.doc.get_heads()
    }

    /// Everything since `heads`, as bytes another copy can merge.
    pub fn save_after(&mut self, heads: &[ChangeHash]) -> Vec<u8> {
        self.doc.save_after(heads)
    }

    /// Merges bytes from another copy; true if anything new arrived.
    pub fn merge_bytes(&mut self, bytes: &[u8]) -> Result<bool, AutomergeError> {
        let before = self.doc.get_heads();
        self.doc.load_incremental(bytes)?;
        Ok(self.doc.get_heads() != before)
    }

    fn commit(&mut self) {
        self.doc.commit_with(CommitOptions::default().with_time(now_ms()));
    }

    fn ensure_roots(&mut self) {
        for k in ["nodes", "edges", "extra"] {
            if self.doc.get(ROOT, k).ok().flatten().is_none() {
                self.doc.put_object(ROOT, k, ObjType::Map).expect("root map");
            }
        }
    }

    fn root_map(&mut self, k: &str) -> ObjId {
        match self.doc.get(ROOT, k).ok().flatten() {
            Some((AmValue::Object(ObjType::Map), id)) => id,
            _ => self.doc.put_object(ROOT, k, ObjType::Map).expect("root map"),
        }
    }

    fn put_field(&mut self, obj: &ObjId, k: &str, v: &Value) {
        if k == "text" {
            let s = v.as_str().unwrap_or_default();
            let t = match self.doc.get(obj, "text").ok().flatten() {
                Some((AmValue::Object(ObjType::Text), id)) => id,
                _ => self.doc.put_object(obj, "text", ObjType::Text).expect("text"),
            };
            let _ = self.doc.update_text(&t, s);
        } else if let Some(sv) = scalar(v) {
            let cur = match self.doc.get(obj, k).ok().flatten() {
                Some((AmValue::Scalar(c), _)) => Some(c.into_owned()),
                _ => None,
            };
            if cur.as_ref() != Some(&sv) {
                let _ = self.doc.put(obj, k, sv);
            }
        } else {
            let _ = self.doc.put(obj, format!("@{k}"), v.to_string());
        }
    }

    /// Creates or updates one element so its fields equal `v`, removing
    /// fields `v` lacks.
    fn write_obj(&mut self, table: &str, id: &str, v: &Value) {
        let t = self.root_map(table);
        let obj = match self.doc.get(&t, id).ok().flatten() {
            Some((AmValue::Object(ObjType::Map), o)) => o,
            _ => self.doc.put_object(&t, id, ObjType::Map).expect("element"),
        };
        let m = v.as_object().cloned().unwrap_or_default();
        for (k, fv) in &m {
            if k != "id" {
                self.put_field(&obj, k, fv);
            }
        }
        let stale: Vec<String> = self
            .doc
            .keys(&obj)
            .filter(|k| {
                let plain = k.strip_prefix('@').unwrap_or(k);
                k != "z" && !m.contains_key(plain)
            })
            .collect();
        for k in stale {
            let _ = self.doc.delete(&obj, k.as_str());
        }
    }

    fn next_z(&self) -> f64 {
        let Some((AmValue::Object(_), t)) = self.doc.get(ROOT, "nodes").ok().flatten() else { return 0.0 };
        let mut z = -1.0f64;
        for k in self.doc.keys(&t) {
            if let Some((_, o)) = self.doc.get(&t, k.as_str()).ok().flatten() {
                if let Some((AmValue::Scalar(s), _)) = self.doc.get(&o, "z").ok().flatten() {
                    if let ScalarValue::F64(f) = s.as_ref() {
                        z = z.max(*f);
                    }
                }
            }
        }
        z + 1.0
    }

    /// Writes a finished local step (forward direction) as one change.
    pub fn record(&mut self, c: &Change) {
        self.ensure_roots();
        for (table, items) in [("nodes", &c.nodes), ("edges", &c.edges)] {
            let t = self.root_map(table);
            for (id, before, after) in items {
                match after {
                    None => {
                        let _ = self.doc.delete(&t, id.as_str());
                    }
                    Some(a) => {
                        let fresh = self.doc.get(&t, id.as_str()).ok().flatten().is_none();
                        // Only the fields this step changed, so concurrent edits of
                        // other fields elsewhere survive.
                        let obj = match self.doc.get(&t, id.as_str()).ok().flatten() {
                            Some((AmValue::Object(ObjType::Map), o)) => o,
                            _ => self.doc.put_object(&t, id.as_str(), ObjType::Map).expect("element"),
                        };
                        let am = a.as_object().cloned().unwrap_or_default();
                        let bm = before.as_ref().and_then(|b| b.as_object().cloned()).unwrap_or_default();
                        for (k, v) in &am {
                            if k != "id" && (fresh || bm.get(k) != Some(v)) {
                                self.put_field(&obj, k, v);
                            }
                        }
                        for k in bm.keys().filter(|k| !am.contains_key(*k)) {
                            let _ = self.doc.delete(&obj, k.as_str());
                            let _ = self.doc.delete(&obj, format!("@{k}"));
                        }
                        if table == "nodes" && fresh {
                            let z = self.next_z();
                            let _ = self.doc.put(&obj, "z", z);
                        }
                    }
                }
            }
        }
        self.commit();
    }

    fn read_obj(&self, obj: &ObjId) -> (Value, f64) {
        let mut m = Map::new();
        let mut z = 0.0;
        for k in self.doc.keys(obj) {
            match self.doc.get(obj, k.as_str()).ok().flatten() {
                Some((AmValue::Object(ObjType::Text), t)) => {
                    m.insert(k.clone(), json!(self.doc.text(&t).unwrap_or_default()));
                }
                Some((AmValue::Scalar(s), _)) if k == "z" => {
                    if let ScalarValue::F64(f) = s.as_ref() {
                        z = *f;
                    }
                }
                Some((AmValue::Scalar(s), _)) => {
                    if let Some(plain) = k.strip_prefix('@') {
                        if let ScalarValue::Str(j) = s.as_ref() {
                            if let Ok(v) = serde_json::from_str(j) {
                                m.insert(plain.to_string(), v);
                            }
                        }
                    } else {
                        m.insert(k.clone(), from_scalar(&s));
                    }
                }
                _ => {}
            }
        }
        (Value::Object(m), z)
    }

    /// The board as the document holds it now.
    pub fn board(&self) -> Board {
        board_of(&self.doc, self)
    }

    /// The board as it was at `heads` (for the timeline).
    pub fn board_at(&mut self, heads: &[ChangeHash]) -> Option<Board> {
        let past = self.doc.fork_at(heads).ok()?;
        let s = Shared { doc: past };
        Some(s.board())
    }

    /// Every change, oldest first: (time ms, hash, actor).
    pub fn changes(&mut self) -> Vec<(i64, ChangeHash, String)> {
        self.steps().into_iter().map(|s| (s.t, s.hash, s.actor)).collect()
    }

    /// Every change with what it depends on, oldest first.
    pub fn steps(&mut self) -> Vec<Step> {
        let mut v: Vec<Step> = self.doc.get_changes(&[]).iter().map(|c| Step { t: c.timestamp(), hash: c.hash(), actor: c.actor_id().to_hex_string(), deps: c.deps().to_vec() }).collect();
        v.sort_by_key(|x| x.t);
        v
    }
}

pub struct Step {
    pub t: i64,
    pub hash: ChangeHash,
    pub actor: String,
    pub deps: Vec<ChangeHash>,
}

/// Heads of the state made of the first `n` steps: those no other step in
/// the prefix builds on.
pub fn heads_of(steps: &[Step], n: usize) -> Vec<ChangeHash> {
    let pre = &steps[..n.min(steps.len())];
    let used: std::collections::HashSet<&ChangeHash> = pre.iter().flat_map(|s| s.deps.iter()).collect();
    pre.iter().filter(|s| !used.contains(&s.hash)).map(|s| s.hash).collect()
}

impl Shared {
}

fn board_of(doc: &AutoCommit, s: &Shared) -> Board {
    let mut nodes: Vec<(f64, String, Value)> = Vec::new();
    let mut edges: Vec<Value> = Vec::new();
    let mut extra = Map::new();
    if let Some((AmValue::Object(_), t)) = doc.get(ROOT, "nodes").ok().flatten() {
        for id in doc.keys(&t) {
            if let Some((AmValue::Object(ObjType::Map), o)) = doc.get(&t, id.as_str()).ok().flatten() {
                let (mut v, z) = s.read_obj(&o);
                v["id"] = json!(id);
                nodes.push((z, id, v));
            }
        }
    }
    if let Some((AmValue::Object(_), t)) = doc.get(ROOT, "edges").ok().flatten() {
        for id in doc.keys(&t) {
            if let Some((AmValue::Object(ObjType::Map), o)) = doc.get(&t, id.as_str()).ok().flatten() {
                let (mut v, _) = s.read_obj(&o);
                v["id"] = json!(id);
                edges.push(v);
            }
        }
    }
    if let Some((AmValue::Object(_), t)) = doc.get(ROOT, "extra").ok().flatten() {
        if let Value::Object(m) = s.read_obj(&t).0 {
            extra = m;
        }
    }
    nodes.sort_by(|a, b| a.0.total_cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    let mut b = Board::new();
    b.extra = extra;
    b.nodes = nodes.iter().filter_map(|x| node_from_json(&x.2)).collect();
    b.edges = edges.iter().filter_map(edge_from_json).collect();
    b.reindex();
    b
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::{Kind, Node, Shape};

    fn node(id: &str, x: f32, text: &str) -> Node {
        Node { id: id.into(), x, y: 0.0, w: 100.0, h: 50.0, text: text.into(), font: 20.0, shape: Shape::Box, color: None, fill: None, extra: Default::default() }
    }

    fn step(before: &Board, after: &Board) -> Change {
        after.diff(&before.snapshot()).unwrap()
    }

    #[test]
    fn round_trip_keeps_board() {
        let mut b = Board::new();
        b.add_node(node("a", 0.0, "Alpha"));
        b.add_node(node("b", 200.0, "Beta"));
        b.nodes[1].extra.insert("tags".into(), json!(["x", 1]));
        b.add_edge("a", "b", Kind::Link);
        let s = Shared::from_board(&b, None);
        let back = s.board();
        assert_eq!(back.serialize(), b.serialize());
    }

    #[test]
    fn concurrent_edits_merge() {
        let mut b = Board::new();
        b.add_node(node("a", 0.0, "hello world"));
        let mut laptop = Shared::from_board(&b, Some(ActorId::random()));
        let bytes = laptop.save();
        let mut phone = Shared::load(&bytes, Some(ActorId::random())).unwrap();

        // Laptop moves the concept and edits the start of the text.
        let mut l = laptop.board();
        let before = l.clone();
        l.get_mut("a").unwrap().x = 500.0;
        l.get_mut("a").unwrap().text = "Hello world".into();
        laptop.record(&step(&before, &l));

        // Phone, offline, edits the end and adds a concept.
        let mut p = phone.board();
        let before = p.clone();
        p.get_mut("a").unwrap().text = "hello world!".into();
        p.add_node(node("c", 0.0, "new"));
        phone.record(&step(&before, &p));

        let from_phone = phone.save_after(&[]);
        assert!(laptop.merge_bytes(&from_phone).unwrap());
        let m = laptop.board();
        let a = m.get("a").unwrap();
        assert_eq!(a.x, 500.0);
        assert_eq!(a.text, "Hello world!");
        assert!(m.get("c").is_some());
        // Creation order survives: a before c.
        assert_eq!(m.nodes[0].id, "a");
    }

    #[test]
    fn timeline_sees_past() {
        let mut b = Board::new();
        b.add_node(node("a", 0.0, "one"));
        let mut s = Shared::from_board(&b, None);
        let first = s.heads();
        let mut l = s.board();
        let before = l.clone();
        l.get_mut("a").unwrap().text = "two".into();
        s.record(&step(&before, &l));
        assert_eq!(s.changes().len(), 2);
        assert_eq!(s.board_at(&first).unwrap().get("a").unwrap().text, "one");
    }
}
