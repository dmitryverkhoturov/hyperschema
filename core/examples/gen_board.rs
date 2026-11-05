//! Generates a large board that looks like a real one: projects as frames,
//! areas inside them, clusters of notes (sticky colours, free text, a few
//! ellipses and diamonds) joined by arrows, a few long links between
//! projects. Each level is several times smaller than the one holding it,
//! as on a hand-made board.
//!
//!   cargo run --release -p hs-core --example gen_board -- 100000 big.hyperschema [seed]

use hs_core::board::{Board, Edge, Kind, Node, Shape};
use serde_json::Map;

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
    fn range(&mut self, a: f32, b: f32) -> f32 {
        a + (b - a) * self.f()
    }
    fn int(&mut self, a: usize, b: usize) -> usize {
        a + (self.next() % (b - a + 1) as u64) as usize
    }
    fn pick<'a>(&mut self, v: &[&'a str]) -> &'a str {
        v[(self.next() % v.len() as u64) as usize]
    }
    fn chance(&mut self, p: f32) -> bool {
        self.f() < p
    }
}

/// Names of the top-level frames, one per region of the world.
const REGIONS: &[&str] = &["Workshop", "Library", "Observatory", "Harbor", "Atelier", "Lighthouse"];
const PROJECTS: &[&str] = &[
    "Getting paid", "Job search", "Hyperschema", "Diary", "Reading list", "Health", "Side projects", "Research",
    "Trading bot", "Calendar app", "Home", "Learning Rust", "Travel", "Writing", "Agents", "Music",
    "Finances", "Family", "Startup ideas", "Talks", "Open source", "Garden", "Photography", "Fitness",
];
const AREAS: &[&str] = &[
    "Ideas", "Open questions", "Dataset", "Metrics", "Experiments", "Architecture", "Sync", "Rendering",
    "Interviews", "Contacts", "Roadmap", "Done", "Dead ends", "Sources", "Notes", "Bugs", "Design",
    "Plan for the week", "Habits", "Budget", "Targets and loss", "Features", "Validation", "Backlog",
    "Decisions", "Risks", "Benchmarks", "Mobile", "Desktop", "Reading", "People", "Tools",
];
const TAGS: &[&str] = &["IDEA", "DEAD END", "TODO", "TESTED", "NOT TESTED", "FOUND", "QUESTION", "DECIDED", "RISK", "HOLE"];
const SUBJ: &[&str] = &[
    "the model", "sync", "the camera", "zoom", "the dataset", "the week plan", "the parser", "each wallet",
    "the history", "touch input", "glyph atlas", "the router", "a frame", "the quadtree", "the inbox",
    "price history", "the APK", "nested notes", "links", "the timeline", "search", "the backlog", "recall",
];
const VERB: &[&str] = &[
    "picks", "drops", "keeps", "breaks on", "ignores", "remembers", "repeats", "slows down on", "explains",
    "needs", "merges", "hides", "splits", "caches", "measures", "loses", "doubles",
];
const OBJ: &[&str] = &[
    "quiet weeks", "long labels", "old entries", "two devices", "small screens", "deep zoom", "the first run",
    "empty frames", "a cold start", "the last step", "big boards", "short trips", "Monday mornings",
    "a second pass", "edge cases", "noisy data", "late answers", "every frame", "new notes",
];
const SHORT: &[&str] = &[
    "Aug 8", "week 8: Sep 26 → Oct 3", "top-5 +5.7 pp/week", "pair 2", "answer", "features", "P 7 days",
    "AGENTS.md", "sync.rs", "v5 — main", "60 fps", "ask Anna", "call back", "draft", "review", "release notes",
    "budget", "v0.8", "MEMORY.md", "→ next week", "maybe", "later", "done ✓", "blocked", "3 options",
];

const FILLS: &[&str] = &["#ffec99", "#b2f2bb", "#e5dbff", "#ffc9c9", "#a5d8ff", "#96f2d7", "#ffd8a8", "#ffffff"];
const INKS: &[&str] = &["#495057", "#0c8599", "#862e9c", "#5f3dc4", "#e8590c", "#2f9e44"];

fn note_text(r: &mut Rng) -> String {
    match r.int(0, 9) {
        0..=3 => format!("{}: {} {} {}", r.pick(TAGS), r.pick(SUBJ), r.pick(VERB), r.pick(OBJ)),
        4..=6 => {
            let s = format!("{} {} {}", r.pick(SUBJ), r.pick(VERB), r.pick(OBJ));
            let mut c = s.chars();
            c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
        }
        7 => format!("{} {} {}.\n{} {} {}?", r.pick(SUBJ), r.pick(VERB), r.pick(OBJ), r.pick(SUBJ), r.pick(VERB), r.pick(OBJ)),
        _ => r.pick(SHORT).to_string(),
    }
}

/// A subtree laid out with its top left corner at (0, 0).
struct Part {
    nodes: Vec<Node>,
    edges: Vec<(usize, usize, Kind)>,
    w: f32,
    h: f32,
}

struct Gen {
    r: Rng,
}

fn node(text: String, w: f32, h: f32, font: f32, shape: Shape) -> Node {
    Node { id: String::new(), x: 0.0, y: 0.0, w, h, text, font, shape, color: None, fill: None, extra: Map::new() }
}

impl Gen {
    /// One note at font size `f`.
    fn note(&mut self, f: f32) -> Node {
        let text = note_text(&mut self.r);
        let shape = match self.r.int(0, 19) {
            0 => Shape::Ellipse,
            1 => Shape::Diamond,
            2..=5 => Shape::Text,
            _ => Shape::Box,
        };
        let chars = text.chars().count() as f32;
        let w = f * self.r.range(7.0, 12.0).max((chars * 0.55 + 2.0).min(12.0)).max(5.0);
        let per_line = ((w - f) / (f * 0.55)).max(1.0);
        let lines = text.split('\n').map(|l| (l.chars().count() as f32 / per_line).ceil().max(1.0)).sum::<f32>();
        let k = if matches!(shape, Shape::Ellipse | Shape::Diamond) { 1.6 } else { 1.0 };
        let h = (lines * f * 1.25 + f * 1.2) * k;
        let mut n = node(text, w * k, h, f, shape);
        if shape != Shape::Text && self.r.chance(0.6) {
            n.fill = Some(self.r.pick(FILLS).to_string());
        }
        if self.r.chance(0.2) {
            n.color = Some(self.r.pick(INKS).to_string());
        }
        n
    }

    /// A frame titled `title` at font `f`, holding about `budget` concepts.
    fn frame(&mut self, title: String, f: f32, budget: usize, depth: usize) -> Part {
        let mut kids: Vec<Part> = Vec::new();
        let child_f = f / self.r.range(4.5, 7.0);
        if budget <= 45 || depth >= 4 {
            for _ in 0..budget.max(3) {
                let n = self.note(child_f);
                kids.push(Part { w: n.w, h: n.h, nodes: vec![n], edges: Vec::new() });
            }
        } else {
            // Areas, and a few loose notes beside them.
            let areas = self.r.int(4, 9).min(budget / 8).max(2);
            let loose = self.r.int(2, 6);
            let mut left = budget - loose;
            let mut names: Vec<&str> = if depth == 0 { PROJECTS.to_vec() } else { AREAS.to_vec() };
            for k in 0..areas {
                let share = if k + 1 == areas { left } else { (left as f32 / (areas - k) as f32 * self.r.range(0.5, 1.5)) as usize };
                let share = share.clamp(4, left.max(4));
                left = left.saturating_sub(share);
                let t = names.swap_remove(self.r.int(0, names.len() - 1)).to_string();
                kids.push(self.frame(t, child_f, share, depth + 1));
            }
            for _ in 0..loose {
                let n = self.note(child_f);
                kids.push(Part { w: n.w, h: n.h, nodes: vec![n], edges: Vec::new() });
            }
            // Mix loose notes in among the areas.
            for i in (1..kids.len()).rev() {
                let j = self.r.int(0, i);
                kids.swap(i, j);
            }
        }
        // Rows of children, aiming at a 4:3 box.
        let leaves = kids.iter().all(|k| k.nodes.len() == 1);
        let gap = child_f * if leaves { self.r.range(1.2, 2.2) } else { self.r.range(2.0, 3.0) };
        let area: f32 = kids.iter().map(|k| (k.w + gap * 1.3) * (k.h + gap * 1.3)).sum();
        let row_w = (area * if depth == 0 { 1.9 } else { 1.5 }).sqrt().max(kids.iter().map(|k| k.w).fold(0.0, f32::max));
        let (mut x, mut y, mut row_h, mut max_w) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
        let mut places = Vec::with_capacity(kids.len());
        for k in &kids {
            if x > 0.0 && x + k.w > row_w {
                x = 0.0;
                y += row_h + gap;
                row_h = 0.0;
            }
            let jx = self.r.range(0.0, gap * 0.3);
            let jy = self.r.range(0.0, gap * 0.4);
            places.push((x + jx, y + jy));
            x += k.w + gap + jx;
            row_h = row_h.max(k.h + jy);
            max_w = max_w.max(x - gap);
        }
        let content_h = y + row_h;
        let pad = f * 0.5;
        let title_h = f * 1.25 + f;
        let (w, h) = (max_w + 2.0 * pad + gap, content_h + title_h + pad + gap * 0.5);
        let mut out = Part { nodes: vec![node(title, w, h, f, Shape::Box)], edges: Vec::new(), w, h };
        if self.r.chance(0.25) {
            out.nodes[0].color = Some(self.r.pick(INKS).to_string());
        }
        let mut heads = Vec::new();
        for (k, (px, py)) in kids.into_iter().zip(places) {
            let base = out.nodes.len();
            heads.push(base);
            out.edges.push((0, base, Kind::Contains));
            for mut n in k.nodes {
                n.x += pad + gap * 0.5 + px;
                n.y += title_h + py;
                out.nodes.push(n);
            }
            out.edges.extend(k.edges.into_iter().map(|(a, b, kind)| (a + base, b + base, kind)));
        }
        // Arrows between neighbours, a hub or two, the odd jump across.
        let n = heads.len();
        for i in 1..n {
            if self.r.chance(0.55) {
                out.edges.push((heads[i - 1], heads[i], Kind::Link));
            }
        }
        if n > 6 && self.r.chance(0.35) {
            let hub = heads[self.r.int(0, n - 1)];
            for _ in 0..self.r.int(2, 5) {
                let t = heads[self.r.int(0, n - 1)];
                if t != hub {
                    out.edges.push((hub, t, Kind::Link));
                }
            }
        }
        for _ in 0..n / 20 {
            let (a, b) = (heads[self.r.int(0, n - 1)], heads[self.r.int(0, n - 1)]);
            if a != b {
                out.edges.push((a, b, Kind::Link));
            }
        }
        out
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let total: usize = args.next().and_then(|v| v.parse().ok()).unwrap_or(100_000);
    let path = args.next().unwrap_or_else(|| "big.hyperschema".into());
    let seed: u64 = args.next().and_then(|v| v.parse().ok()).unwrap_or(7);
    let mut g = Gen { r: Rng(0x9E37_79B9_7F4A_7C15 ^ seed.wrapping_mul(0x2545_F491_4F6C_DD1D)) };
    // Top level: one huge frame per region; the world is a few of them.
    let regions = (total / 25_000).clamp(1, 12);
    let mut b = Board::new();
    let mut ids = 0usize;
    let mut x0 = 0.0f32;
    let mut all_heads: Vec<(usize, f32)> = Vec::new();
    let mut parts = Vec::new();
    for k in 0..regions {
        let title = REGIONS[k % REGIONS.len()].to_string();
        parts.push(g.frame(title, 400_000.0, total / regions, 0));
    }
    let gap = 300_000.0;
    let row = (regions as f32).sqrt().ceil() as usize;
    let mut y0 = 0.0f32;
    let mut row_h = 0.0f32;
    for (k, p) in parts.into_iter().enumerate() {
        if k > 0 && k % row == 0 {
            x0 = 0.0;
            y0 += row_h + gap;
            row_h = 0.0;
        }
        let base = b.nodes.len();
        for mut n in p.nodes {
            n.x += x0;
            n.y += y0;
            n.id = format!("n{ids:07}");
            ids += 1;
            b.nodes.push(n);
        }
        for (a, c, kind) in p.edges {
            let arrow = kind == Kind::Link && g.r.chance(0.8);
            b.edges.push(Edge {
                id: format!("e{:07}", b.edges.len()),
                from: b.nodes[a + base].id.clone(),
                to: b.nodes[c + base].id.clone(),
                kind,
                arrow_from: false,
                arrow_to: arrow,
                label: String::new(),
                extra: Map::new(),
            });
        }
        x0 += p.w + gap;
        row_h = row_h.max(p.h);
        all_heads.extend((base..b.nodes.len()).map(|i| (i, b.nodes[i].font)));
    }
    // A few long links between notes of different projects.
    let notes: Vec<usize> = all_heads.iter().filter(|(_, f)| *f < 400.0).map(|(i, _)| *i).collect();
    for _ in 0..(total / 2000).max(3) {
        if notes.len() < 2 {
            break;
        }
        let (a, c) = (notes[g.r.int(0, notes.len() - 1)], notes[g.r.int(0, notes.len() - 1)]);
        if a != c {
            b.edges.push(Edge {
                id: format!("e{:07}", b.edges.len()),
                from: b.nodes[a].id.clone(),
                to: b.nodes[c].id.clone(),
                kind: Kind::Link,
                arrow_from: false,
                arrow_to: true,
                label: String::new(),
                extra: Map::new(),
            });
        }
    }
    b.reindex();
    let links = b.edges.iter().filter(|e| e.kind == Kind::Link).count();
    std::fs::write(&path, b.serialize()).unwrap();
    println!("{} concepts, {} links -> {path}", b.nodes.len(), links);
}
