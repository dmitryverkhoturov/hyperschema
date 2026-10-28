//! Scene model: plain data, world coordinates (f32 storage, f64 camera).

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Shape {
    Rect = 0,
    Ellipse = 1,
    Diamond = 2,
    Frame = 3,
}

#[derive(Clone, Copy, Debug)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    /// RGBA, R in the low byte (0 alpha = no fill).
    pub fill: u32,
    pub stroke: u32,
    pub stroke_w: f32,
    pub radius: f32,
    pub shape: Shape,
}

#[derive(Clone, Copy, Debug)]
pub struct Line {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
    pub color: u32,
    pub width: f32,
    /// Draw an arrowhead at (x1, y1).
    pub head: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Align {
    Left,
    Center,
    Right,
}

#[derive(Clone, Debug)]
pub struct Label {
    pub x: f32,
    pub y: f32,
    /// Box width; used for alignment and (if `wrap`) word wrapping.
    pub w: f32,
    /// Font size in world units.
    pub size: f32,
    pub color: u32,
    pub align: Align,
    pub wrap: bool,
    pub text: String,
}

impl Label {
    pub fn line_count(&self) -> usize {
        self.text.split('\n').count()
    }
}

#[derive(Default, Clone)]
pub struct Scene {
    pub rects: Vec<Rect>,
    pub lines: Vec<Line>,
    pub labels: Vec<Label>,
}

impl Scene {
    pub fn bounds(&self) -> [f32; 4] {
        let mut b = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
        let mut add = |x0: f32, y0: f32, x1: f32, y1: f32| {
            b[0] = b[0].min(x0);
            b[1] = b[1].min(y0);
            b[2] = b[2].max(x1);
            b[3] = b[3].max(y1);
        };
        for r in &self.rects {
            add(r.x, r.y, r.x + r.w, r.y + r.h);
        }
        for l in &self.lines {
            add(l.x0.min(l.x1), l.y0.min(l.y1), l.x0.max(l.x1), l.y0.max(l.y1));
        }
        for t in &self.labels {
            add(t.x, t.y, t.x + t.w.max(t.size), t.y + t.size * 1.25 * t.line_count() as f32);
        }
        if b[0] > b[2] {
            [0.0, 0.0, 1000.0, 1000.0]
        } else {
            b
        }
    }

    pub fn element_count(&self) -> usize {
        self.rects.len() + self.lines.len() + self.labels.len()
    }
}

pub fn rgba(r: u8, g: u8, b: u8, a: u8) -> u32 {
    r as u32 | (g as u32) << 8 | (b as u32) << 16 | (a as u32) << 24
}

/// Parses "#rrggbb", "#rgb", "#rrggbbaa"; "transparent" and garbage give 0.
pub fn parse_color(s: &str, opacity: f32) -> u32 {
    let s = s.trim();
    let Some(hex) = s.strip_prefix('#') else { return 0 };
    let v = |i: usize, n: usize| u8::from_str_radix(&hex[i..i + n], 16).ok();
    let (r, g, b, a) = match hex.len() {
        3 => {
            let d = |i| v(i, 1).map(|x| x * 17);
            (d(0), d(1), d(2), Some(255))
        }
        6 => (v(0, 2), v(2, 2), v(4, 2), Some(255)),
        8 => (v(0, 2), v(2, 2), v(4, 2), v(6, 2)),
        _ => return 0,
    };
    match (r, g, b, a) {
        (Some(r), Some(g), Some(b), Some(a)) => rgba(r, g, b, (a as f32 * opacity.clamp(0.0, 1.0)) as u8),
        _ => 0,
    }
}
