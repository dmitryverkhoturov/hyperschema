//! Platform-free editor shared by the Windows and Android shells: concepts
//! nest by dropping one inside another, connections run centre to centre
//! around obstacles, every finished action is one undo step.
//!
//! The shell feeds pointer positions (`cursor`, screen pixels) and presses;
//! the editor owns the engine, the board, selection and the gesture mode.
//! Committed steps collect in `done` for the shell to save into history.

use crate::board::{contains, Bbox, Board, Change, Kind, Node, Shape as NShape, Snapshot};
use crate::route::{self, midpoint, seg_dist, Pt};
use crate::scene::{rgba, Align, Label, Line, Rect, Scene, Shape};
use crate::text::LINE_H;
use crate::{view, Engine};
use serde_json::Map;
use std::collections::{HashMap, HashSet};
use std::time::Instant;

/// A pointer position in screen pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Pos {
    pub x: f64,
    pub y: f64,
}

/// Something the shell must show and answer (a native menu, a bar).
pub enum Request {
    /// The connection's middle handle was tapped: direction, label, delete
    /// (answer with `apply_edge_choice`).
    EdgeMenu { edge: String, x: f64, y: f64 },
    /// Right click on a concept: colour, shape and commands for the
    /// selection (answer with `apply_node_choice`, see `NODE_MENU_*`).
    NodeMenu { x: f64, y: f64 },
    /// Right click on empty canvas: a new concept or paste there.
    CanvasMenu { x: f64, y: f64 },
}

/// Fill colours offered for concepts (none first): name, colour.
pub const PALETTE: [(&str, Option<&str>); 8] = [
    ("Без заливки", None),
    ("Серый", Some("#e9ecef")),
    ("Красный", Some("#ffc9c9")),
    ("Оранжевый", Some("#ffd8a8")),
    ("Жёлтый", Some("#fff3bf")),
    ("Зелёный", Some("#b2f2bb")),
    ("Голубой", Some("#a5d8ff")),
    ("Фиолетовый", Some("#d0bfff")),
];

/// Menu choices: `NODE_COLOR + k` picks `PALETTE[k]`, `NODE_SHAPE + k`
/// picks box, ellipse, diamond, bare text.
pub const NODE_COLOR: i32 = 10;
pub const NODE_SHAPE: i32 = 20;
pub const NODE_EDIT: i32 = 30;
pub const NODE_DUPLICATE: i32 = 31;
pub const NODE_WRAP: i32 = 32;
pub const NODE_DELETE: i32 = 33;
pub const NODE_DELETE_DEEP: i32 = 34;
pub const CANVAS_NEW: i32 = 40;

const BLUE: (u8, u8, u8) = (0x4a, 0x7d, 0xff);

fn blue(a: u8) -> u32 {
    rgba(BLUE.0, BLUE.1, BLUE.2, a)
}

/// Resize handle: which edges move (bit 0 left, 1 right, 2 top, 3 bottom).
pub type Handle = u8;
const L: u8 = 1;
const R: u8 = 2;
const T: u8 = 4;
const B: u8 = 8;
const HANDLES: [Handle; 8] = [L | T, T, R | T, R, R | B, B, L | B, L];

#[derive(Clone, PartialEq, Debug)]
pub enum TextTarget {
    Node(String),
    Edge(String),
}

pub enum Mode {
    Idle,
    /// Left-drag on empty canvas; a release without moving deselects.
    Pan { moved: bool, at: Pos },
    /// Pressed, not yet sure whether it is a click or a drag.
    Press { at: Pos, node: Option<String>, double: bool, edge_mid: Option<String> },
    Move { start: (f64, f64), roots: Vec<String>, base: Board, target: Option<String>, guides: Vec<(Pt, Pt)> },
    Boxing { from: (f64, f64), add: bool },
    Resize { id: String, handle: Handle, base: Board, start: (f64, f64) },
    Connect { from: String },
    EdgeEnd { edge: String, moving_from: bool },
    Text { target: TextTarget, text: String, created: bool },
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum TextKey {
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    Backspace,
    Delete,
}

/// Ctrl+F: the query, what matches it and which match is shown.
#[derive(Default)]
pub struct Search {
    pub query: String,
    pub hits: Vec<TextTarget>,
    pub at: usize,
}

/// A camera flight: from, to, start, length.
struct Flight {
    from: (f64, f64, f64),
    to: (f64, f64, f64),
    start: Instant,
    ms: f64,
}

pub enum Hit {
    Handle(String, Handle),
    Dot(String),
    EdgeEnd(String, bool),
    EdgeMid(String),
    Edge(String),
    /// A node by something that moves it (a leaf anywhere, a parent by title or border).
    Node(String),
    /// The empty inside of a parent.
    Inside(String),
    Empty,
}

pub struct Editor {
    pub engine: Engine,
    pub board: Board,
    /// History preview shown instead of the board (read-only while set).
    pub preview: Option<Board>,
    /// The timeline is open: no editing.
    pub frozen: bool,
    pub sel: Vec<String>,
    pub sel_edge: Option<String>,
    pub hover: Option<String>,
    pub mode: Mode,
    pub routes: HashMap<String, Vec<Pt>>,
    /// Routes before lanes, kept so a drag re-routes only what it touches.
    raw_routes: HashMap<String, Vec<Pt>>,
    /// Show connections between a concept and what is nested in it.
    pub show_nested_links: bool,
    /// The pointer moved during a move or resize; applied once per frame.
    drag_pending: bool,
    pub hidden_nodes: HashSet<String>,
    pub hidden_edges: HashSet<String>,
    pub cursor: Pos,
    /// Shift held (pure scaling while resizing, adding to the selection).
    pub shift: bool,
    /// Device pixels per layout pixel (Windows scale factor, Android density).
    pub ui: f32,
    /// Finger input: bigger targets, handles only on the selection.
    pub touch: bool,
    pub caret_epoch: Instant,
    /// Caret and selection anchor in the text being edited, in characters.
    pub caret: usize,
    pub anchor: usize,
    /// The mouse is selecting text (pressed inside the text being edited).
    text_drag: bool,
    /// Size of the concept when its text editing began: typing grows it,
    /// erasing shrinks it back, never below this.
    text_box: Option<(f32, f32)>,
    /// Where the last context menu was asked for, in the world.
    menu_at: Pt,
    /// The button down is the right one (its click opens a menu).
    right_press: bool,
    pub search: Option<Search>,
    flight: Option<Flight>,
    pub undo_stack: Vec<Change>,
    pub redo_stack: Vec<Change>,
    pub step_base: Option<Snapshot>,
    /// Steps committed since the shell last took them (for history and saving).
    pub done: Vec<Change>,
    pub request: Option<Request>,
}

impl Editor {
    pub fn new(engine: Engine, ui: f32, touch: bool) -> Editor {
        Editor {
            engine,
            board: Board::new(),
            preview: None,
            frozen: false,
            sel: Vec::new(),
            sel_edge: None,
            hover: None,
            mode: Mode::Idle,
            routes: HashMap::new(),
            raw_routes: HashMap::new(),
            show_nested_links: false,
            drag_pending: false,
            hidden_nodes: HashSet::new(),
            hidden_edges: HashSet::new(),
            cursor: Pos { x: -1.0, y: -1.0 },
            shift: false,
            ui,
            touch,
            caret_epoch: Instant::now(),
            caret: 0,
            anchor: 0,
            text_drag: false,
            text_box: None,
            menu_at: (0.0, 0.0),
            right_press: false,
            search: None,
            flight: None,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            step_base: None,
            done: Vec::new(),
            request: None,
        }
    }

    /// Replaces the board (a file was opened): selection and steps reset.
    pub fn load(&mut self, board: Board) {
        self.board = board;
        self.preview = None;
        self.sel.clear();
        self.sel_edge = None;
        self.hover = None;
        self.mode = Mode::Idle;
        self.hidden_nodes.clear();
        self.hidden_edges.clear();
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.step_base = None;
        self.rebuild();
    }

    pub fn world(&self) -> (f32, f32) {
        let (x, y) = self.engine.screen_to_world(self.cursor.x, self.cursor.y);
        (x as f32, y as f32)
    }

    /// One screen pixel in world units.
    pub fn px(&self) -> f32 {
        (1.0 / self.engine.cam.s) as f32
    }

    /// One layout pixel (device pixels / scale) in world units.
    fn dp(&self) -> f32 {
        self.px() * self.ui
    }

    fn tol(&self) -> f32 {
        self.dp() * if self.touch { 14.0 } else { 6.0 }
    }

    /// How far from a handle or dot a press still grabs it.
    fn reach(&self) -> f32 {
        self.dp() * if self.touch { 24.0 } else { 10.0 }
    }

    /// Resize handles: corners only for fingers (sides crowd small concepts).
    fn handle_set(&self) -> Vec<Handle> {
        if self.touch {
            vec![L | T, R | T, R | B, L | B]
        } else {
            HANDLES.to_vec()
        }
    }

    fn handle_r(&self) -> f32 {
        self.dp() * if self.touch { 7.0 } else { 4.5 }
    }

    /// Selected concepts whose ancestors are not selected too.
    fn sel_roots(&self) -> Vec<String> {
        let b = self.shown();
        let sel: HashSet<&String> = self.sel.iter().collect();
        self.sel.iter().filter(|id| b.get(id).is_some() && !b.ancestors(id).iter().any(|a| sel.contains(a))).cloned().collect()
    }

    /// What the resize handles frame: one concept, or the whole selection
    /// (empty id) when several are selected.
    fn handle_box(&self) -> Option<(String, Bbox)> {
        if !self.handles_visible() {
            return None;
        }
        match &self.sel[..] {
            [] => None,
            [one] => self.shown().get(one).map(|n| (one.clone(), n.bbox())),
            _ => self.shown().bounds_of(self.sel_roots().iter()).map(|b| (String::new(), b)),
        }
    }

    fn handle_pad(&self, id: &str) -> f32 {
        self.dp() * if id.is_empty() { 10.0 } else { 4.0 }
    }

    /// Concepts showing connection dots: the single selected one, and the
    /// hovered one with a mouse.
    fn dot_nodes(&self) -> Vec<String> {
        let mut v = Vec::new();
        if let [one] = &self.sel[..] {
            v.push(one.clone());
        }
        if !self.touch {
            if let Some(h) = &self.hover {
                if !v.contains(h) {
                    v.push(h.clone());
                }
            }
        }
        v
    }

    fn dot_off(&self) -> f32 {
        self.dp() * if self.touch { 30.0 } else { 18.0 }
    }

    fn sf(&self) -> f32 {
        self.ui
    }

    pub fn lines(&self, text: &str, size: f32, w: f32) -> usize {
        self.engine.wrapped_lines(text, size, w)
    }

    fn title_lines(&self, n: &Node) -> usize {
        view::title_lines(n, &|t, s, w| self.engine.wrapped_lines(t, s, w))
    }

    /// The board on screen: the timeline preview while browsing history.
    pub fn shown(&self) -> &Board {
        self.preview.as_ref().unwrap_or(&self.board)
    }

    // ------------------------------------------------------------ scene

    /// Rebuilds routes and the indexed scene, minus what the overlay draws.
    pub fn rebuild(&mut self) {
        let board = self.shown().clone();
        self.raw_routes = route::raw_routes(&board, self.show_nested_links, None, &HashMap::new());
        self.routes = route::finish(&board, &self.raw_routes);
        let m = |t: &str, s: f32, w: f32| self.engine.wrapped_lines(t, s, w);
        let skip = view::Skip { nodes: &self.hidden_nodes, edges: &self.hidden_edges };
        let scene = view::build(&board, &self.routes, &m, &skip, view::bg_color());
        self.engine.set_scene(scene);
    }

    fn hide(&mut self, nodes: HashSet<String>, edges: HashSet<String>) {
        self.hidden_nodes = nodes;
        self.hidden_edges = edges;
        self.rebuild();
    }

    fn unhide(&mut self) {
        self.hide(HashSet::new(), HashSet::new());
    }

    /// Applies the latest pointer position of a move or resize.
    fn flush_drag(&mut self) {
        if !std::mem::take(&mut self.drag_pending) {
            return;
        }
        let t = Instant::now();
        match self.mode {
            Mode::Move { .. } => self.drag_move(),
            Mode::Resize { .. } => self.drag_resize(),
            _ => {}
        }
        if let Some(p) = std::env::var_os("HS_PROF") {
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new().append(true).create(true).open(p) {
                let _ = writeln!(f, "drag {:.2} ms", t.elapsed().as_secs_f64() * 1000.0);
            }
        }
    }

    /// Selection, handles, things being dragged, caret, guides. Called once
    /// per frame, so it also applies a pending drag.
    pub fn refresh_overlay(&mut self) {
        self.flush_drag();
        self.fly();
        let mut ov = Scene::default();
        let px = self.px();
        let board = self.shown().clone();
        let m = |t: &str, s: f32, w: f32| self.engine.wrapped_lines(t, s, w);
        // Live elements.
        for id in board.draw_order() {
            if self.hidden_nodes.contains(&id) {
                view::node_into(&board, board.node(&id), &m, &mut ov);
            }
        }
        for eid in &self.hidden_edges {
            let Some(e) = board.edge(eid) else { continue };
            if e.kind != Kind::Link {
                continue;
            }
            let (Some(a), Some(b)) = (board.get(&e.from), board.get(&e.to)) else { continue };
            let p = route::route_between(&board, a, b);
            view::edge_into(&board, eid, &p, &m, view::bg_color(), &mut ov);
        }
        let outline = |ov: &mut Scene, b: Bbox, pad: f32, c: u32, w: f32| {
            ov.rects.push(Rect { x: b[0] - pad, y: b[1] - pad, w: b[2] - b[0] + 2.0 * pad, h: b[3] - b[1] + 2.0 * pad, fill: 0, stroke: c, stroke_w: w, radius: 0.0, shape: Shape::Rect });
        };
        let dot = |ov: &mut Scene, p: Pt, r: f32, fill: u32| {
            ov.rects.push(Rect { x: p.0 - r, y: p.1 - r, w: 2.0 * r, h: 2.0 * r, fill, stroke: blue(255), stroke_w: 1.5 * px, radius: 0.0, shape: Shape::Ellipse });
        };
        let dp = self.dp();
        if let Some(sr) = &self.search {
            for (k, h) in sr.hits.iter().enumerate() {
                if let TextTarget::Node(id) = h {
                    if let Some(n) = board.get(id) {
                        let (a, w) = if k == sr.at { (255, 3.0) } else { (170, 2.0) };
                        outline(&mut ov, n.bbox(), 7.0 * dp, rgba(0xff, 0xb0, 0x00, a), w * dp);
                    }
                }
            }
        }
        for id in &self.sel {
            if let Some(n) = board.get(id) {
                outline(&mut ov, n.bbox(), 4.0 * dp, blue(255), 1.5 * dp);
            }
        }
        if let Some((id, b)) = self.handle_box() {
            let pad = self.handle_pad(&id);
            if id.is_empty() {
                outline(&mut ov, b, pad, blue(120), 1.0 * dp);
            }
            {
                for h in self.handle_set() {
                    let p = handle_pos(&b, h, pad);
                    let r = self.handle_r();
                    ov.rects.push(Rect { x: p.0 - r, y: p.1 - r, w: 2.0 * r, h: 2.0 * r, fill: rgba(255, 255, 255, 255), stroke: blue(255), stroke_w: 1.5 * px, radius: 0.0, shape: Shape::Rect });
                }
            }
        }
        if let Some(eid) = &self.sel_edge {
            if let Some(p) = self.routes.get(eid).cloned().or_else(|| self.live_route(eid)) {
                for w in p.windows(2) {
                    ov.lines.push(Line { x0: w[0].0, y0: w[0].1, x1: w[1].0, y1: w[1].1, color: blue(90), width: 7.0 * dp, head: false });
                }
                if !matches!(self.mode, Mode::EdgeEnd { .. }) {
                    let r = self.handle_r() * 1.2;
                    dot(&mut ov, p[0], r, rgba(255, 255, 255, 255));
                    dot(&mut ov, p[p.len() - 1], r, rgba(255, 255, 255, 255));
                    dot(&mut ov, midpoint(&p), r, blue(255));
                }
            }
        }
        // Connection dots: drag one out to connect (or to make a new concept).
        if matches!(self.mode, Mode::Idle | Mode::Press { .. }) && !self.frozen {
            for id in self.dot_nodes() {
                if let Some(n) = board.get(&id) {
                    for d in connect_dots(n, self.dot_off()) {
                        dot(&mut ov, d, self.handle_r() * 1.2, blue(255));
                    }
                }
            }
        }
        match &self.mode {
            Mode::Text { target, text, .. } => {
                if let Some(l) = self.text_label(&board, target, text) {
                    let slots = self.engine.caret_slots(&l);
                    let lh = LINE_H * l.size;
                    let (a, b) = (self.anchor.min(self.caret).min(slots.len() - 1), self.anchor.max(self.caret).min(slots.len() - 1));
                    // Selection: one band per line it spans.
                    let mut k = a;
                    while k < b {
                        let line = slots[k].2;
                        let mut e = k;
                        while e < b && slots[e + 1].2 == line {
                            e += 1;
                        }
                        let (x0, x1) = (slots[k].0.min(slots[e].0), slots[k].0.max(slots[e].0));
                        ov.rects.push(Rect { x: x0, y: slots[k].1, w: (x1 - x0).max(l.size * 0.25), h: lh, fill: blue(70), stroke: 0, stroke_w: 0.0, radius: 0.0, shape: Shape::Rect });
                        k = e + 1;
                    }
                    if (self.caret_epoch.elapsed().as_millis() / 530) % 2 == 0 || self.text_drag {
                        let (x, y, _) = slots[self.caret.min(slots.len() - 1)];
                        ov.rects.push(Rect { x, y, w: (1.5 * px).max(l.size * 0.06), h: lh, fill: blue(255), stroke: 0, stroke_w: 0.0, radius: 0.0, shape: Shape::Rect });
                    }
                }
            }
            Mode::Boxing { from, .. } => {
                let (x, y) = self.world();
                let b = [(from.0 as f32).min(x), (from.1 as f32).min(y), (from.0 as f32).max(x), (from.1 as f32).max(y)];
                ov.rects.push(Rect { x: b[0], y: b[1], w: b[2] - b[0], h: b[3] - b[1], fill: blue(22), stroke: blue(200), stroke_w: 1.0 * px, radius: 0.0, shape: Shape::Rect });
            }
            Mode::Move { target, guides, .. } => {
                if let Some(t) = target.as_ref().and_then(|t| board.get(t)) {
                    outline(&mut ov, t.bbox(), 2.0 * px, blue(160), 2.5 * px);
                }
                for (a, b) in guides {
                    ov.lines.push(Line { x0: a.0, y0: a.1, x1: b.0, y1: b.1, color: rgba(0xff, 0x3b, 0x8d, 220), width: 1.0 * px, head: false });
                }
            }
            Mode::Connect { from } => {
                if let Some(a) = board.get(from) {
                    let p = self.world();
                    let target = self.connect_target(from);
                    let end = target.as_ref().and_then(|t| board.get(t)).map_or(p, |t| t.center());
                    let path = route::clip(&[a.center(), end], &a.bbox(), &target.as_ref().and_then(|t| board.get(t)).map_or([p.0, p.1, p.0, p.1], |t| t.bbox()));
                    for w in path.windows(2) {
                        ov.lines.push(Line { x0: w[0].0, y0: w[0].1, x1: w[1].0, y1: w[1].1, color: blue(255), width: 2.0 * px, head: false });
                    }
                    if let Some(t) = target.as_ref().and_then(|t| board.get(t)) {
                        outline(&mut ov, t.bbox(), 3.0 * px, blue(200), 2.0 * px);
                    }
                }
            }
            Mode::EdgeEnd { edge, moving_from } => {
                if let Some(e) = board.edge(edge) {
                    let fixed = if *moving_from { &e.to } else { &e.from };
                    if let Some(a) = board.get(fixed) {
                        let p = self.world();
                        let target = self.connect_target(fixed);
                        let end = target.as_ref().and_then(|t| board.get(t)).map_or(p, |t| t.center());
                        for w in route::clip(&[a.center(), end], &a.bbox(), &[end.0, end.1, end.0, end.1]).windows(2) {
                            ov.lines.push(Line { x0: w[0].0, y0: w[0].1, x1: w[1].0, y1: w[1].1, color: blue(255), width: 2.0 * px, head: false });
                        }
                        if let Some(t) = target.as_ref().and_then(|t| board.get(t)) {
                            outline(&mut ov, t.bbox(), 3.0 * px, blue(200), 2.0 * px);
                        }
                    }
                }
            }
            _ => {}
        }
        self.engine.set_overlay(&ov);
    }

    /// The label showing `text` for a text target, laid out as drawn.
    fn text_label(&self, board: &Board, target: &TextTarget, text: &str) -> Option<Label> {
        let m = |t: &str, s: f32, w: f32| self.engine.wrapped_lines(t, s, w);
        match target {
            TextTarget::Node(id) => board.get(id).and_then(|n| caret_label(board, n, text, &m)),
            TextTarget::Edge(eid) => self.routes.get(eid).map(|p| {
                let font = board.edge(eid).map_or(20.0, |e| view::edge_font(board, &e.from, &e.to)) * 0.8;
                let c = midpoint(p);
                let lines = m(text, font, font * 12.0).max(1) as f32;
                Label { x: c.0 - font * 6.0, y: c.1 - lines * LINE_H * font * 0.5, w: font * 12.0, size: font, color: 0, align: Align::Center, wrap: true, text: text.to_string() }
            }),
        }
    }

    fn live_route(&self, eid: &str) -> Option<Vec<Pt>> {
        let b = self.shown();
        let e = b.edge(eid)?;
        Some(route::route_between(b, b.get(&e.from)?, b.get(&e.to)?))
    }

    fn handles_visible(&self) -> bool {
        matches!(self.mode, Mode::Idle | Mode::Resize { .. } | Mode::Press { .. }) && !self.frozen
    }

    /// True while the overlay changes by itself (caret blink).
    pub fn animating(&self) -> bool {
        matches!(self.mode, Mode::Text { .. }) || self.flight.is_some()
    }

    // ------------------------------------------------------------ search

    /// Ctrl+F: opens the search (empty query).
    pub fn search_open(&mut self) {
        self.commit_text();
        if self.search.is_none() {
            self.search = Some(Search::default());
        }
    }

    pub fn search_close(&mut self) {
        self.search = None;
    }

    /// New query: matches in concept text and connection labels, ignoring
    /// case, in reading order (top to bottom, then left to right); flies to
    /// the first.
    pub fn search_set(&mut self, q: &str) {
        let Some(sr) = &mut self.search else { return };
        sr.query = q.to_string();
        let ql = q.trim().to_lowercase();
        let mut hits: Vec<(f32, f32, TextTarget)> = Vec::new();
        if !ql.is_empty() {
            for n in &self.board.nodes {
                if n.text.to_lowercase().contains(&ql) {
                    hits.push((n.y, n.x, TextTarget::Node(n.id.clone())));
                }
            }
            for e in &self.board.edges {
                if e.kind == Kind::Link && e.label.to_lowercase().contains(&ql) {
                    if let Some(p) = self.routes.get(&e.id) {
                        let c = midpoint(p);
                        hits.push((c.1, c.0, TextTarget::Edge(e.id.clone())));
                    }
                }
            }
        }
        hits.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1)));
        sr.hits = hits.into_iter().map(|h| h.2).collect();
        sr.at = 0;
        self.search_show();
    }

    /// Enter / Shift+Enter: the next or previous match.
    pub fn search_step(&mut self, back: bool) {
        let Some(sr) = &mut self.search else { return };
        let n = sr.hits.len();
        if n == 0 {
            return;
        }
        sr.at = if back { (sr.at + n - 1) % n } else { (sr.at + 1) % n };
        self.search_show();
    }

    /// Selects the current match and flies the camera to it.
    fn search_show(&mut self) {
        let Some(hit) = self.search.as_ref().and_then(|sr| sr.hits.get(sr.at).cloned()) else { return };
        let (b, font) = match &hit {
            TextTarget::Node(id) => {
                let Some(n) = self.board.get(id) else { return };
                self.sel = vec![id.clone()];
                self.sel_edge = None;
                (n.bbox(), n.font)
            }
            TextTarget::Edge(eid) => {
                let (Some(p), Some(e)) = (self.routes.get(eid), self.board.edge(eid)) else { return };
                let c = midpoint(p);
                let f = view::edge_font(&self.board, &e.from, &e.to);
                self.sel.clear();
                self.sel_edge = Some(eid.clone());
                ([c.0 - f * 4.0, c.1 - f, c.0 + f * 4.0, c.1 + f], f)
            }
        };
        // Keep the zoom if the text reads well, otherwise make it about
        // 14 px; never so close that the match does not fit.
        let (w, h) = self.engine.size();
        let ui = self.ui as f64;
        let cam = &self.engine.cam;
        let fit = ((w as f64 / (b[2] - b[0]).max(1.0) as f64).min(h as f64 / (b[3] - b[1]).max(1.0) as f64)) * 0.8;
        let s = cam.s.clamp(14.0 * ui / font as f64, 32.0 * ui / font as f64).min(fit);
        let to = ((b[0] + b[2]) as f64 * 0.5, (b[1] + b[3]) as f64 * 0.5, s);
        self.flight = Some(Flight { from: (cam.cx, cam.cy, cam.s), to, start: Instant::now(), ms: 450.0 });
    }

    /// Moves the camera along a flight; called once per frame.
    fn fly(&mut self) {
        let Some(f) = &self.flight else { return };
        let t = (f.start.elapsed().as_secs_f64() * 1000.0 / f.ms).min(1.0);
        let e = if t < 0.5 { 4.0 * t * t * t } else { 1.0 - (-2.0 * t + 2.0).powi(3) / 2.0 };
        // Scale moves geometrically; the centre so that the screen point
        // of the target travels evenly.
        let s = f.from.2 * (f.to.2 / f.from.2).powf(e);
        let k = if (f.to.2 - f.from.2).abs() < 1e-9 { e } else { (1.0 / f.from.2 - 1.0 / s) / (1.0 / f.from.2 - 1.0 / f.to.2) };
        let cam = &mut self.engine.cam;
        cam.s = s;
        cam.cx = f.from.0 + (f.to.0 - f.from.0) * k;
        cam.cy = f.from.1 + (f.to.1 - f.from.1) * k;
        if t >= 1.0 {
            self.flight = None;
        }
    }

    // ------------------------------------------------------------ hit testing

    pub fn hit(&self) -> Hit {
        let board = self.shown();
        let p = self.world();
        let (tol, reach) = (self.tol(), self.reach());
        if let Some((id, b)) = self.handle_box() {
            let pad = self.handle_pad(&id);
            for h in self.handle_set() {
                let q = handle_pos(&b, h, pad);
                if (q.0 - p.0).abs() < reach * 0.8 && (q.1 - p.1).abs() < reach * 0.8 {
                    return Hit::Handle(id, h);
                }
            }
        }
        if let Some(eid) = &self.sel_edge {
            if let Some(path) = self.routes.get(eid) {
                let near = |q: Pt| (q.0 - p.0).hypot(q.1 - p.1) < reach;
                if near(path[0]) {
                    return Hit::EdgeEnd(eid.clone(), true);
                }
                if near(path[path.len() - 1]) {
                    return Hit::EdgeEnd(eid.clone(), false);
                }
                if near(midpoint(path)) {
                    return Hit::EdgeMid(eid.clone());
                }
            }
        }
        for id in self.dot_nodes() {
            if let Some(n) = board.get(&id) {
                if connect_dots(n, self.dot_off()).iter().any(|d| (d.0 - p.0).hypot(d.1 - p.1) < reach) {
                    return Hit::Dot(id);
                }
            }
        }
        // Text concepts sit on top of whatever they overlap.
        for n in board.nodes.iter().rev() {
            if n.shape == NShape::Text && inside(&n.bbox(), p, tol) {
                return Hit::Node(n.id.clone());
            }
        }
        // A concept under the pointer wins over a connection crossing it:
        // a press usually means grabbing, and zoomed out connections cover
        // much of the screen.
        let under = match board.deepest_at(p.0, p.1, &HashSet::new()) {
            None => Hit::Empty,
            Some(id) => {
                let n = board.node(&id);
                let b = n.bbox();
                let border = p.0 - b[0] < 1.5 * tol || b[2] - p.0 < 1.5 * tol || p.1 - b[1] < 1.5 * tol || b[3] - p.1 < 1.5 * tol;
                let own = if board.has_children(&id) { p.1 < n.y + board.title_h(n, self.title_lines(n)) } else { !self.roomy_spot(n, p) };
                if border || own {
                    return Hit::Node(id);
                }
                Hit::Inside(id)
            }
        };
        for e in board.edges.iter().filter(|e| e.kind == Kind::Link) {
            if let Some(path) = self.routes.get(&e.id) {
                if path.windows(2).any(|w| seg_dist(p, w[0], w[1]) < tol) {
                    return Hit::Edge(e.id.clone());
                }
            }
        }
        under
    }

    /// A leaf big on screen has room for concepts inside: away from its
    /// text it acts like a parent's empty inside (pan, double-click or long
    /// press creates a nested concept).
    fn roomy_spot(&self, n: &Node, p: Pt) -> bool {
        n.shape != NShape::Text && n.w.min(n.h) >= 200.0 * self.dp() && !self.on_text(n, p)
    }

    /// Whether a double click at the pointer on leaf `id` nests a new
    /// concept in it rather than editing its text: off the text, and the
    /// leaf is big enough on screen to aim at a spot in it.
    fn nest_spot(&self, id: &str) -> bool {
        let board = self.shown();
        let Some(n) = board.get(id) else { return false };
        n.shape != NShape::Text && !board.has_children(id) && n.w.min(n.h) >= 48.0 * self.dp() && !self.on_text(n, self.world())
    }

    /// Whether `p` is on the text of leaf `n`: the box the text covers plus
    /// a margin (an empty leaf: a short line at its centre).
    fn on_text(&self, n: &Node, p: Pt) -> bool {
        let m = |t: &str, s: f32, w: f32| self.engine.wrapped_lines(t, s, w);
        let Some(l) = caret_label(self.shown(), n, &n.text, &m) else { return false };
        let lh = LINE_H * l.size;
        let pad = (0.35 * l.size).max(6.0 * self.dp());
        let zone = if n.text.trim().is_empty() {
            let (cx, cy) = (n.x + n.w * 0.5, n.y + n.h * 0.5);
            [cx - 2.0 * l.size, cy - lh * 0.5, cx + 2.0 * l.size, cy + lh * 0.5]
        } else {
            let slots = self.engine.caret_slots(&l);
            let (x0, x1) = slots.iter().fold((f32::MAX, f32::MIN), |(a, b), s| (a.min(s.0), b.max(s.0)));
            let lines = slots.iter().map(|s| s.2).max().unwrap_or(0) as f32 + 1.0;
            [x0, l.y, x1, l.y + lines * lh]
        };
        inside(&zone, p, pad)
    }

    /// A concept a connection from `from` may attach to, under the pointer.
    fn connect_target(&self, from: &str) -> Option<String> {
        let board = self.shown();
        let p = self.world();
        let tol = self.tol();
        let t = board
            .nodes
            .iter()
            .rev()
            .find(|n| n.shape == NShape::Text && inside(&n.bbox(), p, tol))
            .map(|n| n.id.clone())
            .or_else(|| board.deepest_at(p.0, p.1, &HashSet::new()))?;
        (t != from && !board.is_ancestor(&t, from) && !board.is_ancestor(from, &t)).then_some(t)
    }

    /// Returns true if the hover changed (redraw).
    pub fn update_hover(&mut self) -> bool {
        if !matches!(self.mode, Mode::Idle) || self.frozen {
            return false;
        }
        let new = match self.hit() {
            Hit::Node(id) | Hit::Handle(id, _) => {
                let n = self.shown().get(&id);
                n.filter(|n| n.shape != NShape::Text).map(|n| n.id.clone())
            }
            Hit::Dot(id) => Some(id),
            // On the way from a concept to one of its dots the pointer
            // crosses empty canvas: keep the hover so the dots stay put.
            Hit::Empty | Hit::Inside(_) => {
                let p = self.world();
                let keep = self.dot_off() + self.reach();
                self.hover.clone().filter(|h| self.shown().get(h).is_some_and(|n| inside(&n.bbox(), p, keep)))
            }
            _ => None,
        };
        if new != self.hover {
            self.hover = new;
            return true;
        }
        false
    }

    // ------------------------------------------------------------ steps

    /// Remembers the board before an action, once.
    pub fn begin(&mut self) {
        if self.step_base.is_none() {
            self.step_base = Some(self.board.snapshot());
        }
    }

    /// Ends an action: one undo step, saved shortly after.
    pub fn commit(&mut self) {
        let Some(base) = self.step_base.take() else { return };
        if let Some(c) = self.board.diff(&base) {
            self.undo_stack.push(c.clone());
            self.redo_stack.clear();
            self.done.push(c);
        }
        self.sel.retain(|id| self.board.get(id).is_some());
        if self.sel_edge.as_ref().is_some_and(|e| self.board.edge(e).is_none()) {
            self.sel_edge = None;
        }
        self.unhide();
    }

    pub fn undo(&mut self, redo: bool) {
        self.commit_text();
        let Some(c) = (if redo { self.redo_stack.pop() } else { self.undo_stack.pop() }) else { return };
        self.board.apply(&c, redo);
        let mut done = c.clone();
        if !redo {
            // History records what happened: the undo is a step of its own.
            done = Change { t: 0, dev: String::new(), nodes: invert(&c.nodes), edges: invert(&c.edges) };
            self.redo_stack.push(c);
        } else {
            self.undo_stack.push(c);
        }
        done.t = now_ms();
        self.done.push(done);
        self.sel.retain(|id| self.board.get(id).is_some());
        self.sel_edge = None;
        self.rebuild();
    }

    // ------------------------------------------------------------ input

    /// Left button / finger down. `double`: the second press of a double click.
    pub fn press(&mut self, alt: bool, shift: bool, double: bool) {
        self.right_press = false;
        // On the text being edited a press moves the caret (a drag selects,
        // a double click selects a word). Fingers edit through the native
        // text field, which owns the caret there.
        if !self.touch && !self.frozen {
            if let Some(k) = self.slot_at_pointer(true) {
                if double {
                    self.select_word(k);
                } else {
                    self.caret = k;
                    if !shift {
                        self.anchor = k;
                    }
                    self.text_drag = true;
                }
                self.caret_epoch = Instant::now();
                return;
            }
        }
        self.commit_text();
        // Hover is not tracked while typing: find what is under the pointer now.
        self.update_hover();
        if self.frozen {
            self.mode = Mode::Pan { moved: true, at: self.cursor };
            return;
        }
        let hit = self.hit();
        let at = self.cursor;
        if double {
            match hit {
                // A leaf off its text: a concept nested in it, like a parent's inside.
                Hit::Node(id) if self.nest_spot(&id) => {
                    let p = self.world();
                    self.create_at(p, Some(id));
                }
                Hit::Node(id) => {
                    self.start_text(TextTarget::Node(id), false);
                    if !self.touch {
                        if let Some(k) = self.slot_at_pointer(false) {
                            (self.caret, self.anchor) = (k, k);
                        }
                    }
                }
                Hit::Handle(id, _) if !id.is_empty() => self.start_text(TextTarget::Node(id), false),
                Hit::Edge(e) | Hit::EdgeMid(e) | Hit::EdgeEnd(e, _) => self.start_text(TextTarget::Edge(e), false),
                _ => self.mode = Mode::Press { at, node: None, double: true, edge_mid: None },
            }
            return;
        }
        match hit {
            Hit::Handle(id, h) => {
                self.begin();
                let start = self.engine.screen_to_world(self.cursor.x, self.cursor.y);
                self.mode = Mode::Resize { id, handle: h, base: self.board.clone(), start };
            }
            Hit::Dot(id) => self.mode = Mode::Connect { from: id },
            Hit::Node(id) if alt => self.mode = Mode::Connect { from: id },
            Hit::EdgeEnd(e, from) => self.mode = Mode::EdgeEnd { edge: e, moving_from: from },
            Hit::EdgeMid(e) => self.mode = Mode::Press { at, node: None, double: false, edge_mid: Some(e) },
            Hit::Edge(e) => {
                self.sel.clear();
                self.sel_edge = Some(e);
            }
            Hit::Node(id) => {
                self.sel_edge = None;
                if shift {
                    if let Some(k) = self.sel.iter().position(|s| *s == id) {
                        self.sel.remove(k);
                    } else {
                        self.sel.push(id.clone());
                    }
                } else if !self.sel.contains(&id) {
                    self.sel = vec![id.clone()];
                }
                self.mode = Mode::Press { at, node: Some(id), double: false, edge_mid: None };
            }
            Hit::Inside(_) | Hit::Empty => {
                if shift {
                    let w = self.engine.screen_to_world(self.cursor.x, self.cursor.y);
                    self.mode = Mode::Boxing { from: w, add: true };
                } else {
                    self.mode = Mode::Pan { moved: false, at };
                }
            }
        }
    }

    pub fn press_right(&mut self, shift: bool) {
        self.commit_text();
        self.right_press = true;
        if self.frozen {
            return;
        }
        let w = self.engine.screen_to_world(self.cursor.x, self.cursor.y);
        self.mode = Mode::Boxing { from: w, add: shift };
    }

    pub fn panning(&self) -> bool {
        matches!(self.mode, Mode::Pan { .. })
    }

    /// Pointer moved while a button is down. Returns true if it needs a redraw.
    pub fn drag_to(&mut self, prev: Pos) -> bool {
        if self.text_drag {
            if let Some(k) = self.slot_at_pointer(false) {
                self.caret = k;
            }
            return true;
        }
        let cur = self.cursor;
        match &mut self.mode {
            Mode::Pan { moved, at } => {
                if (at.x - cur.x).hypot(at.y - cur.y) > 3.0 {
                    *moved = true;
                }
                self.engine.pan(cur.x - prev.x, cur.y - prev.y);
                true
            }
            Mode::Press { at, node, double, .. } => {
                if (at.x - cur.x).hypot(at.y - cur.y) < 4.0 {
                    return false;
                }
                let (at, has_node, double) = (*at, node.is_some(), *double);
                if has_node {
                    self.start_move(at);
                } else if double {
                    let w = self.engine.screen_to_world(at.x, at.y);
                    self.mode = Mode::Boxing { from: w, add: false };
                } else {
                    self.mode = Mode::Idle;
                }
                true
            }
            // Pointers report faster than frames: the work waits for the frame.
            Mode::Move { .. } | Mode::Resize { .. } => {
                self.drag_pending = true;
                true
            }
            Mode::Boxing { .. } | Mode::Connect { .. } | Mode::EdgeEnd { .. } => true,
            _ => false,
        }
    }

    fn start_move(&mut self, at: Pos) {
        // Move the selected concepts whose ancestors are not selected too.
        let sel: HashSet<String> = self.sel.iter().cloned().collect();
        let roots: Vec<String> = self.sel.iter().filter(|id| !self.board.ancestors(id).iter().any(|a| sel.contains(a))).cloned().collect();
        if roots.is_empty() {
            self.mode = Mode::Idle;
            return;
        }
        self.begin();
        let mut nodes = HashSet::new();
        for r in &roots {
            nodes.extend(self.board.subtree(r));
        }
        // Only connections of what moves follow it live (routing them all
        // every frame is too slow on real boards); the rest re-route on drop.
        let edges: HashSet<String> = self
            .board
            .edges
            .iter()
            .filter(|e| route::shown(&self.board, e, self.show_nested_links) && (nodes.contains(&e.from) || nodes.contains(&e.to)))
            .map(|e| e.id.clone())
            .collect();
        let start = self.engine.screen_to_world(at.x, at.y);
        self.mode = Mode::Move { start, roots, base: self.board.clone(), target: None, guides: Vec::new() };
        self.hide(nodes, edges);
        self.drag_move();
    }

    fn drag_move(&mut self) {
        let w = self.engine.screen_to_world(self.cursor.x, self.cursor.y);
        let px = self.px();
        let cursor = self.world();
        let Mode::Move { start, roots, base, target, guides } = &mut self.mode else { return };
        let (mut dx, mut dy) = ((w.0 - start.0) as f32, (w.1 - start.1) as f32);
        let mut board = base.clone();
        let moving: HashSet<String> = roots.iter().flat_map(|r| base.subtree(r)).collect();
        // Snap to siblings' edges and centres.
        let mb = base.bounds_of(roots.iter()).unwrap();
        let moved = [mb[0] + dx, mb[1] + dy, mb[2] + dx, mb[3] + dy];
        let center = ((moved[0] + moved[2]) * 0.5, (moved[1] + moved[3]) * 0.5);
        let probe = if roots.len() == 1 { center } else { cursor };
        let tgt = base.deepest_at(probe.0, probe.1, &moving);
        let siblings: Vec<Bbox> = base.nodes.iter().filter(|n| !moving.contains(&n.id) && base.parent(&n.id).map(String::from) == tgt).map(|n| n.bbox()).collect();
        let snap = 6.0 * px;
        guides.clear();
        let xs = [moved[0], (moved[0] + moved[2]) * 0.5, moved[2]];
        let ys = [moved[1], (moved[1] + moved[3]) * 0.5, moved[3]];
        let mut best_x: Option<(f32, f32, Bbox)> = None;
        let mut best_y: Option<(f32, f32, Bbox)> = None;
        for s in &siblings {
            for sx in [s[0], (s[0] + s[2]) * 0.5, s[2]] {
                for &x in &xs {
                    let d = sx - x;
                    if d.abs() < snap && best_x.map_or(true, |b| d.abs() < b.0.abs()) {
                        best_x = Some((d, sx, *s));
                    }
                }
            }
            for sy in [s[1], (s[1] + s[3]) * 0.5, s[3]] {
                for &y in &ys {
                    let d = sy - y;
                    if d.abs() < snap && best_y.map_or(true, |b| d.abs() < b.0.abs()) {
                        best_y = Some((d, sy, *s));
                    }
                }
            }
        }
        if let Some((d, _, _)) = best_x {
            dx += d;
        }
        if let Some((d, _, _)) = best_y {
            dy += d;
        }
        let fin = [mb[0] + dx, mb[1] + dy, mb[2] + dx, mb[3] + dy];
        if let Some((_, sx, s)) = best_x {
            guides.push(((sx, s[1].min(fin[1])), (sx, s[3].max(fin[3]))));
        }
        if let Some((_, sy, s)) = best_y {
            guides.push(((s[0].min(fin[0]), sy), (s[2].max(fin[2]), sy)));
        }
        for r in roots.iter() {
            board.move_tree(r, dx, dy);
        }
        *target = tgt;
        self.board = board;
    }

    fn drag_resize(&mut self) {
        let w = self.engine.screen_to_world(self.cursor.x, self.cursor.y);
        let shift = self.shift;
        let roots = self.sel_roots();
        let Mode::Resize { id, handle, base, start } = &self.mode else { return };
        let (id, handle) = (id.clone(), *handle);
        let (dx, dy) = ((w.0 - start.0) as f32, (w.1 - start.1) as f32);
        let mut board = base.clone();
        let b0 = if id.is_empty() { base.bounds_of(roots.iter()) } else { base.get(&id).map(|n| n.bbox()) };
        let Some(b0) = b0 else { return };
        // No floor tied to the text: the look shrinks with the box.
        let (ow, oh) = (b0[2] - b0[0], b0[3] - b0[1]);
        let (minw, minh) = (ow * 0.02, oh * 0.02);
        let mut nb = b0;
        if handle & L != 0 {
            nb[0] = (b0[0] + dx).min(b0[2] - minw);
        }
        if handle & R != 0 {
            nb[2] = (b0[2] + dx).max(b0[0] + minw);
        }
        if handle & T != 0 {
            nb[1] = (b0[1] + dy).min(b0[3] - minh);
        }
        if handle & B != 0 {
            nb[3] = (b0[3] + dy).max(b0[1] + minh);
        }
        let (nw, nh) = (nb[2] - nb[0], nb[3] - nb[1]);
        // Anchor: the opposite side stays put.
        let ax = if handle & L != 0 { b0[2] } else { b0[0] };
        let ay = if handle & T != 0 { b0[3] } else { b0[1] };
        let picture = match (handle & (L | R) != 0, handle & (T | B) != 0) {
            (true, true) => (nw / ow).max(nh / oh),
            (true, false) => nw / ow,
            _ => nh / oh,
        }
        .max(0.02);
        let changed: Vec<String> = if id.is_empty() {
            // A group scales like a picture: concepts, text, gaps, contents.
            for r in &roots {
                board.scale_tree(r, picture, ax, ay);
            }
            roots
        } else if shift {
            // Pure scaling of the whole concept with everything inside.
            board.scale_tree(&id, picture, ax, ay);
            vec![id]
        } else {
            // The concept's own look (text, outline, corners) follows its
            // smaller side, so it never looks stretched or squashed. What is
            // inside lives on its own: growing adds room, shrinking first
            // takes the empty room, then scales the content.
            let n = base.node(&id).clone();
            {
                let node = board.get_mut(&id).unwrap();
                node.x = nb[0];
                node.y = nb[1];
                node.w = nw;
                node.h = nh;
                node.font = n.font * (nw.min(nh) / ow.min(oh)).max(0.02);
            }
            // The text must stay inside a leaf: narrowing it only sideways
            // makes it taller, otherwise the text gets smaller.
            if !base.has_children(&id) && n.shape != NShape::Text && !n.text.is_empty() {
                let need = |f: f32| self.lines(&n.text, f, (nw - f).max(f)) as f32 * LINE_H * f + f;
                let node = board.get_mut(&id).unwrap();
                if need(node.font) > nh {
                    if handle & (T | B) == 0 {
                        node.h = need(node.font);
                    } else {
                        let (mut lo, mut hi) = (node.font * 0.05, node.font);
                        for _ in 0..12 {
                            let mid = (lo + hi) * 0.5;
                            if need(mid) > nh {
                                hi = mid;
                            } else {
                                lo = mid;
                            }
                        }
                        node.font = lo;
                    }
                }
            }
            if base.has_children(&id) {
                let nn = board.node(&id).clone();
                let lines = view::title_lines(&nn, &|t: &str, s: f32, w: f32| self.engine.wrapped_lines(t, s, w));
                let kids = base.children(&id).to_vec();
                let cb = base.bounds_of(kids.iter()).unwrap();
                let (gap, title) = (nn.gap(), board.title_h(&nn, lines));
                let (cw, ch) = (cb[2] - cb[0], cb[3] - cb[1]);
                let k = ((nw - 2.0 * gap) / cw).min((nh - title - gap) / ch).min(1.0).max(0.001);
                let cax = if handle & L != 0 { cb[2] } else { cb[0] };
                let cay = if handle & T != 0 { cb[3] } else { cb[1] };
                if k < 1.0 {
                    board.scale_children(&id, k, cax, cay);
                }
                // Keep the content inside the (possibly smaller) area.
                let kb = board.bounds_of(kids.iter()).unwrap();
                let area = [nb[0] + gap, nb[1] + title, nb[2] - gap, nb[3] - gap];
                let sx = if kb[0] < area[0] { area[0] - kb[0] } else if kb[2] > area[2] { area[2] - kb[2] } else { 0.0 };
                let sy = if kb[1] < area[1] { area[1] - kb[1] } else if kb[3] > area[3] { area[3] - kb[3] } else { 0.0 };
                for c in &kids {
                    board.move_tree(c, sx, sy);
                }
            }
            vec![id]
        };
        let eng = &self.engine;
        let one = move |n: &Node| view::title_lines(n, &|t: &str, s: f32, w: f32| eng.wrapped_lines(t, s, w));
        for c in &changed {
            board.fit_ancestors(c, &one);
        }
        let Mode::Resize { base, .. } = &self.mode else { return };
        // What changed against the start of the drag: (id, old box, new box).
        let mut moved: Vec<(String, Bbox, Bbox)> = Vec::new();
        for n in &board.nodes {
            if let Some(o) = base.get(&n.id) {
                if o.bbox() != n.bbox() || o.font != n.font {
                    moved.push((n.id.clone(), o.bbox(), n.bbox()));
                }
            }
        }
        self.board = board;
        self.rebuild_live(&moved);
    }

    /// Rebuilds the scene during a resize. A connection is routed again only
    /// if one of its ends changed or a changed concept that can stand in its
    /// way (not an ancestor of its ends) comes near its route; the release
    /// does a full rebuild.
    fn rebuild_live(&mut self, moved: &[(String, Bbox, Bbox)]) {
        let touch = |a: &Bbox, b: &Bbox, m: f32| a[0] <= b[2] + m && a[2] >= b[0] - m && a[1] <= b[3] + m && a[3] >= b[1] - m;
        let ends: HashSet<&str> = moved.iter().map(|m| m.0.as_str()).collect();
        let board = &self.board;
        let dirty: HashSet<String> = board
            .edges
            .iter()
            .filter(|e| e.kind == Kind::Link)
            .filter(|e| {
                if ends.contains(e.from.as_str()) || ends.contains(e.to.as_str()) {
                    return true;
                }
                let Some(p) = self.raw_routes.get(&e.id) else { return true };
                let pb = route::path_box(p);
                let margin = board.get(&e.from).map_or(0.0, |n| n.font) * 2.0;
                let mut up: Vec<String> = board.ancestors(&e.from);
                up.extend(board.ancestors(&e.to));
                moved.iter().any(|(id, o, n)| !up.contains(id) && (touch(&pb, o, margin) || touch(&pb, n, margin)))
            })
            .map(|e| e.id.clone())
            .collect();
        self.raw_routes = route::raw_routes(&self.board, self.show_nested_links, Some(&dirty), &self.raw_routes);
        self.routes = route::finish(&self.board, &self.raw_routes);
        let m = |t: &str, s: f32, w: f32| self.engine.wrapped_lines(t, s, w);
        let skip = view::Skip { nodes: &self.hidden_nodes, edges: &self.hidden_edges };
        let scene = view::build(&self.board, &self.routes, &m, &skip, view::bg_color());
        self.engine.set_scene(scene);
    }

    pub fn release(&mut self) {
        self.text_drag = false;
        self.flush_drag();
        match std::mem::replace(&mut self.mode, Mode::Idle) {
            Mode::Pan { moved: false, .. } => {
                self.sel.clear();
                self.sel_edge = None;
            }
            Mode::Press { double: true, node: None, .. } => {
                let p = self.world();
                self.create_at(p, None);
            }
            Mode::Press { edge_mid: Some(e), .. } => self.edge_menu(&e),
            Mode::Press { node: Some(id), .. } => {
                if !self.shift && self.sel.len() > 1 && self.sel.contains(&id) {
                    self.sel = vec![id];
                }
            }
            Mode::Move { roots, target, .. } => self.drop(roots, target),
            Mode::Resize { .. } => self.commit(),
            // A right click without a drag opens the context menu.
            Mode::Boxing { from, .. } if self.right_press && {
                let w = self.engine.screen_to_world(self.cursor.x, self.cursor.y);
                (w.0 - from.0).hypot(w.1 - from.1) * self.engine.cam.s < 4.0
            } => self.context_menu(),
            Mode::Boxing { from, add } => {
                let (x, y) = self.world();
                let b = [(from.0 as f32).min(x), (from.1 as f32).min(y), (from.0 as f32).max(x), (from.1 as f32).max(y)];
                if !add {
                    self.sel.clear();
                }
                self.sel_edge = None;
                let picked: Vec<String> = self.board.nodes.iter().filter(|n| contains(&b, &n.bbox())).map(|n| n.id.clone()).collect();
                for id in picked {
                    if !self.sel.contains(&id) {
                        self.sel.push(id);
                    }
                }
            }
            Mode::Connect { from } => self.finish_connect(from),
            Mode::EdgeEnd { edge, moving_from } => {
                let fixed = self.board.edge(&edge).map(|e| if moving_from { e.to.clone() } else { e.from.clone() });
                if let Some(fixed) = fixed {
                    if let Some(t) = self.connect_target(&fixed) {
                        if self.board.edge_between(&t, &fixed).is_none() {
                            self.begin();
                            let e = self.board.edge_mut(&edge).unwrap();
                            if moving_from {
                                e.from = t;
                            } else {
                                e.to = t;
                            }
                            self.board.reindex();
                            self.commit();
                        }
                    }
                }
            }
            m @ Mode::Text { .. } => self.mode = m,
            _ => {}
        }
        self.rebuild();
    }

    fn drop(&mut self, roots: Vec<String>, target: Option<String>) {
        for r in &roots {
            self.board.set_parent(r, target.as_deref());
        }
        let eng = &self.engine;
        let one = move |n: &Node| view::title_lines(n, &|t: &str, s: f32, w: f32| eng.wrapped_lines(t, s, w));
        let mut board = std::mem::take(&mut self.board);
        for r in &roots {
            board.fit_ancestors(r, &one);
            board.resolve_overlap(r);
            board.fit_ancestors(r, &one);
        }
        self.board = board;
        self.commit();
    }

    fn finish_connect(&mut self, from: String) {
        let p = self.world();
        if let Some(t) = self.connect_target(&from) {
            if self.board.edge_between(&from, &t).is_none() {
                self.begin();
                let e = self.board.add_edge(&from, &t, Kind::Link);
                self.commit();
                self.sel.clear();
                self.sel_edge = Some(e);
            }
            return;
        }
        let Some(a) = self.board.get(&from).cloned() else { return };
        if inside(&a.bbox(), p, self.tol()) {
            return;
        }
        // Released on empty space: a new concept there, already connected.
        let parent = self.board.deepest_at(p.0, p.1, &HashSet::new()).filter(|t| !self.board.is_ancestor(&from, t) && *t != from);
        self.begin();
        let id = self.place_new(p, parent.as_deref());
        if !self.board.is_ancestor(&id, &from) {
            self.board.add_edge(&from, &id, Kind::Link);
        }
        self.sel = vec![id.clone()];
        self.text_box = self.board.get(&id).map(|n| (n.w, n.h));
        self.mode = Mode::Text { target: TextTarget::Node(id), text: String::new(), created: true };
        (self.caret, self.anchor) = (0, 0);
        self.caret_epoch = Instant::now();
    }

    /// Adds a concept centred at `p`, about 160x80 px on screen, nested in
    /// `parent` (sized to fit it). Does not commit.
    fn place_new(&mut self, p: Pt, parent: Option<&str>) -> String {
        let s = self.sf();
        let px = self.px();
        let (mut w, mut h, mut font) = (160.0 * s * px, 80.0 * s * px, 18.0 * s * px);
        // Neighbours at the same level set the size, so a new concept matches
        // them whatever the zoom: the nearest leaf sibling of a similar scale.
        let screen = font;
        let near = self
            .board
            .nodes
            .iter()
            .filter(|n| self.board.parent(&n.id) == parent && !self.board.has_children(&n.id) && n.shape != NShape::Text)
            .filter(|n| n.font > screen * 0.125 && n.font < screen * 8.0)
            .min_by(|a, b| {
                let d = |n: &Node| (n.center().0 - p.0).hypot(n.center().1 - p.1);
                d(a).partial_cmp(&d(b)).unwrap()
            })
            .cloned();
        if let Some(n) = near {
            font = n.font;
            w = font * 8.0;
            h = font * 4.0;
        } else if let Some(pn) = parent.and_then(|q| self.board.get(q)).cloned() {
            let lines = self.title_lines(&pn);
            let area = self.board.content_area(&pn, lines);
            // Smaller than the parent, but readable on screen: the parent
            // grows around it instead.
            let k = ((area[2] - area[0]) * 0.45 / w).min(1.0).max(0.6).min(pn.font / font);
            w *= k;
            h *= k;
            font *= k;
        }
        let _ = px;
        let id = self.board.new_id();
        self.board.add_node(Node { id: id.clone(), x: p.0 - w * 0.5, y: p.1 - h * 0.5, w, h, text: String::new(), font, shape: NShape::Box, color: None, fill: None, extra: Map::new() });
        if let Some(q) = parent {
            self.board.set_parent(&id, Some(q));
        }
        let eng = &self.engine;
        let one = move |n: &Node| view::title_lines(n, &|t: &str, s: f32, w: f32| eng.wrapped_lines(t, s, w));
        let mut board = std::mem::take(&mut self.board);
        board.fit_ancestors(&id, &one);
        board.resolve_overlap(&id);
        board.fit_ancestors(&id, &one);
        self.board = board;
        id
    }

    /// Adds another board next to what is here (to the right, tops
    /// aligned), as one undo step, selects it and shows it.
    pub fn merge_in(&mut self, other: Board) {
        if other.nodes.is_empty() {
            return;
        }
        self.commit_text();
        let ob = other.bounds();
        let (dx, dy) = if self.board.nodes.is_empty() {
            (0.0, 0.0)
        } else {
            let b = self.board.bounds();
            let gap = ((b[2] - b[0]).max(ob[2] - ob[0])) * 0.1;
            (b[2] + gap - ob[0], b[1] - ob[1])
        };
        self.begin();
        let roots = self.board.merge(other, dx, dy);
        self.commit();
        self.sel = roots;
        self.sel_edge = None;
        self.rebuild();
        if let Some(b) = self.board.bounds_of(self.sel.iter()) {
            self.engine.fit_box(b, 0.85);
        }
    }

    /// Double-click on empty canvas (or a parent's empty inside).
    fn create_at(&mut self, p: Pt, parent: Option<String>) {
        let parent = parent.or_else(|| self.board.deepest_at(p.0, p.1, &HashSet::new()));
        self.begin();
        let id = self.place_new(p, parent.as_deref());
        self.sel = vec![id.clone()];
        self.sel_edge = None;
        self.text_box = self.board.get(&id).map(|n| (n.w, n.h));
        self.mode = Mode::Text { target: TextTarget::Node(id), text: String::new(), created: true };
        (self.caret, self.anchor) = (0, 0);
        self.caret_epoch = Instant::now();
    }

    // ------------------------------------------------------------ text

    fn start_text(&mut self, target: TextTarget, created: bool) {
        let text = match &target {
            TextTarget::Node(id) => {
                self.sel = vec![id.clone()];
                self.sel_edge = None;
                self.board.get(id).map(|n| n.text.clone())
            }
            TextTarget::Edge(e) => {
                self.sel.clear();
                self.sel_edge = Some(e.clone());
                self.board.edge(e).map(|e| e.label.clone())
            }
        };
        let Some(text) = text else { return };
        self.begin();
        self.text_box = match &target {
            TextTarget::Node(id) => self.board.get(id).map(|n| (n.w, n.h)),
            TextTarget::Edge(_) => None,
        };
        self.caret_epoch = Instant::now();
        self.caret = text.chars().count();
        self.anchor = self.caret;
        self.mode = Mode::Text { target, text, created };
    }

    /// The caret slot nearest the pointer in the text being edited. With
    /// `strict`, only while the pointer is on that concept or label.
    fn slot_at_pointer(&self, strict: bool) -> Option<usize> {
        let Mode::Text { target, text, .. } = &self.mode else { return None };
        let board = self.shown();
        let l = self.text_label(board, target, text)?;
        let p = self.world();
        let slots = self.engine.caret_slots(&l);
        let lh = LINE_H * l.size;
        let last = slots.iter().map(|s| s.2).max().unwrap_or(0);
        if strict {
            let on = match target {
                TextTarget::Node(id) => board.get(id).is_some_and(|n| inside(&n.bbox(), p, self.tol())),
                TextTarget::Edge(_) => {
                    let (x0, x1) = slots.iter().fold((f32::MAX, f32::MIN), |(a, b), s| (a.min(s.0), b.max(s.0)));
                    inside(&[x0, l.y, x1, l.y + (last + 1) as f32 * lh], p, self.tol() + l.size)
                }
            };
            if !on {
                return None;
            }
        }
        let line = (((p.1 - l.y) / lh).floor().max(0.0) as u16).min(last);
        (0..slots.len()).filter(|&k| slots[k].2 == line).min_by(|&a, &b| (slots[a].0 - p.0).abs().total_cmp(&(slots[b].0 - p.0).abs()))
    }

    /// Selects the word (or run of spaces or punctuation) around slot `k`.
    fn select_word(&mut self, k: usize) {
        let Mode::Text { text, .. } = &self.mode else { return };
        let cs: Vec<char> = text.chars().collect();
        let kind = |i: usize| cs.get(i).map(|&c| if c == '\n' { 2 } else if c.is_alphanumeric() || c == '_' { 1 } else { 0 });
        let (mut a, mut b) = (k.min(cs.len()), k.min(cs.len()));
        let pick = if kind(b) == Some(1) || a == 0 { kind(b) } else { kind(a - 1) };
        if pick.is_none() || pick == Some(2) {
            return;
        }
        while a > 0 && kind(a - 1) == pick {
            a -= 1;
        }
        while b < cs.len() && kind(b) == pick {
            b += 1;
        }
        self.anchor = a;
        self.caret = b;
    }

    /// The selected part of the text being edited.
    pub fn selected_text(&self) -> Option<String> {
        let Mode::Text { text, .. } = &self.mode else { return None };
        let (a, b) = (self.anchor.min(self.caret), self.anchor.max(self.caret));
        (a < b).then(|| text.chars().skip(a).take(b - a).collect())
    }

    pub fn select_all(&mut self) {
        let Mode::Text { text, .. } = &self.mode else { return };
        self.anchor = 0;
        self.caret = text.chars().count();
        self.caret_epoch = Instant::now();
    }

    /// Caret movement and deletion keys while editing text.
    pub fn text_key(&mut self, key: TextKey, shift: bool, ctrl: bool) {
        let Mode::Text { target, text, .. } = &self.mode else { return };
        let cs: Vec<char> = text.chars().collect();
        let n = cs.len();
        let (a, b) = (self.anchor.min(self.caret), self.anchor.max(self.caret));
        let word_left = |mut k: usize| {
            while k > 0 && !cs[k - 1].is_alphanumeric() {
                k -= 1;
            }
            while k > 0 && cs[k - 1].is_alphanumeric() {
                k -= 1;
            }
            k
        };
        let word_right = |mut k: usize| {
            while k < n && !cs[k].is_alphanumeric() {
                k += 1;
            }
            while k < n && cs[k].is_alphanumeric() {
                k += 1;
            }
            k
        };
        let slots = self.text_label(self.shown(), target, text).map(|l| self.engine.caret_slots(&l)).unwrap_or_default();
        let c = self.caret.min(n);
        let line_of = |k: usize| slots.get(k).map_or(0, |s| s.2);
        let to_line = |line: i32| -> usize {
            let x = slots.get(c).map_or(0.0, |s| s.0);
            (0..slots.len()).filter(|&k| slots[k].2 as i32 == line).min_by(|&p, &q| (slots[p].0 - x).abs().total_cmp(&(slots[q].0 - x).abs())).unwrap_or(if line < 0 { 0 } else { n })
        };
        let moved = match key {
            TextKey::Left if !shift && !ctrl && a < b => Some(a),
            TextKey::Right if !shift && !ctrl && a < b => Some(b),
            TextKey::Left if ctrl => Some(word_left(c)),
            TextKey::Right if ctrl => Some(word_right(c)),
            TextKey::Left => Some(c.saturating_sub(1)),
            TextKey::Right => Some((c + 1).min(n)),
            TextKey::Up => Some(to_line(line_of(c) as i32 - 1)),
            TextKey::Down => Some(to_line(line_of(c) as i32 + 1)),
            TextKey::Home if ctrl => Some(0),
            TextKey::End if ctrl => Some(n),
            TextKey::Home => Some((0..=c).rev().take_while(|&i| line_of(i) == line_of(c)).last().unwrap_or(0)),
            TextKey::End => Some((c..slots.len()).take_while(|&i| line_of(i) == line_of(c)).last().unwrap_or(n)),
            TextKey::Backspace | TextKey::Delete => None,
        };
        if let Some(k) = moved {
            self.caret = k.min(n);
            if !shift {
                self.anchor = self.caret;
            }
        } else {
            let (from, to) = if a < b {
                (a, b)
            } else if key == TextKey::Backspace {
                (if ctrl { word_left(c) } else { c.saturating_sub(1) }, c)
            } else {
                (c, if ctrl { word_right(c) } else { (c + 1).min(n) })
            };
            if from < to {
                self.replace_range(from, to, "");
            }
        }
        self.caret_epoch = Instant::now();
    }

    /// Replaces characters `from..to` of the text being edited with `s` and
    /// puts the caret after it.
    fn replace_range(&mut self, from: usize, to: usize, s: &str) {
        let Mode::Text { target, text, .. } = &mut self.mode else { return };
        let byte = |t: &str, k: usize| t.char_indices().nth(k).map_or(t.len(), |(i, _)| i);
        let (bf, bt) = (byte(text, from), byte(text, to));
        text.replace_range(bf..bt, s);
        self.caret = from + s.chars().count();
        self.anchor = self.caret;
        let (target, text) = (target.clone(), text.clone());
        self.apply_text(&target, &text);
        self.rebuild();
    }

    /// Enter / F2: edit the text of the selected concept or connection.
    pub fn edit_selected(&mut self) {
        if let [one] = &self.sel[..] {
            let id = one.clone();
            self.start_text(TextTarget::Node(id), false);
        } else if let Some(e) = self.sel_edge.clone() {
            self.start_text(TextTarget::Edge(e), false);
        }
    }

    pub fn editing_text(&self) -> bool {
        matches!(self.mode, Mode::Text { .. })
    }

    /// Replaces the text being edited (a native text field owns the caret).
    pub fn set_text(&mut self, s: &str) {
        let Mode::Text { target, text, .. } = &mut self.mode else { return };
        *text = s.to_string();
        self.caret = s.chars().count();
        self.anchor = self.caret;
        let (target, text) = (target.clone(), text.clone());
        self.apply_text(&target, &text);
        self.rebuild();
    }

    /// Typed or pasted text: replaces the selection at the caret.
    /// Backspace arrives as U+0008.
    pub fn type_text(&mut self, s: &str) {
        if !self.editing_text() {
            return;
        }
        let mut buf = String::new();
        for c in s.chars() {
            match c {
                '\u{8}' => {
                    if !buf.is_empty() {
                        self.insert(&std::mem::take(&mut buf));
                    }
                    self.text_key(TextKey::Backspace, false, false);
                }
                '\r' => {}
                '\n' => buf.push('\n'),
                '\t' => buf.push(' '),
                c if !c.is_control() => buf.push(c),
                _ => {}
            }
        }
        if !buf.is_empty() {
            self.insert(&buf);
        }
        self.caret_epoch = Instant::now();
    }

    fn insert(&mut self, s: &str) {
        let (a, b) = (self.anchor.min(self.caret), self.anchor.max(self.caret));
        self.replace_range(a, b, s);
    }

    fn apply_text(&mut self, target: &TextTarget, text: &str) {
        match target {
            TextTarget::Edge(e) => {
                if let Some(e) = self.board.edge_mut(e) {
                    e.label = text.to_string();
                }
            }
            TextTarget::Node(id) => {
                let Some(n) = self.board.get(id).cloned() else { return };
                let pad = n.pad();
                let (tw, _) = self.engine.measure(text, n.font);
                let mut nn = n.clone();
                nn.text = text.to_string();
                if n.shape == NShape::Text {
                    let (w, h) = self.engine.measure(text, n.font);
                    nn.w = w.max(n.font);
                    nn.h = h.max(n.font * LINE_H);
                } else if !self.board.has_children(id) {
                    let (bw, bh) = self.text_box.unwrap_or((n.w, n.h));
                    let want_w = (tw.min(n.font * 14.0) + 2.0 * pad).max(bw);
                    let lines = self.lines(text, n.font, want_w - 2.0 * pad) as f32;
                    nn.w = want_w;
                    nn.h = bh.max(lines * LINE_H * n.font + 2.0 * pad);
                }
                *self.board.get_mut(id).unwrap() = nn.clone();
                // A longer title pushes the content area down: keep children clear of it.
                if self.board.has_children(id) {
                    let lines = self.title_lines(&nn);
                    let area_top = nn.y + self.board.title_h(&nn, lines);
                    let kids = self.board.children(id).to_vec();
                    if let Some(kb) = self.board.bounds_of(kids.iter()) {
                        if kb[1] < area_top {
                            let d = area_top - kb[1];
                            for c in &kids {
                                self.board.move_tree(c, 0.0, d);
                            }
                            self.board.get_mut(id).unwrap().h += d;
                        }
                    }
                }
                let eng = &self.engine;
        let one = move |n: &Node| view::title_lines(n, &|t: &str, s: f32, w: f32| eng.wrapped_lines(t, s, w));
                let mut board = std::mem::take(&mut self.board);
                board.fit_ancestors(id, &one);
                self.board = board;
            }
        }
    }

    /// Ends text editing; a new concept left empty is removed again.
    pub fn commit_text(&mut self) {
        let Mode::Text { target, text, created } = std::mem::replace(&mut self.mode, Mode::Idle) else { return };
        self.text_box = None;
        if let TextTarget::Node(id) = &target {
            if created && text.trim().is_empty() {
                self.board.delete(&[id.clone()], false);
                self.sel.clear();
            }
        }
        self.commit();
        self.rebuild();
    }

    // ------------------------------------------------------------ commands

    pub fn delete_selection(&mut self, deep: bool) {
        if let Some(e) = self.sel_edge.take() {
            self.begin();
            self.board.delete_edge(&e);
            self.commit();
            self.rebuild();
            return;
        }
        if self.sel.is_empty() {
            return;
        }
        self.begin();
        let sel = std::mem::take(&mut self.sel);
        self.board.delete(&sel, deep);
        self.commit();
        self.rebuild();
    }

    /// Right click without a drag: a menu for what is under the pointer.
    fn context_menu(&mut self) {
        let (x, y) = (self.cursor.x, self.cursor.y);
        self.menu_at = self.world();
        match self.hit() {
            Hit::Node(id) | Hit::Inside(id) | Hit::Handle(id, _) if !id.is_empty() => {
                if !self.sel.contains(&id) {
                    self.sel = vec![id];
                }
                self.sel_edge = None;
                self.request = Some(Request::NodeMenu { x, y });
            }
            Hit::Handle(..) => self.request = Some(Request::NodeMenu { x, y }),
            Hit::Edge(e) | Hit::EdgeMid(e) | Hit::EdgeEnd(e, _) => {
                self.sel.clear();
                self.sel_edge = Some(e.clone());
                self.edge_menu(&e);
            }
            _ => self.request = Some(Request::CanvasMenu { x, y }),
        }
    }

    /// The answer to a `NodeMenu` or `CanvasMenu` request (0: dismissed).
    pub fn apply_node_choice(&mut self, choice: i32) {
        let sel = self.sel.clone();
        match choice {
            c if (NODE_COLOR..NODE_COLOR + PALETTE.len() as i32).contains(&c) => {
                let fill = PALETTE[(c - NODE_COLOR) as usize].1.map(String::from);
                self.begin();
                for id in &sel {
                    if let Some(n) = self.board.get_mut(id) {
                        n.fill = fill.clone();
                    }
                }
                self.commit();
            }
            c if (NODE_SHAPE..NODE_SHAPE + 4).contains(&c) => {
                let shape = [NShape::Box, NShape::Ellipse, NShape::Diamond, NShape::Text][(c - NODE_SHAPE) as usize];
                self.begin();
                self.text_box = None;
                for id in &sel {
                    let Some(n) = self.board.get(id).cloned() else { continue };
                    if n.shape == shape || (shape == NShape::Text && self.board.has_children(id)) {
                        continue;
                    }
                    let was_text = n.shape == NShape::Text;
                    let node = self.board.get_mut(id).unwrap();
                    node.shape = shape;
                    if was_text {
                        // Room around the text for the new outline.
                        node.x -= n.pad();
                        node.y -= n.pad();
                        node.w += 2.0 * n.pad();
                        node.h += 2.0 * n.pad();
                    } else if shape == NShape::Text {
                        let (cx, cy) = (n.x + n.w * 0.5, n.y + n.h * 0.5);
                        let (w, h) = self.engine.measure(&n.text, n.font);
                        let node = self.board.get_mut(id).unwrap();
                        (node.w, node.h) = (w.max(n.font), h.max(n.font * LINE_H));
                        (node.x, node.y) = (cx - node.w * 0.5, cy - node.h * 0.5);
                    }
                    let text = n.text.clone();
                    self.apply_text(&TextTarget::Node(id.clone()), &text);
                }
                self.commit();
            }
            NODE_EDIT => self.edit_selected(),
            NODE_DUPLICATE => self.duplicate(),
            NODE_WRAP => self.wrap_selection(),
            NODE_DELETE => self.delete_selection(false),
            NODE_DELETE_DEEP => self.delete_selection(true),
            CANVAS_NEW => {
                let p = self.menu_at;
                self.create_at(p, None);
            }
            _ => {}
        }
        self.rebuild();
    }

    /// Tab: a new concept to the right of the selected one, connected to
    /// it, with its text open (while typing: finishes the text first).
    pub fn tab_new(&mut self) {
        self.commit_text();
        let [one] = &self.sel[..] else { return };
        let Some(n) = self.board.get(one).cloned() else { return };
        let parent = self.board.parent(&n.id).map(String::from);
        let p = (n.x + n.w * 1.9, n.y + n.h * 0.5);
        self.begin();
        let id = self.place_new(p, parent.as_deref());
        // The same look as the concept it continues.
        if n.shape != NShape::Text {
            let m = self.board.get_mut(&id).unwrap();
            let (cx, cy) = (m.x + m.w * 0.5, m.y + m.h * 0.5);
            (m.w, m.h, m.font, m.shape, m.fill) = (n.w, n.h, n.font, n.shape, n.fill.clone());
            (m.x, m.y) = (cx - n.w * 0.5, cy - n.h * 0.5);
            let eng = &self.engine;
            let one = move |n: &Node| view::title_lines(n, &|t: &str, s: f32, w: f32| eng.wrapped_lines(t, s, w));
            let mut board = std::mem::take(&mut self.board);
            board.resolve_overlap(&id);
            board.fit_ancestors(&id, &one);
            self.board = board;
        }
        self.board.add_edge(&n.id, &id, Kind::Link);
        self.sel = vec![id.clone()];
        self.sel_edge = None;
        self.text_box = self.board.get(&id).map(|n| (n.w, n.h));
        self.mode = Mode::Text { target: TextTarget::Node(id.clone()), text: String::new(), created: true };
        (self.caret, self.anchor) = (0, 0);
        self.caret_epoch = Instant::now();
        self.rebuild();
        self.show(&id);
    }

    /// Pans just enough to bring concept `id` into view.
    fn show(&mut self, id: &str) {
        let Some(b) = self.board.get(id).map(|n| n.bbox()) else { return };
        let v = self.engine.view_rect();
        let m = 24.0 * self.dp();
        let dx = if b[2] + m > v[2] { v[2] - b[2] - m } else if b[0] - m < v[0] { v[0] - b[0] + m } else { 0.0 };
        let dy = if b[3] + m > v[3] { v[3] - b[3] - m } else if b[1] - m < v[1] { v[1] - b[1] + m } else { 0.0 };
        let s = self.engine.cam.s;
        self.engine.pan(dx as f64 * s, dy as f64 * s);
    }

    /// Ctrl+A on the canvas.
    pub fn select_all_nodes(&mut self) {
        self.sel = self.board.nodes.iter().map(|n| n.id.clone()).collect();
        self.sel_edge = None;
    }

    /// Ctrl+C on the canvas: the text of the selected concepts, one per line.
    pub fn selection_text(&self) -> Option<String> {
        if let Some(e) = self.sel_edge.as_ref().and_then(|e| self.board.edge(e)) {
            return (!e.label.is_empty()).then(|| e.label.clone());
        }
        let t: Vec<&str> = self.board.nodes.iter().filter(|n| self.sel.contains(&n.id) && !n.text.is_empty()).map(|n| n.text.as_str()).collect();
        (!t.is_empty()).then(|| t.join("\n"))
    }

    /// Ctrl+C on the canvas: the selected concepts with everything nested
    /// in them and the connections among them, as a board of their own.
    pub fn copy_selection(&self) -> Option<Board> {
        let roots = self.sel_roots();
        if roots.is_empty() {
            return None;
        }
        let ids: HashSet<String> = roots.iter().flat_map(|r| self.board.subtree(r)).collect();
        let mut b = Board::new();
        b.nodes = self.board.nodes.iter().filter(|n| ids.contains(&n.id)).cloned().collect();
        b.edges = self.board.edges.iter().filter(|e| ids.contains(&e.from) && ids.contains(&e.to)).cloned().collect();
        b.reindex();
        Some(b)
    }

    /// The pointer in the world, or the middle of the view when the pointer
    /// is outside the window.
    fn paste_point(&mut self) -> Pt {
        let (w, h) = self.engine.size();
        if self.cursor.x < 0.0 || self.cursor.y < 0.0 || self.cursor.x > w as f64 || self.cursor.y > h as f64 {
            self.cursor = Pos { x: w as f64 / 2.0, y: h as f64 / 2.0 };
        }
        self.world()
    }

    /// Ctrl+V of copied concepts: centred under the pointer, nested in what
    /// is there, fresh ids, one undo step, selected.
    pub fn paste_board(&mut self, other: Board) {
        if other.nodes.is_empty() {
            return;
        }
        self.commit_text();
        let p = self.paste_point();
        let target = self.board.deepest_at(p.0, p.1, &HashSet::new());
        self.place_board(other, p, target);
    }

    /// Ctrl+D: a copy of the selection next to it, at the same level.
    pub fn duplicate(&mut self) {
        let Some(other) = self.copy_selection() else { return };
        let roots = self.sel_roots();
        let parent = self.board.parent(&roots[0]).map(String::from);
        let b = other.bounds();
        let gap = 24.0 * self.dp();
        self.place_board(other, ((b[0] + b[2]) * 0.5 + gap, (b[1] + b[3]) * 0.5 + gap), parent);
    }

    fn place_board(&mut self, other: Board, p: Pt, parent: Option<String>) {
        let ob = other.bounds();
        let (dx, dy) = (p.0 - (ob[0] + ob[2]) * 0.5, p.1 - (ob[1] + ob[3]) * 0.5);
        self.begin();
        let roots = self.board.merge(other, dx, dy);
        for r in &roots {
            self.board.set_parent(r, parent.as_deref());
        }
        let eng = &self.engine;
        let one = move |n: &Node| view::title_lines(n, &|t: &str, s: f32, w: f32| eng.wrapped_lines(t, s, w));
        let mut board = std::mem::take(&mut self.board);
        for r in &roots {
            board.fit_ancestors(r, &one);
            board.resolve_overlap(r);
            board.fit_ancestors(r, &one);
        }
        self.board = board;
        self.commit();
        self.sel = roots;
        self.sel_edge = None;
        self.rebuild();
    }

    /// Ctrl+V on the canvas: a new concept with `text` under the pointer
    /// (the middle of the view when the pointer is outside the window).
    pub fn paste_concept(&mut self, text: &str) {
        let p = self.paste_point();
        self.create_at(p, None);
        self.type_text(text);
        self.commit_text();
    }

    /// Ctrl+G: a new concept around the selection, holding it.
    pub fn wrap_selection(&mut self) {
        let sel: HashSet<String> = self.sel.iter().cloned().collect();
        let roots: Vec<String> = self.sel.iter().filter(|id| !self.board.ancestors(id).iter().any(|a| sel.contains(a))).cloned().collect();
        let Some(cb) = self.board.bounds_of(roots.iter()) else { return };
        let font = roots.iter().map(|r| self.board.node(r).font).fold(0.0, f32::max) * 1.15;
        let parents: HashSet<Option<String>> = roots.iter().map(|r| self.board.parent(r).map(String::from)).collect();
        let common = if parents.len() == 1 { parents.into_iter().next().unwrap() } else { None };
        self.begin();
        let id = self.board.new_id();
        let mut n = Node { id: id.clone(), x: 0.0, y: 0.0, w: 0.0, h: 0.0, text: String::new(), font, shape: NShape::Box, color: None, fill: None, extra: Map::new() };
        let (gap, title) = (n.gap(), font * LINE_H + font);
        n.x = cb[0] - gap;
        n.y = cb[1] - title;
        n.w = cb[2] - cb[0] + 2.0 * gap;
        n.h = cb[3] - cb[1] + title + gap;
        self.board.add_node(n);
        if let Some(c) = &common {
            self.board.set_parent(&id, Some(c));
        }
        for r in &roots {
            self.board.set_parent(r, Some(&id));
        }
        let eng = &self.engine;
        let one = move |n: &Node| view::title_lines(n, &|t: &str, s: f32, w: f32| eng.wrapped_lines(t, s, w));
        let mut board = std::mem::take(&mut self.board);
        board.fit_ancestors(&id, &one);
        self.board = board;
        self.sel = vec![id.clone()];
        self.caret_epoch = Instant::now();
        self.mode = Mode::Text { target: TextTarget::Node(id), text: String::new(), created: false };
        self.rebuild();
    }

    /// Click on a connection's middle handle: direction, label, delete.
    pub fn edge_menu(&mut self, eid: &str) {
        let Some(e) = self.board.edge(eid).cloned() else { return };
        let _ = e;
        self.request = Some(Request::EdgeMenu { edge: eid.to_string(), x: self.cursor.x, y: self.cursor.y });
    }

    pub fn apply_edge_choice(&mut self, eid: &str, choice: i32) {
        match choice {
            1..=4 => {
                self.begin();
                if let Some(e) = self.board.edge_mut(eid) {
                    e.arrow_from = choice == 3 || choice == 4;
                    e.arrow_to = choice == 2 || choice == 4;
                }
                self.commit();
            }
            5 => self.start_text(TextTarget::Edge(eid.to_string()), false),
            6 => {
                self.begin();
                self.board.delete_edge(eid);
                self.sel_edge = None;
                self.commit();
            }
            _ => {}
        }
        self.rebuild();
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

fn invert(v: &[(String, Option<serde_json::Value>, Option<serde_json::Value>)]) -> Vec<(String, Option<serde_json::Value>, Option<serde_json::Value>)> {
    v.iter().map(|(id, b, a)| (id.clone(), a.clone(), b.clone())).collect()
}

fn inside(b: &Bbox, p: Pt, tol: f32) -> bool {
    p.0 >= b[0] - tol && p.0 <= b[2] + tol && p.1 >= b[1] - tol && p.1 <= b[3] + tol
}

fn handle_pos(b: &Bbox, h: Handle, pad: f32) -> Pt {
    let x = if h & L != 0 {
        b[0] - pad
    } else if h & R != 0 {
        b[2] + pad
    } else {
        (b[0] + b[2]) * 0.5
    };
    let y = if h & T != 0 {
        b[1] - pad
    } else if h & B != 0 {
        b[3] + pad
    } else {
        (b[1] + b[3]) * 0.5
    };
    (x, y)
}

/// The "drag to connect" dots: just outside the middle of each side.
fn connect_dots(n: &Node, off: f32) -> [Pt; 4] {
    let (cx, cy) = (n.x + n.w * 0.5, n.y + n.h * 0.5);
    [(n.x + n.w + off, cy), (cx, n.y + n.h + off), (n.x - off, cy), (cx, n.y - off)]
}

/// The label a concept would show with `text`, for placing the caret.
fn caret_label(board: &Board, n: &Node, text: &str, m: view::Measure) -> Option<Label> {
    let mut nn = n.clone();
    nn.text = if text.is_empty() { " ".into() } else { text.to_string() };
    let mut s = Scene::default();
    view::node_into(board, &nn, m, &mut s);
    s.labels.pop().map(|mut l| {
        l.text = text.to_string();
        l
    })
}
