//! Synthetic heavy scene: mind-map clusters of labelled boxes with arrows,
//! roughly what a very large personal knowledge board looks like.

use crate::scene::{rgba, Align, Label, Line, Rect, Scene, Shape};

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn f(&mut self) -> f32 {
        (self.next() >> 40) as f32 / (1u64 << 24) as f32
    }
    fn pick<'a>(&mut self, v: &[&'a str]) -> &'a str {
        v[(self.next() % v.len() as u64) as usize]
    }
}

const WORDS: &[&str] = &[
    "Синхронизация", "между", "устройствами", "Бесконечное", "поле", "Связи", "Слои", "заметки", "архитектура",
    "Nested", "dimension", "рендер", "плитки", "камера", "Obsidian", "идея", "проект", "задача", "схема",
    "агент", "память", "Вдохновляться", "Freeform", "масштаб", "скрытие", "элементы", "Android", "Windows",
    "плавно", "графика", "markdown", "ссылка", "фрейм", "группа", "портал", "to-do", "вопрос", "гипотеза",
];

const PALETTE: &[(u8, u8, u8)] = &[
    (255, 236, 153), (178, 242, 187), (165, 216, 255), (255, 201, 201), (208, 191, 255), (255, 255, 255),
];

pub fn generate(n_boxes: usize) -> Scene {
    let mut rng = Rng(0x9E3779B97F4A7C15);
    let mut s = Scene::default();
    let per = 50usize;
    let clusters = n_boxes.div_ceil(per);
    let side = (clusters as f32).sqrt().ceil() as usize;
    let spacing = 2600.0f32;
    let ink = rgba(30, 30, 30, 255);
    for c in 0..clusters {
        let ox = (c % side) as f32 * spacing + rng.f() * 300.0;
        let oy = (c / side) as f32 * spacing + rng.f() * 300.0;
        let count = per.min(n_boxes - c * per);
        let mut centres: Vec<(f32, f32)> = Vec::with_capacity(count);
        for i in 0..count {
            let (cx, cy) = if i == 0 {
                (ox + 1100.0, oy + 1100.0)
            } else {
                let ring = 1 + (i - 1) / 12;
                let a = (i as f32) * 0.523 + rng.f() * 0.2;
                let r = ring as f32 * 260.0;
                (ox + 1100.0 + a.cos() * r * 1.6, oy + 1100.0 + a.sin() * r)
            };
            let (w, h) = if i == 0 { (240.0, 120.0) } else { (150.0 + rng.f() * 60.0, 70.0 + rng.f() * 30.0) };
            let (r, g, b) = PALETTE[(rng.next() % PALETTE.len() as u64) as usize];
            let shape = match rng.next() % 10 {
                0 => Shape::Ellipse,
                1 => Shape::Diamond,
                _ => Shape::Rect,
            };
            s.rects.push(Rect {
                x: cx - w * 0.5,
                y: cy - h * 0.5,
                w,
                h,
                fill: rgba(r, g, b, 255),
                stroke: ink,
                stroke_w: if i == 0 { 3.0 } else { 1.5 },
                radius: 10.0,
                shape,
            });
            let words = 1 + (rng.next() % 4) as usize;
            let text: Vec<&str> = (0..words).map(|_| rng.pick(WORDS)).collect();
            let size = if i == 0 { 26.0 } else { 18.0 };
            s.labels.push(Label {
                x: cx - w * 0.5 + 10.0,
                y: cy - size * 0.75,
                w: w - 20.0,
                size,
                color: ink,
                align: Align::Center,
                wrap: true,
                text: text.join(" "),
            });
            if i > 0 {
                let p = if i <= 12 { 0 } else { 1 + (i - 13) % 12 };
                let (px, py) = centres[p];
                let (dx, dy) = (cx - px, cy - py);
                let len = (dx * dx + dy * dy).sqrt().max(1.0);
                let (ux, uy) = (dx / len, dy / len);
                s.lines.push(Line {
                    x0: px + ux * 60.0,
                    y0: py + uy * 45.0,
                    x1: cx - ux * 75.0,
                    y1: cy - uy * 40.0,
                    color: rgba(60, 60, 70, 255),
                    width: 1.5,
                    head: true,
                });
            }
            centres.push((cx, cy));
        }
    }
    s
}
