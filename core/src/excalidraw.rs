//! Import of Excalidraw scenes: plain `.excalidraw` JSON or Obsidian
//! `.excalidraw.md` with a `compressed-json` / `json` drawing block.

use crate::scene::{parse_color, Align, Label, Line, Rect, Scene, Shape};
use serde_json::Value;

pub fn load_str(text: &str) -> Result<Scene, String> {
    let json = extract_json(text)?;
    let v: Value = serde_json::from_str(&json).map_err(|e| e.to_string())?;
    let els = v["elements"].as_array().ok_or("no elements")?;
    let mut scene = Scene::default();
    for e in els {
        element_into(e, &mut scene);
    }
    Ok(scene)
}

/// Appends the drawable parts of one Excalidraw element to `scene`.
pub fn element_into(e: &Value, scene: &mut Scene) {
    {
        if e["isDeleted"].as_bool().unwrap_or(false) {
            return;
        }
        let f = |k: &str| e[k].as_f64().unwrap_or(0.0) as f32;
        let (x, y, w, h) = (f("x"), f("y"), f("width"), f("height"));
        let opacity = e["opacity"].as_f64().unwrap_or(100.0) as f32 / 100.0;
        let stroke = parse_color(e["strokeColor"].as_str().unwrap_or("#1e1e1e"), opacity);
        let fill = parse_color(e["backgroundColor"].as_str().unwrap_or("transparent"), opacity);
        let sw = e["strokeWidth"].as_f64().unwrap_or(1.0) as f32;
        let rounded = !e["roundness"].is_null();
        match e["type"].as_str().unwrap_or("") {
            t @ ("rectangle" | "ellipse" | "diamond" | "image" | "embeddable" | "iframe") => {
                let shape = match t {
                    "ellipse" => Shape::Ellipse,
                    "diamond" => Shape::Diamond,
                    _ => Shape::Rect,
                };
                let radius = if rounded && shape == Shape::Rect { (w.min(h) * 0.25).min(32.0) } else { 0.0 };
                scene.rects.push(Rect { x, y, w, h, fill, stroke, stroke_w: sw, radius, shape });
            }
            "frame" | "magicframe" => {
                let grey = parse_color("#bbbbbb", 1.0);
                scene.rects.push(Rect { x, y, w, h, fill: 0, stroke: grey, stroke_w: 2.0, radius: 8.0, shape: Shape::Frame });
                if let Some(name) = e["name"].as_str() {
                    scene.labels.push(Label {
                        x,
                        y: y - 30.0,
                        w,
                        size: 22.0,
                        color: parse_color("#888888", 1.0),
                        align: Align::Left,
                        wrap: false,
                        text: name.to_string(),
                    });
                }
            }
            "text" => {
                let align = match e["textAlign"].as_str() {
                    Some("center") => Align::Center,
                    Some("right") => Align::Right,
                    _ => Align::Left,
                };
                scene.labels.push(Label {
                    x,
                    y,
                    w,
                    size: e["fontSize"].as_f64().unwrap_or(20.0) as f32,
                    color: stroke,
                    align,
                    wrap: false,
                    text: e["text"].as_str().unwrap_or("").to_string(),
                });
            }
            t @ ("arrow" | "line" | "freedraw") => {
                let Some(pts) = e["points"].as_array() else { return };
                let pts: Vec<(f32, f32)> = pts
                    .iter()
                    .filter_map(|p| Some((x + p[0].as_f64()? as f32, y + p[1].as_f64()? as f32)))
                    .collect();
                // Start arrowheads are not drawn yet.
                let head = t == "arrow" && !e["endArrowhead"].is_null();
                let n = pts.len();
                for i in 1..n {
                    let (a, b) = (pts[i - 1], pts[i]);
                    scene.lines.push(Line { x0: a.0, y0: a.1, x1: b.0, y1: b.1, color: stroke, width: sw, head: head && i == n - 1 });
                }
            }
            _ => {}
        }
    }
}

pub(crate) fn extract_json(text: &str) -> Result<String, String> {
    if let Some(rest) = text.split("```compressed-json").nth(1) {
        let body: String = rest.split("```").next().unwrap_or("").chars().filter(|c| !c.is_whitespace()).collect();
        let wide = lz_str::decompress_from_base64(&body).ok_or("bad compressed-json")?;
        return String::from_utf16(&wide).map_err(|e| e.to_string());
    }
    if let Some(rest) = text.split("```json").nth(1) {
        return Ok(rest.split("```").next().unwrap_or("").to_string());
    }
    Ok(text.to_string())
}
