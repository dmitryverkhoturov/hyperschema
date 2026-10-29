//! One-way import of an Excalidraw drawing into a board. Shapes become
//! concepts (their bound text is the concept's text), frames become
//! concepts with children, free text becomes text concepts, arrows become
//! connections (a loose end snaps to the nearest concept or the arrow is
//! dropped). Anything lying fully inside a bigger concept is nested in it.

use crate::board::{contains, Board, Edge, Kind, Node, Shape};
use crate::excalidraw::extract_json;
use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet};

pub struct Report {
    pub dropped_arrows: usize,
}

fn color(v: &Value, default: &str) -> Option<String> {
    v.as_str().filter(|s| *s != default && *s != "transparent" && s.starts_with('#')).map(String::from)
}

pub fn from_excalidraw(text: &str) -> Result<(Board, Report), String> {
    let json = extract_json(text)?;
    let v: Value = serde_json::from_str(&json).map_err(|e| e.to_string())?;
    let els: Vec<&Value> = v["elements"].as_array().ok_or("no elements")?.iter().filter(|e| !e["isDeleted"].as_bool().unwrap_or(false)).collect();
    let f = |e: &Value, k: &str| e[k].as_f64().unwrap_or(0.0) as f32;
    let s = |e: &Value, k: &str| e[k].as_str().unwrap_or("").to_string();

    // Text bound to a container, by container id.
    let mut bound: HashMap<String, &Value> = HashMap::new();
    for e in &els {
        if s(e, "type") == "text" {
            if let Some(c) = e["containerId"].as_str() {
                bound.insert(c.to_string(), e);
            }
        }
    }

    let mut b = Board::new();
    let mut frame_of: Vec<(String, String)> = Vec::new();
    let mut frames: HashSet<String> = HashSet::new();
    for e in &els {
        let id = s(e, "id");
        let kind = s(e, "type");
        let (x, y, w, h) = (f(e, "x"), f(e, "y"), f(e, "width"), f(e, "height"));
        let shape = match kind.as_str() {
            "rectangle" | "image" | "embeddable" | "iframe" | "frame" | "magicframe" => Shape::Box,
            "ellipse" => Shape::Ellipse,
            "diamond" => Shape::Diamond,
            "text" if e["containerId"].is_null() => Shape::Text,
            _ => continue,
        };
        let (text, font) = if kind == "text" {
            (s(e, "text"), f(e, "fontSize").max(1.0))
        } else if kind.ends_with("frame") {
            frames.insert(id.clone());
            (s(e, "name"), 22.0)
        } else {
            match bound.get(&id) {
                Some(t) => (s(t, "text"), f(t, "fontSize").max(1.0)),
                None => (String::new(), 20.0),
            }
        };
        let text_color = if kind == "text" { e } else { bound.get(&id).copied().unwrap_or(e) };
        if let Some(fr) = e["frameId"].as_str() {
            frame_of.push((id.clone(), fr.to_string()));
        }
        b.nodes.push(Node {
            id,
            x: x.min(x + w),
            y: y.min(y + h),
            w: w.abs().max(1.0),
            h: h.abs().max(1.0),
            text,
            font,
            shape,
            color: color(&text_color["strokeColor"], "#1e1e1e"),
            fill: color(&e["backgroundColor"], "transparent"),
            extra: Map::new(),
        });
    }
    b.reindex();

    // Frames draw their name above; ours sits inside the top strip.
    for id in &frames {
        let n = b.get_mut(id).unwrap();
        let t = n.font * crate::text::LINE_H + n.font;
        n.y -= t;
        n.h += t;
    }

    // Nesting: frame membership first, then geometric containment.
    let mut parent: HashMap<String, String> = frame_of.into_iter().filter(|(c, p)| b.get(c).is_some() && b.get(p).is_some()).collect();
    for n in &b.nodes {
        if parent.contains_key(&n.id) {
            continue;
        }
        let nb = n.bbox();
        let best = b
            .nodes
            .iter()
            .filter(|o| o.id != n.id && o.shape != Shape::Text && o.w * o.h > n.w * n.h && contains(&o.bbox(), &nb))
            .min_by(|a, c| (a.w * a.h).partial_cmp(&(c.w * c.h)).unwrap());
        if let Some(p) = best {
            parent.insert(n.id.clone(), p.id.clone());
        }
    }
    // Frame members that also sit inside a shape within the frame: nest in the shape.
    for (c, p) in parent.clone() {
        let cb = b.node(&c).bbox();
        let inner = b
            .nodes
            .iter()
            .filter(|o| o.id != c && o.id != p && o.shape != Shape::Text && parent.get(&o.id) == Some(&p) && o.w * o.h > b.node(&c).w * b.node(&c).h && contains(&o.bbox(), &cb))
            .min_by(|a, d| (a.w * a.h).partial_cmp(&(d.w * d.h)).unwrap())
            .map(|o| o.id.clone());
        if let Some(i) = inner {
            parent.insert(c, i);
        }
    }
    for (c, p) in &parent {
        let id = b.new_id();
        b.edges.push(Edge { id, from: p.clone(), to: c.clone(), kind: Kind::Contains, arrow_from: false, arrow_to: false, label: String::new(), extra: Map::new() });
    }
    b.reindex();

    // Arrows and lines.
    let mut dropped = 0;
    let nearest = |b: &Board, p: (f32, f32), not: Option<&str>| -> Option<String> {
        b.nodes
            .iter()
            .filter(|n| Some(n.id.as_str()) != not)
            .map(|n| {
                let bb = n.bbox();
                let dx = (bb[0] - p.0).max(p.0 - bb[2]).max(0.0);
                let dy = (bb[1] - p.1).max(p.1 - bb[3]).max(0.0);
                (dx.hypot(dy) / n.font.max(1.0), n.id.clone(), n.w * n.h)
            })
            .filter(|x| x.0 < 4.0)
            .min_by(|a, c| a.0.partial_cmp(&c.0).unwrap().then(a.2.partial_cmp(&c.2).unwrap()))
            .map(|x| x.1)
    };
    for e in &els {
        let kind = s(e, "type");
        if kind != "arrow" && kind != "line" {
            continue;
        }
        let pts: Vec<(f32, f32)> = e["points"].as_array().map(|a| a.iter().filter_map(|p| Some((f(e, "x") + p[0].as_f64()? as f32, f(e, "y") + p[1].as_f64()? as f32))).collect()).unwrap_or_default();
        if pts.len() < 2 {
            dropped += 1;
            continue;
        }
        let bind = |k: &str| e[k]["elementId"].as_str().filter(|id| b.get(id).is_some()).map(String::from);
        let from = bind("startBinding").or_else(|| nearest(&b, pts[0], None));
        let to = bind("endBinding").or_else(|| nearest(&b, pts[pts.len() - 1], from.as_deref()));
        let (Some(from), Some(to)) = (from, to) else {
            dropped += 1;
            continue;
        };
        if from == to || b.is_ancestor(&from, &to) || b.is_ancestor(&to, &from) || b.edge_between(&from, &to).is_some() {
            dropped += 1;
            continue;
        }
        let label = bound.get(&s(e, "id")).map(|t| s(t, "text")).unwrap_or_default();
        b.edges.push(Edge {
            id: s(e, "id"),
            from,
            to,
            kind: Kind::Link,
            arrow_from: kind == "arrow" && !e["startArrowhead"].is_null(),
            arrow_to: kind == "arrow" && !e["endArrowhead"].is_null(),
            label,
            extra: Map::new(),
        });
    }
    b.reindex();

    // Parents keep their title strip clear: grow them around their content.
    let order = b.draw_order();
    for id in order.iter().rev() {
        let one = |_: &Node| 1usize;
        b.fit_ancestors(id, &one);
    }
    Ok((b, Report { dropped_arrows: dropped }))
}
