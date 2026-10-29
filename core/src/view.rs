//! Board -> drawable scene. A concept with children shows its text as a
//! title along the top; a leaf shows it centred and wrapped.

use crate::board::{Board, Node, Shape as NShape};
use crate::route::{midpoint, Pt};
use crate::scene::{parse_color, rgba, Align, Label, Line, Rect, Scene, Shape};
use crate::text::LINE_H;
use std::collections::{HashMap, HashSet};

pub const INK: &str = "#1e1e1e";
pub const EDGE_INK: &str = "#5c5c5c";

/// Measures wrapped line count: (text, font size, width) -> lines.
pub type Measure<'a> = &'a dyn Fn(&str, f32, f32) -> usize;

pub fn title_lines(n: &Node, m: Measure) -> usize {
    m(&n.text, n.font, (n.w - 2.0 * n.pad()).max(n.font))
}

pub fn node_into(board: &Board, n: &Node, m: Measure, out: &mut Scene) {
    let stroke = parse_color(n.color.as_deref().unwrap_or(INK), 1.0);
    let fill = n.fill.as_deref().map_or(0, |c| parse_color(c, 1.0));
    let sw = (n.font * 0.08).max(0.0);
    let shape = match n.shape {
        NShape::Box => Some(Shape::Rect),
        NShape::Ellipse => Some(Shape::Ellipse),
        NShape::Diamond => Some(Shape::Diamond),
        NShape::Text => None,
    };
    if let Some(shape) = shape {
        let radius = if shape == Shape::Rect { (n.w.min(n.h) * 0.12).min(n.font * 0.8) } else { 0.0 };
        out.rects.push(Rect { x: n.x, y: n.y, w: n.w, h: n.h, fill, stroke, stroke_w: sw, radius, shape });
    }
    if n.text.is_empty() {
        return;
    }
    let pad = n.pad();
    if n.shape == NShape::Text {
        out.labels.push(Label { x: n.x, y: n.y, w: n.w, size: n.font, color: stroke, align: Align::Left, wrap: false, text: n.text.clone() });
    } else if board.has_children(&n.id) {
        out.labels.push(Label { x: n.x + pad, y: n.y + pad, w: n.w - 2.0 * pad, size: n.font, color: stroke, align: Align::Left, wrap: true, text: n.text.clone() });
    } else {
        let w = (n.w - 2.0 * pad).max(n.font);
        let lines = m(&n.text, n.font, w) as f32;
        let y = n.y + (n.h - lines * LINE_H * n.font) * 0.5;
        out.labels.push(Label { x: n.x + pad, y, w, size: n.font, color: stroke, align: Align::Center, wrap: true, text: n.text.clone() });
    }
}

/// Font of a connection: follows the smaller of its two ends.
pub fn edge_font(board: &Board, from: &str, to: &str) -> f32 {
    let f = |id: &str| board.get(id).map_or(20.0, |n| n.font);
    f(from).min(f(to))
}

pub fn edge_into(board: &Board, eid: &str, path: &[Pt], m: Measure, bg: u32, out: &mut Scene) {
    let Some(e) = board.edge(eid) else { return };
    if path.len() < 2 {
        return;
    }
    let font = edge_font(board, &e.from, &e.to);
    let color = parse_color(EDGE_INK, 1.0);
    let width = font * 0.08;
    let n = path.len();
    for i in 1..n {
        let (a, b) = (path[i - 1], path[i]);
        out.lines.push(Line { x0: a.0, y0: a.1, x1: b.0, y1: b.1, color, width, head: e.arrow_to && i == n - 1 });
    }
    if e.arrow_from {
        // The head goes on the segment's far end, so draw the first one reversed.
        let (a, b) = (path[1], path[0]);
        out.lines.push(Line { x0: a.0, y0: a.1, x1: b.0, y1: b.1, color, width, head: true });
    }
    if !e.label.is_empty() {
        let size = font * 0.8;
        let c = midpoint(path);
        let w = size * 12.0;
        let lines = m(&e.label, size, w) as f32;
        let tw = label_width(&e.label, size, w, m);
        let h = lines * LINE_H * size;
        out.rects.push(Rect { x: c.0 - tw * 0.5 - size * 0.3, y: c.1 - h * 0.5, w: tw + size * 0.6, h, fill: bg, stroke: 0, stroke_w: 0.0, radius: size * 0.3, shape: Shape::Rect });
        out.labels.push(Label { x: c.0 - w * 0.5, y: c.1 - h * 0.5, w, size, color: parse_color(INK, 1.0), align: Align::Center, wrap: true, text: e.label.clone() });
    }
}

/// Rough width of a wrapped label (the wrap width if it wraps at all).
fn label_width(text: &str, size: f32, w: f32, m: Measure) -> f32 {
    if m(text, size, w) > text.split('\n').count() {
        return w;
    }
    // Unwrapped: about half an em per character is close enough for the backdrop.
    text.split('\n').map(|l| l.chars().count()).max().unwrap_or(0) as f32 * size * 0.55
}

pub struct Skip<'a> {
    pub nodes: &'a HashSet<String>,
    pub edges: &'a HashSet<String>,
}

pub fn build(board: &Board, routes: &HashMap<String, Vec<Pt>>, m: Measure, skip: &Skip, bg: u32) -> Scene {
    let mut s = Scene::default();
    for id in board.draw_order() {
        if !skip.nodes.contains(&id) {
            node_into(board, board.node(&id), m, &mut s);
        }
    }
    for e in &board.edges {
        if skip.edges.contains(&e.id) {
            continue;
        }
        if let Some(p) = routes.get(&e.id) {
            edge_into(board, &e.id, p, m, bg, &mut s);
        }
    }
    s
}

pub fn bg_color() -> u32 {
    rgba(247, 246, 243, 255)
}
