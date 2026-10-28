//! Editable document: the Excalidraw element list kept as raw JSON, so every
//! field we do not understand survives a save untouched. Edits patch only
//! the fields they change and bump `version` the way Excalidraw does.

use crate::excalidraw::{element_into, extract_json};
use crate::scene::Scene;
use serde_json::{json, Value};
use std::collections::HashSet;

enum Source {
    /// Obsidian `.excalidraw.md`: the whole note text; drawing in a fenced block.
    Markdown(String),
    /// Plain `.excalidraw` JSON.
    Json,
}

pub struct Doc {
    root: Value,
    pub els: Vec<Value>,
    source: Source,
    undo: Vec<Vec<Value>>,
    redo: Vec<Vec<Value>>,
    pub dirty: bool,
    rng: u64,
}

pub type Bbox = [f32; 4];

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

fn seg_dist(p: (f32, f32), a: (f32, f32), b: (f32, f32)) -> f32 {
    let (abx, aby) = (b.0 - a.0, b.1 - a.1);
    let t = (((p.0 - a.0) * abx + (p.1 - a.1) * aby) / (abx * abx + aby * aby).max(1e-6)).clamp(0.0, 1.0);
    (p.0 - a.0 - abx * t).hypot(p.1 - a.1 - aby * t)
}

/// Where the ray from the centre of `b` towards `to` leaves the box, pushed out by `gap`.
fn box_exit(b: &Bbox, to: (f32, f32), gap: f32) -> (f32, f32) {
    let c = ((b[0] + b[2]) * 0.5, (b[1] + b[3]) * 0.5);
    let (dx, dy) = (to.0 - c.0, to.1 - c.1);
    let len = dx.hypot(dy).max(1e-6);
    let (hw, hh) = ((b[2] - b[0]) * 0.5, (b[3] - b[1]) * 0.5);
    let t = (hw / dx.abs().max(1e-6)).min(hh / dy.abs().max(1e-6)).min(1.0);
    (c.0 + dx * t + dx / len * gap, c.1 + dy * t + dy / len * gap)
}

impl Doc {
    pub fn parse(text: &str) -> Result<Doc, String> {
        let json = extract_json(text)?;
        let mut root: Value = serde_json::from_str(&json).map_err(|e| e.to_string())?;
        let els = match root.get_mut("elements").map(Value::take) {
            Some(Value::Array(a)) => a,
            _ => return Err("no elements".into()),
        };
        let source = if text.contains("```compressed-json") || text.contains("```json") {
            Source::Markdown(text.to_string())
        } else {
            Source::Json
        };
        Ok(Doc { root, els, source, undo: Vec::new(), redo: Vec::new(), dirty: false, rng: now_ms() | 1 })
    }

    pub fn live(&self, i: usize) -> bool {
        !self.els[i]["isDeleted"].as_bool().unwrap_or(false)
    }

    pub fn kind(&self, i: usize) -> &str {
        self.els[i]["type"].as_str().unwrap_or("")
    }

    fn id(&self, i: usize) -> &str {
        self.els[i]["id"].as_str().unwrap_or("")
    }

    pub fn find(&self, id: &str) -> Option<usize> {
        self.els.iter().position(|e| e["id"].as_str() == Some(id))
    }

    fn num(&self, i: usize, k: &str) -> f32 {
        self.els[i][k].as_f64().unwrap_or(0.0) as f32
    }

    /// Scene of every live element except `skip`.
    pub fn scene(&self, skip: &HashSet<usize>) -> Scene {
        let mut s = Scene::default();
        for (i, e) in self.els.iter().enumerate() {
            if !skip.contains(&i) {
                element_into(e, &mut s);
            }
        }
        s
    }

    /// Scene of just `only`.
    pub fn scene_of(&self, only: &[usize]) -> Scene {
        let mut s = Scene::default();
        for &i in only {
            element_into(&self.els[i], &mut s);
        }
        s
    }

    /// Absolute points of a linear element.
    pub fn points(&self, i: usize) -> Vec<(f32, f32)> {
        let (x, y) = (self.num(i, "x"), self.num(i, "y"));
        self.els[i]["points"]
            .as_array()
            .map(|a| a.iter().filter_map(|p| Some((x + p[0].as_f64()? as f32, y + p[1].as_f64()? as f32))).collect())
            .unwrap_or_default()
    }

    pub fn is_linear(&self, i: usize) -> bool {
        matches!(self.kind(i), "arrow" | "line" | "freedraw")
    }

    pub fn bbox(&self, i: usize) -> Bbox {
        if self.is_linear(i) {
            let mut b = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
            for (x, y) in self.points(i) {
                b = [b[0].min(x), b[1].min(y), b[2].max(x), b[3].max(y)];
            }
            return b;
        }
        let (x, y, w, h) = (self.num(i, "x"), self.num(i, "y"), self.num(i, "width"), self.num(i, "height"));
        [x.min(x + w), y.min(y + h), x.max(x + w), y.max(y + h)]
    }

    pub fn container(&self, i: usize) -> Option<usize> {
        self.els[i]["containerId"].as_str().and_then(|id| self.find(id))
    }

    /// The bound text of a shape, if it has one.
    pub fn bound_text(&self, i: usize) -> Option<usize> {
        let id = self.id(i);
        (0..self.els.len()).find(|&j| self.live(j) && self.kind(j) == "text" && self.els[j]["containerId"].as_str() == Some(id))
    }

    /// Topmost element under (x, y): arrows first, then text, then the
    /// smallest shape, then frame borders. `tol` is in world units.
    pub fn hit(&self, x: f32, y: f32, tol: f32) -> Option<usize> {
        let mut best: Option<((u8, f32), usize)> = None;
        for i in (0..self.els.len()).rev() {
            if !self.live(i) {
                continue;
            }
            let b = self.bbox(i);
            let inside = x >= b[0] - tol && x <= b[2] + tol && y >= b[1] - tol && y <= b[3] + tol;
            let area = (b[2] - b[0]) * (b[3] - b[1]);
            let cand = match self.kind(i) {
                "arrow" | "line" | "freedraw" => {
                    let p = self.points(i);
                    let near = p.windows(2).any(|w| seg_dist((x, y), w[0], w[1]) < tol);
                    near.then_some(((0, 0.0), i))
                }
                "text" => inside.then(|| ((1, 0.0), self.container(i).unwrap_or(i))),
                "frame" | "magicframe" => {
                    let border = inside && (x - b[0] < tol || b[2] - x < tol || y - b[1] < tol || b[3] - y < tol);
                    let name = x >= b[0] && x <= b[2] && y >= b[1] - 32.0 && y <= b[1];
                    (border || name).then_some(((3, area), i))
                }
                _ => inside.then_some(((2, area), i)),
            };
            if let Some(c) = cand {
                // Ties keep the earlier (= topmost) candidate.
                if best.as_ref().map_or(true, |b| c.0 .0 < b.0 .0 || (c.0 .0 == b.0 .0 && c.0 .1 < b.0 .1)) {
                    best = Some(c);
                }
            }
        }
        best.map(|b| b.1)
    }

    /// What a click on `i` selects: its outermost group, or itself.
    pub fn unit(&self, i: usize) -> Vec<usize> {
        let groups = self.els[i]["groupIds"].as_array();
        match groups.and_then(|g| g.last()).and_then(Value::as_str) {
            Some(g) => (0..self.els.len())
                .filter(|&j| self.live(j) && self.els[j]["groupIds"].as_array().is_some_and(|a| a.iter().any(|v| v.as_str() == Some(g))))
                .collect(),
            None => vec![i],
        }
    }

    /// The selection plus everything that moves with it: bound text and
    /// the contents of selected frames.
    pub fn closure(&self, sel: &[usize]) -> Vec<usize> {
        let mut set: HashSet<usize> = sel.iter().copied().collect();
        loop {
            let ids: HashSet<&str> = set.iter().map(|&i| self.id(i)).collect();
            let more: Vec<usize> = (0..self.els.len())
                .filter(|&j| !set.contains(&j) && self.live(j))
                .filter(|&j| {
                    let e = &self.els[j];
                    e["containerId"].as_str().is_some_and(|c| ids.contains(c)) || e["frameId"].as_str().is_some_and(|f| ids.contains(f))
                })
                .collect();
            if more.is_empty() {
                break;
            }
            set.extend(more);
        }
        let mut v: Vec<usize> = set.into_iter().collect();
        v.sort_unstable();
        v
    }

    /// Arrows outside `set` with an end bound to something inside it.
    pub fn attached_arrows(&self, set: &[usize]) -> Vec<usize> {
        let inside: HashSet<usize> = set.iter().copied().collect();
        let ids: HashSet<&str> = set.iter().map(|&i| self.id(i)).collect();
        (0..self.els.len())
            .filter(|&j| !inside.contains(&j) && self.live(j) && self.kind(j) == "arrow")
            .filter(|&j| {
                ["startBinding", "endBinding"].iter().any(|k| self.els[j][*k]["elementId"].as_str().is_some_and(|id| ids.contains(id)))
            })
            .collect()
    }

    fn touch(&mut self, i: usize) {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        let e = &mut self.els[i];
        e["version"] = json!(e["version"].as_u64().unwrap_or(1) + 1);
        e["versionNonce"] = json!(self.rng as u32 >> 1);
        e["updated"] = json!(now_ms());
        self.dirty = true;
    }

    fn shift(&mut self, i: usize, dx: f32, dy: f32) {
        let e = &mut self.els[i];
        e["x"] = json!(e["x"].as_f64().unwrap_or(0.0) + dx as f64);
        e["y"] = json!(e["y"].as_f64().unwrap_or(0.0) + dy as f64);
        self.touch(i);
    }

    /// Moves `set` (already a closure) and drags the bound ends of attached arrows.
    pub fn move_by(&mut self, set: &[usize], dx: f32, dy: f32) {
        for &i in set {
            self.shift(i, dx, dy);
        }
        let ids: HashSet<String> = set.iter().map(|&i| self.id(i).to_string()).collect();
        for a in self.attached_arrows(set) {
            let bound = |k: &str| self.els[a][k]["elementId"].as_str().is_some_and(|id| ids.contains(id));
            let (start, end) = (bound("startBinding"), bound("endBinding"));
            if start && end {
                self.shift(a, dx, dy);
                continue;
            }
            let Some(pts) = self.els[a]["points"].as_array_mut() else { continue };
            let n = pts.len();
            if start {
                // Keep points relative to x/y: move the origin, pull the rest back.
                for p in pts.iter_mut().skip(1) {
                    p[0] = json!(p[0].as_f64().unwrap_or(0.0) - dx as f64);
                    p[1] = json!(p[1].as_f64().unwrap_or(0.0) - dy as f64);
                }
                self.shift(a, dx, dy);
            } else if end && n > 0 {
                let p = &mut pts[n - 1];
                p[0] = json!(p[0].as_f64().unwrap_or(0.0) + dx as f64);
                p[1] = json!(p[1].as_f64().unwrap_or(0.0) + dy as f64);
            }
            self.fix_linear_size(a);
            self.touch(a);
        }
    }

    fn fix_linear_size(&mut self, i: usize) {
        let Some(pts) = self.els[i]["points"].as_array() else { return };
        let (mut x0, mut y0, mut x1, mut y1) = (0f64, 0f64, 0f64, 0f64);
        for p in pts {
            let (x, y) = (p[0].as_f64().unwrap_or(0.0), p[1].as_f64().unwrap_or(0.0));
            (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
        }
        self.els[i]["width"] = json!(x1 - x0);
        self.els[i]["height"] = json!(y1 - y0);
    }

    pub fn delete(&mut self, set: &[usize]) {
        for &i in set {
            self.els[i]["isDeleted"] = json!(true);
            self.touch(i);
        }
    }

    // ------------------------------------------------------------ undo

    pub fn checkpoint(&mut self) {
        self.undo.push(self.els.clone());
        if self.undo.len() > 200 {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    pub fn undo(&mut self) -> bool {
        let Some(prev) = self.undo.pop() else { return false };
        self.redo.push(std::mem::replace(&mut self.els, prev));
        self.dirty = true;
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(next) = self.redo.pop() else { return false };
        self.undo.push(std::mem::replace(&mut self.els, next));
        self.dirty = true;
        true
    }

    // ------------------------------------------------------------ creation

    fn new_id(&mut self) -> String {
        const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
        (0..8)
            .map(|_| {
                self.rng ^= self.rng << 13;
                self.rng ^= self.rng >> 7;
                self.rng ^= self.rng << 17;
                A[(self.rng % A.len() as u64) as usize] as char
            })
            .collect()
    }

    fn base(&mut self, kind: &str, x: f32, y: f32, w: f32, h: f32) -> Value {
        let id = self.new_id();
        let seed = (self.rng >> 1) as u32;
        json!({
            "id": id, "type": kind, "x": x, "y": y, "width": w, "height": h, "angle": 0,
            "strokeColor": "#1e1e1e", "backgroundColor": "transparent", "fillStyle": "solid",
            "strokeWidth": 2, "strokeStyle": "solid", "roughness": 1, "opacity": 100,
            "groupIds": [], "frameId": null, "roundness": null, "seed": seed,
            "version": 1, "versionNonce": seed ^ 0x5bd1e995, "isDeleted": false,
            "boundElements": null, "updated": now_ms(), "link": null, "locked": false
        })
    }

    /// Font family most texts in this file use, so new text matches.
    fn font_family(&self) -> Value {
        let mut counts: Vec<(i64, usize)> = Vec::new();
        for e in &self.els {
            if let Some(f) = e["fontFamily"].as_i64() {
                match counts.iter_mut().find(|c| c.0 == f) {
                    Some(c) => c.1 += 1,
                    None => counts.push((f, 1)),
                }
            }
        }
        json!(counts.iter().max_by_key(|c| c.1).map_or(5, |c| c.0))
    }

    /// A rounded box centred at (cx, cy) with an empty bound text. Returns (box, text).
    pub fn add_box(&mut self, cx: f32, cy: f32) -> (usize, usize) {
        let (w, h) = (200.0, 80.0);
        let mut b = self.base("rectangle", cx - w * 0.5, cy - h * 0.5, w, h);
        b["roundness"] = json!({ "type": 3 });
        let mut t = self.base("text", cx, cy - 12.5, 0.0, 25.0);
        b["boundElements"] = json!([{ "type": "text", "id": t["id"] }]);
        let extra = json!({
            "text": "", "originalText": "", "fontSize": 20, "fontFamily": self.font_family(),
            "textAlign": "center", "verticalAlign": "middle", "containerId": b["id"],
            "autoResize": true, "lineHeight": 1.25
        });
        for (k, v) in extra.as_object().unwrap() {
            t[k] = v.clone();
        }
        self.els.push(b);
        self.els.push(t);
        self.dirty = true;
        (self.els.len() - 2, self.els.len() - 1)
    }

    /// The bound text of `shape`, creating an empty one if it has none.
    pub fn ensure_text(&mut self, shape: usize) -> usize {
        if let Some(t) = self.bound_text(shape) {
            return t;
        }
        let (cx, cy) = self.center(shape);
        let t = self.add_text(cx, cy - 12.5);
        let sid = self.els[shape]["id"].clone();
        self.els[t]["containerId"] = sid;
        self.els[t]["textAlign"] = json!("center");
        self.els[t]["verticalAlign"] = json!("middle");
        let tid = self.els[t]["id"].clone();
        let e = &mut self.els[shape];
        if !e["boundElements"].is_array() {
            e["boundElements"] = json!([]);
        }
        e["boundElements"].as_array_mut().unwrap().push(json!({ "type": "text", "id": tid }));
        self.touch(shape);
        t
    }

    /// Free text with its top-left at (x, y).
    pub fn add_text(&mut self, x: f32, y: f32) -> usize {
        let mut t = self.base("text", x, y, 0.0, 25.0);
        let extra = json!({
            "text": "", "originalText": "", "fontSize": 20, "fontFamily": self.font_family(),
            "textAlign": "left", "verticalAlign": "top", "containerId": null,
            "autoResize": true, "lineHeight": 1.25
        });
        for (k, v) in extra.as_object().unwrap() {
            t[k] = v.clone();
        }
        self.els.push(t);
        self.dirty = true;
        self.els.len() - 1
    }

    pub fn text(&self, i: usize) -> String {
        let e = &self.els[i];
        e["originalText"].as_str().or(e["text"].as_str()).unwrap_or("").to_string()
    }

    pub fn font_size(&self, i: usize) -> f32 {
        self.els[i]["fontSize"].as_f64().unwrap_or(20.0) as f32
    }

    /// Sets the text of a text element; `size` is its measured (w, h).
    /// Bound text is re-centred and its container grows to fit.
    pub fn set_text(&mut self, i: usize, text: &str, size: (f32, f32)) {
        let (w, h) = size;
        self.els[i]["text"] = json!(text);
        self.els[i]["originalText"] = json!(text);
        self.els[i]["width"] = json!(w);
        self.els[i]["height"] = json!(h);
        if let Some(c) = self.container(i) {
            let b = self.bbox(c);
            let (cw, ch) = ((b[2] - b[0]).max(w + 20.0), (b[3] - b[1]).max(h + 20.0));
            if cw > b[2] - b[0] || ch > b[3] - b[1] {
                let (cx, cy) = ((b[0] + b[2]) * 0.5, (b[1] + b[3]) * 0.5);
                self.els[c]["x"] = json!(cx - cw * 0.5);
                self.els[c]["y"] = json!(cy - ch * 0.5);
                self.els[c]["width"] = json!(cw);
                self.els[c]["height"] = json!(ch);
                self.touch(c);
                // Keep attached arrows touching the grown box.
                self.refit_arrows(c);
            }
            let b = self.bbox(c);
            self.els[i]["x"] = json!((b[0] + b[2]) * 0.5 - w * 0.5);
            self.els[i]["y"] = json!((b[1] + b[3]) * 0.5 - h * 0.5);
        }
        self.touch(i);
    }

    fn refit_arrows(&mut self, shape: usize) {
        for a in self.attached_arrows(&[shape]) {
            let s = self.els[a]["startBinding"]["elementId"].as_str().and_then(|id| self.find(id));
            let e = self.els[a]["endBinding"]["elementId"].as_str().and_then(|id| self.find(id));
            let pts = self.points(a);
            if pts.len() < 2 {
                continue;
            }
            let (p0, p1) = (pts[0], pts[pts.len() - 1]);
            let q0 = s.map_or(p0, |s| box_exit(&self.bbox(s), pts[1], 5.0));
            let q1 = e.map_or(p1, |e| box_exit(&self.bbox(e), pts[pts.len() - 2], 5.0));
            let n = pts.len();
            let mut abs = pts.clone();
            abs[0] = q0;
            abs[n - 1] = q1;
            self.set_points(a, &abs);
        }
    }

    fn set_points(&mut self, a: usize, abs: &[(f32, f32)]) {
        let (x, y) = abs[0];
        self.els[a]["x"] = json!(x);
        self.els[a]["y"] = json!(y);
        self.els[a]["points"] = Value::Array(abs.iter().map(|&(px, py)| json!([px - x, py - y])).collect());
        self.fix_linear_size(a);
        self.touch(a);
    }

    /// An arrow from p0 to p1; ends dropped on shapes are bound to them.
    pub fn add_arrow(&mut self, p0: (f32, f32), p1: (f32, f32), from: Option<usize>, to: Option<usize>) -> usize {
        let to = to.filter(|t| Some(*t) != from);
        let a0 = from.map_or(p0, |f| box_exit(&self.bbox(f), to.map_or(p1, |t| self.center(t)), 5.0));
        let a1 = to.map_or(p1, |t| box_exit(&self.bbox(t), from.map_or(p0, |f| self.center(f)), 5.0));
        let mut a = self.base("arrow", a0.0, a0.1, 0.0, 0.0);
        a["roundness"] = json!({ "type": 2 });
        let extra = json!({
            "points": [[0.0, 0.0], [a1.0 - a0.0, a1.1 - a0.1]], "lastCommittedPoint": null,
            "startArrowhead": null, "endArrowhead": "arrow", "elbowed": false,
            "startBinding": from.map(|f| json!({ "elementId": self.id(f), "focus": 0, "gap": 5 })),
            "endBinding": to.map(|t| json!({ "elementId": self.id(t), "focus": 0, "gap": 5 }))
        });
        for (k, v) in extra.as_object().unwrap() {
            a[k] = v.clone();
        }
        let aid = a["id"].clone();
        self.els.push(a);
        let idx = self.els.len() - 1;
        self.fix_linear_size(idx);
        for s in [from, to].into_iter().flatten() {
            let e = &mut self.els[s];
            if !e["boundElements"].is_array() {
                e["boundElements"] = json!([]);
            }
            e["boundElements"].as_array_mut().unwrap().push(json!({ "type": "arrow", "id": aid }));
            self.touch(s);
        }
        self.dirty = true;
        idx
    }

    pub fn center(&self, i: usize) -> (f32, f32) {
        let b = self.bbox(i);
        ((b[0] + b[2]) * 0.5, (b[1] + b[3]) * 0.5)
    }

    // ------------------------------------------------------------ save

    fn json_text(&self, pretty: bool) -> String {
        let mut root = self.root.clone();
        root["elements"] = Value::Array(self.els.clone());
        if pretty {
            serde_json::to_string_pretty(&root).unwrap_or_default()
        } else {
            serde_json::to_string(&root).unwrap_or_default()
        }
    }

    /// The `## Text Elements` body the Obsidian plugin keeps in sync with
    /// the drawing (it reads text back from there on load).
    /// Entries already listed keep their place in `old`; new ones follow.
    fn text_section(&self, old: &str) -> String {
        let mut items: Vec<(usize, usize)> = Vec::new();
        for i in 0..self.els.len() {
            if self.live(i) && self.kind(i) == "text" {
                let pos = old.find(&format!(" ^{}\n", self.id(i))).unwrap_or(usize::MAX);
                items.push((pos, i));
            }
        }
        items.sort();
        let mut s = String::new();
        for (_, i) in items {
            s.push_str(&format!("{} ^{}\n\n", self.text(i), self.id(i)));
        }
        s
    }

    /// The whole file as it should be written to disk.
    pub fn serialize(&self) -> String {
        let Source::Markdown(md) = &self.source else { return self.json_text(true) };
        let mut out = md.clone();
        // Text Elements: from the heading to the next section or %% marker.
        if let Some(h) = out.find("## Text Elements\n") {
            let body = h + "## Text Elements\n".len();
            let end = out[body..]
                .match_indices('\n')
                .map(|(k, _)| body + k + 1)
                .find(|&k| out[k..].starts_with("%%") || out[k..].starts_with("## ") || out[k..].starts_with("# "))
                .unwrap_or(out.len());
            let section = self.text_section(&out[body..end]);
            out.replace_range(body..end, &section);
        }
        if let Some(fence) = out.find("```compressed-json\n") {
            let body = fence + "```compressed-json\n".len();
            let end = body + out[body..].find("```").unwrap_or(out.len() - body);
            let packed = lz_str::compress_to_base64(self.json_text(false).as_str());
            let mut wrapped = String::new();
            for chunk in packed.as_bytes().chunks(256) {
                wrapped.push_str(std::str::from_utf8(chunk).unwrap_or(""));
                wrapped.push_str("\n\n");
            }
            out.replace_range(body..end, &wrapped);
        } else if let Some(fence) = out.find("```json\n") {
            let body = fence + "```json\n".len();
            let end = body + out[body..].find("```").unwrap_or(out.len() - body);
            out.replace_range(body..end, &(self.json_text(true) + "\n"));
        }
        out
    }

    /// Replaces the markdown template with what was written, so a later
    /// save starts from the current file.
    pub fn saved_as(&mut self, written: String) {
        if let Source::Markdown(md) = &mut self.source {
            *md = written;
        }
        self.dirty = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> String {
        let els = json!({"type":"excalidraw","version":2,"elements":[
            {"id":"r1","type":"rectangle","x":0,"y":0,"width":100,"height":50,"isDeleted":false,"version":1,"boundElements":[{"type":"text","id":"t1"}]},
            {"id":"t1","type":"text","x":10,"y":10,"width":50,"height":25,"text":"Hi","originalText":"Hi","containerId":"r1","isDeleted":false,"fontSize":20},
            {"id":"a1","type":"arrow","x":100,"y":25,"width":100,"height":0,"points":[[0,0],[100,0]],"isDeleted":false,"startBinding":{"elementId":"r1","focus":0,"gap":5},"endBinding":null}
        ],"appState":{"gridSize":null},"files":{}});
        let packed = lz_str::compress_to_base64(els.to_string().as_str());
        format!("---\nexcalidraw-plugin: parsed\n---\n# Excalidraw Data\n\n## Text Elements\nHi ^t1\n\n%%\n## Drawing\n```compressed-json\n{packed}\n```\n%%")
    }

    #[test]
    fn roundtrip_keeps_everything() {
        let text = sample();
        let d = Doc::parse(&text).unwrap();
        let back = Doc::parse(&d.serialize()).unwrap();
        assert_eq!(d.els, back.els);
        assert_eq!(back.root["appState"]["gridSize"], Value::Null);
        assert!(d.serialize().contains("Hi ^t1\n\n%%"));
    }

    #[test]
    fn moving_box_drags_text_and_arrow_start() {
        let mut d = Doc::parse(&sample()).unwrap();
        let set = d.closure(&d.unit(d.hit(5.0, 5.0, 1.0).unwrap()));
        assert_eq!(set, vec![0, 1]);
        d.move_by(&set, 10.0, 20.0);
        assert_eq!(d.els[1]["x"], json!(20.0));
        let p = d.points(2);
        assert_eq!(p[0], (110.0, 45.0));
        assert_eq!(p[1], (200.0, 25.0));
        assert!(d.undo.is_empty());
    }

    #[test]
    fn new_box_and_arrow_bind() {
        let mut d = Doc::parse(&sample()).unwrap();
        let (b, t) = d.add_box(500.0, 0.0);
        d.set_text(t, "Новый", (60.0, 25.0));
        let a = d.add_arrow((50.0, 25.0), (500.0, 0.0), Some(0), Some(b));
        assert_eq!(d.els[a]["endBinding"]["elementId"], d.els[b]["id"]);
        let s = d.serialize();
        assert!(s.contains("Новый ^"));
        assert_eq!(Doc::parse(&s).unwrap().els.len(), 6);
    }
}
