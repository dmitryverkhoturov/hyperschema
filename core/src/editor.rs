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
use crate::trail::{Layout, Trail};
use crate::view::Look;
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
/// Names of PALETTE colours in Russian, same order.
const PALETTE_RU: [&str; 8] = ["Без заливки", "Серый", "Красный", "Оранжевый", "Жёлтый", "Зелёный", "Голубой", "Фиолетовый"];

/// Name of `PALETTE[k]` in the interface language.
pub fn color_name(k: usize) -> &'static str {
    if crate::i18n::russian() { PALETTE_RU[k] } else { PALETTE[k].0 }
}

pub const PALETTE: [(&str, Option<&str>); 8] = [
    ("No fill", None),
    ("Grey", Some("#e9ecef")),
    ("Red", Some("#ffc9c9")),
    ("Orange", Some("#ffd8a8")),
    ("Yellow", Some("#fff3bf")),
    ("Green", Some("#b2f2bb")),
    ("Blue", Some("#a5d8ff")),
    ("Violet", Some("#d0bfff")),
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
pub const NODE_PATH: i32 = 35;
pub const NODE_CLOSE: i32 = 36;
/// How many recent walks are remembered.
const WALKS_KEPT: usize = 20;

#[derive(Clone)]
struct Walk {
    links: Vec<String>,
    kept: Vec<(String, crate::trail::Pose)>,
}

pub const CANVAS_NEW: i32 = 40;

const BLUE: (u8, u8, u8) = (0x4a, 0x7d, 0xff);

fn blue(a: u8) -> u32 {
    rgba(BLUE.0, BLUE.1, BLUE.2, a)
}

/// The colour of the walked path.
fn path_red(a: u8) -> u32 {
    rgba(0xe0, 0x3a, 0x3a, a)
}

/// What a tap on the path bar hits: back (leave the walk) or a link index.
pub const BAR_BACK: i32 = -1;
pub const BAR_LAYOUT: i32 = -3;

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
    /// An end of the selected connection: a click turns its arrowhead on
    /// or off, a drag moves the end to another concept.
    EdgeEnd { edge: String, moving_from: bool, at: Pos, moved: bool },
    Text { target: TextTarget, text: String, created: bool },
    /// Path mode: a concept out of the walk dragged to a new spot (only
    /// where it is drawn; the board does not move).
    PathDrag { id: String, start: (f64, f64), base: (f32, f32) },
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
    /// What slides in path mode (with what it holds): drawn in the overlay
    /// while the scene, built once without it, stays still.
    sliding: Option<HashSet<String>>,
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
    /// Where the button went down, and whether the pointer has left that
    /// spot since: the edge scroll waits for a real drag.
    press_at: Pos,
    press_moved: bool,
    /// When the edge scroll last moved the view (its speed is per second).
    edge_at: Option<Instant>,
    pub search: Option<Search>,
    flight: Option<Flight>,
    pub undo_stack: Vec<Change>,
    pub redo_stack: Vec<Change>,
    pub step_base: Option<Snapshot>,
    /// Steps committed since the shell last took them (for history and saving).
    pub done: Vec<Change>,
    pub request: Option<Request>,
    /// Path mode: the walk, and the board as drawn with its offsets.
    pub trail: Option<Trail>,
    display: Option<Board>,
    /// Recent walks, newest first: entering on a concept of one brings it
    /// back. Shells keep them on disk (`walks_json`).
    walks: Vec<Walk>,
    /// Text being typed: earlier states (text, caret) for Ctrl+Z, and
    /// those undone for Ctrl+Y; typing is grouped into words and pauses.
    text_undo: Vec<(String, usize)>,
    text_redo: Vec<(String, usize)>,
    text_last: Option<(Instant, bool)>,
    /// The last step taken by a tap, and when: the second tap of a double
    /// tap edits that concept even though the camera moved under it.
    last_step: Option<(String, Instant)>,
    /// Layout of the walk, kept for the next one.
    pub layout: Layout,
    /// Screen y where the path bar starts (under the shell's own strip).
    pub ui_top: f32,
    bar_hits: Vec<([f32; 4], i32)>,
    /// Arrowheads (from end, to end) of the next new connection: the look
    /// of the last one made or changed.
    pub next_arrows: (bool, bool),
    /// When the hovered concept changed (its outline thickens smoothly).
    hover_at: Instant,
    /// The selection before a press picked another concept: a finger
    /// that only drags it puts the selection back (no bar pops up for a
    /// concept just moved).
    sel_before: Option<Vec<String>>,
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
            sliding: None,
            show_nested_links: false,
            drag_pending: false,
            hidden_nodes: HashSet::new(),
            hidden_edges: HashSet::new(),
            cursor: Pos { x: -1.0, y: -1.0 },
            shift: false,
            ui,
            touch,
            caret_epoch: crate::clock::now(),
            caret: 0,
            anchor: 0,
            text_drag: false,
            text_box: None,
            menu_at: (0.0, 0.0),
            right_press: false,
            press_at: Pos { x: -1.0, y: -1.0 },
            press_moved: false,
            edge_at: None,
            search: None,
            flight: None,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            step_base: None,
            done: Vec::new(),
            request: None,
            trail: None,
            display: None,
            walks: Vec::new(),
            last_step: None,
            text_undo: Vec::new(),
            text_redo: Vec::new(),
            text_last: None,
            layout: Layout::default(),
            ui_top: 0.0,
            bar_hits: Vec::new(),
            next_arrows: (false, true),
            hover_at: crate::clock::now(),
            sel_before: None,
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
        self.trail = None;
        self.walks.clear();
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

    /// Concepts showing connection dots: the single selected one (hover
    /// only thickens the outline; dots under a moving pointer were hard to
    /// catch).
    fn dot_nodes(&self) -> Vec<String> {
        match &self.sel[..] {
            [one] => vec![one.clone()],
            _ => Vec::new(),
        }
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
        self.preview.as_ref().or(self.display.as_ref()).unwrap_or(&self.board)
    }

    // ------------------------------------------------------------ scene

    /// Rebuilds routes and the indexed scene, minus what the overlay draws.
    pub fn rebuild(&mut self) {
        // A slide starting from where everything is drawn now: only
        // connections new to the board as drawn need a route; the rest are
        // routed when it ends.
        let only = self.trail.as_ref().is_some_and(|t| t.animating()).then(HashSet::new);
        self.rebuild_routing(only);
    }

    /// `only`: the connections to route again (with those the board as
    /// drawn has no route for yet); `None` routes all.
    fn rebuild_routing(&mut self, only: Option<HashSet<String>>) {
        self.sync_trail();
        self.sliding = None;
        let board = self.shown().clone();
        self.raw_routes = route::raw_routes(&board, self.show_nested_links, only.as_ref(), &self.raw_routes);
        self.routes = route::finish(&board, &self.raw_routes);
        self.build_scene(&board);
    }

    fn build_scene(&mut self, board: &Board) {
        let bright = self.trail.as_ref().filter(|t| !t.leaving).map(|t| t.bright(&self.board));
        let m = |t: &str, s: f32, w: f32| self.engine.wrapped_lines(t, s, w);
        let skip = view::Skip { nodes: &self.hidden_nodes, edges: &self.hidden_edges, bright: bright.as_ref(), moving: self.sliding.as_ref() };
        let scene = view::build(board, &self.routes, &m, &skip, view::bg_color());
        self.engine.set_scene(scene);
    }

    /// While concepts slide: only what slides moves in the board as drawn
    /// (its structure stays as the last rebuild left it) and its connections
    /// go straight between their ends. The scene is built once per slide
    /// without them; `slide_into` draws them over it each frame.
    fn rebuild_moving(&mut self) {
        let Some(t) = &self.trail else { return };
        let Some(mut board) = self.display.take() else { return self.rebuild() };
        if self.sliding.is_none() {
            let mut moving: HashSet<String> = HashSet::new();
            for id in t.moving() {
                if self.board.get(&id).is_some() {
                    moving.extend(self.board.subtree(&id));
                }
            }
            self.sliding = Some(moving);
            self.build_scene(&board);
        }
        let (Some(t), Some(moving)) = (&self.trail, &self.sliding) else { return };
        t.pose_all(&self.board, &mut board, moving);
        for e in &board.edges {
            if (moving.contains(&e.from) || moving.contains(&e.to)) && route::shown(&board, e, self.show_nested_links) {
                if let (Some(a), Some(b)) = (board.get(&e.from), board.get(&e.to)) {
                    self.routes.insert(e.id.clone(), route::clip(&[a.center(), b.center()], &a.bbox(), &b.bbox()));
                }
            }
        }
        self.display = Some(board);
    }

    /// What slides, under the walk: dimmed (or plain, when the walk ends)
    /// unless the walk draws it bright itself.
    fn slide_into(&self, board: &Board, ov: &mut Scene) {
        let Some(moving) = &self.sliding else { return };
        let m = |t: &str, s: f32, w: f32| self.engine.wrapped_lines(t, s, w);
        let bright = self.trail.as_ref().filter(|t| !t.leaving).map(|t| t.bright(&self.board));
        let lit = |id: &str| bright.as_ref().is_some_and(|b| b.contains(id));
        let look = if bright.is_some() { Look::Dim } else { Look::Plain };
        for id in board.draw_order() {
            if moving.contains(&id) && !lit(&id) && !self.hidden_nodes.contains(&id) {
                view::node_styled(board, board.node(&id), &m, look, ov);
            }
        }
        for e in &board.edges {
            if (moving.contains(&e.from) || moving.contains(&e.to)) && !(lit(&e.from) && lit(&e.to)) && !self.hidden_edges.contains(&e.id) {
                if let Some(p) = self.routes.get(&e.id) {
                    view::edge_styled(board, &e.id, p, &m, view::bg_color(), look == Look::Dim, ov);
                }
            }
        }
    }

    /// Keeps the walk in step with the board (deleted and new concepts)
    /// and refreshes the board as drawn.
    fn sync_trail(&mut self) {
        let Some(t) = &mut self.trail else {
            self.display = None;
            return;
        };
        if !t.leaving {
            let moves = t.sync(&self.board);
            // A concept made where the current one is drawn moves to lie
            // beside it on the board too; only inside an action, so the
            // move is part of that step.
            if self.step_base.is_some() {
                for (id, o) in moves {
                    self.board.move_tree(&id, o.0, o.1);
                }
            }
            if t.links.is_empty() {
                self.trail = None;
                self.display = None;
                return;
            }
        }
        self.display = Some(t.display(&self.board));
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
        let t = crate::clock::now();
        match self.mode {
            Mode::Move { .. } => self.drag_move(),
            Mode::Resize { .. } => self.drag_resize(),
            Mode::PathDrag { .. } => self.drag_path(),
            _ => {}
        }
        if let Some(p) = std::env::var_os("HS_PROF") {
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new().append(true).create(true).open(p) {
                let _ = writeln!(f, "drag {:.2} ms", crate::clock::since(t).as_secs_f64() * 1000.0);
            }
        }
    }

    /// Selection, handles, things being dragged, caret, guides. Called once
    /// per frame, so it also applies a pending drag.
    pub fn refresh_overlay(&mut self) {
        let started = crate::clock::now();
        self.refresh_overlay_inner();
        if self.trail.is_some() {
            if let Some(p) = std::env::var_os("HS_PROF") {
                use std::io::Write;
                if let Ok(mut f) = std::fs::OpenOptions::new().append(true).create(true).open(p) {
                    let _ = writeln!(f, "frame {:.2} ms", crate::clock::since(started).as_secs_f64() * 1000.0);
                }
            }
        }
    }

    fn refresh_overlay_inner(&mut self) {
        self.edge_pan();
        self.flush_drag();
        self.fly();
        self.tick_trail();
        let mut ov = Scene::default();
        let px = self.px();
        let board = self.shown();
        let m = |t: &str, s: f32, w: f32| self.engine.wrapped_lines(t, s, w);
        self.slide_into(board, &mut ov);
        self.walk_into(board, &mut ov);
        // Live elements.
        let order = if self.hidden_nodes.is_empty() { Vec::new() } else { board.draw_order() };
        for id in order {
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
        // The concept under the mouse: its outline thickens smoothly.
        if let Some(n) = self.hover.as_ref().filter(|h| !self.touch && !self.sel.contains(h) && matches!(self.mode, Mode::Idle)).and_then(|h| board.get(h)) {
            let k = self.hover_k();
            if k > 0.0 {
                let o = 1.6 * dp * k;
                let a = (k * 255.0) as u8;
                let ink = crate::scene::parse_color(n.color.as_deref().unwrap_or(view::INK), 1.0) & 0x00ff_ffff | (a as u32) << 24;
                let shape = match n.shape {
                    NShape::Ellipse => Shape::Ellipse,
                    NShape::Diamond => Shape::Diamond,
                    _ => Shape::Rect,
                };
                let radius = if shape == Shape::Rect && n.shape != NShape::Text { (n.w.min(n.h) * 0.12).min(n.font * 0.8) + o } else { 0.0 };
                ov.rects.push(Rect { x: n.x - o, y: n.y - o, w: n.w + 2.0 * o, h: n.h + 2.0 * o, fill: 0, stroke: ink, stroke_w: o, radius, shape });
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
                if !matches!(self.mode, Mode::EdgeEnd { moved: true, .. }) {
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
                    if (crate::clock::since(self.caret_epoch).as_millis() / 530) % 2 == 0 || self.text_drag {
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
            Mode::EdgeEnd { edge, moving_from, moved: true, .. } => {
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

    /// How fast the view should scroll while something is dragged to the
    /// window's edge (screen pixels per second), if it should.
    fn edge_push(&self) -> Option<(f64, f64)> {
        let dragging = matches!(self.mode, Mode::Connect { .. } | Mode::EdgeEnd { moved: true, .. } | Mode::Move { .. } | Mode::Boxing { .. } | Mode::PathDrag { .. });
        if !dragging || self.frozen || !self.press_moved {
            return None;
        }
        let (w, h) = self.engine.size();
        let band = 36.0 * self.ui as f64;
        let c = self.cursor;
        let push = |d: f64| if d < band { ((band - d) / band).min(1.5) } else { 0.0 };
        let top = self.ui_top as f64;
        let (vx, vy) = (push(c.x) - push(w as f64 - c.x), push(c.y - top) - push(h as f64 - c.y));
        let speed = 720.0 * self.ui as f64;
        (vx != 0.0 || vy != 0.0).then_some((vx * speed, vy * speed))
    }

    /// Scrolls the view while a connection, concept or box is dragged to
    /// the window's edge; what is dragged stays under the pointer.
    fn edge_pan(&mut self) {
        let Some((vx, vy)) = self.edge_push() else {
            self.edge_at = None;
            return;
        };
        let now = crate::clock::now();
        let dt = self.edge_at.map_or(1.0 / 60.0, |t| now.duration_since(t).as_secs_f64().min(0.05));
        self.edge_at = Some(now);
        self.engine.pan(vx * dt, vy * dt);
        if matches!(self.mode, Mode::Move { .. } | Mode::PathDrag { .. }) {
            self.drag_pending = true;
        }
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

    fn tick_trail(&mut self) {
        let Some(t) = &mut self.trail else { return };
        if !t.animating() {
            return;
        }
        t.tick();
        let started = crate::clock::now();
        let (done, leaving) = (!t.animating(), t.leaving);
        if done && leaving {
            self.trail = None;
            self.rebuild();
        } else if done {
            // Only connections of what slid have moved; the rest keep their routes.
            match self.sliding.take() {
                Some(moved) => {
                    let only = self.shown().edges.iter().filter(|e| moved.contains(&e.from) || moved.contains(&e.to)).map(|e| e.id.clone()).collect();
                    self.rebuild_routing(Some(only));
                }
                None => self.rebuild(),
            }
        } else {
            self.rebuild_moving();
        }
        if let Some(p) = std::env::var_os("HS_PROF") {
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new().append(true).create(true).open(p) {
                let _ = writeln!(f, "slide {:.2} ms", crate::clock::since(started).as_secs_f64() * 1000.0);
            }
        }
    }

    /// The walk on top of the dimmed board: concepts out on cards, their
    /// connections, the path line, the current concept, "+N" badges.
    fn walk_into(&self, board: &Board, ov: &mut Scene) {
        let Some(t) = self.trail.as_ref().filter(|t| !t.leaving) else { return };
        let m = |t: &str, s: f32, w: f32| self.engine.wrapped_lines(t, s, w);
        let bright = t.bright(&self.board);
        let walk: Vec<&String> = t.links.iter().chain(t.touched.iter()).collect();
        for id in board.draw_order() {
            if bright.contains(&id) && !self.hidden_nodes.contains(&id) {
                // A frame around the walk keeps the dimmed things it holds visible.
                let around = walk.iter().any(|w| board.is_ancestor(&id, w));
                view::node_styled(board, board.node(&id), &m, if around { Look::Plain } else { Look::Card }, ov);
            }
        }
        for e in &board.edges {
            if bright.contains(&e.from) && bright.contains(&e.to) && !self.hidden_edges.contains(&e.id) {
                if let Some(p) = self.routes.get(&e.id) {
                    view::edge_into(board, &e.id, p, &m, view::bg_color(), ov);
                }
            }
        }
        let dp = self.dp();
        for w in t.links.windows(2) {
            let (Some(a), Some(b)) = (board.get(&w[0]), board.get(&w[1])) else { continue };
            let p = board
                .edge_between(&w[0], &w[1])
                .and_then(|k| self.routes.get(&board.edges[k].id).cloned())
                .unwrap_or_else(|| route::clip(&[a.center(), b.center()], &a.bbox(), &b.bbox()));
            for s in p.windows(2) {
                ov.lines.push(Line { x0: s[0].0, y0: s[0].1, x1: s[1].0, y1: s[1].1, color: path_red(200), width: 3.5 * dp, head: false });
            }
        }
        if let Some(n) = board.get(t.current()) {
            let b = n.bbox();
            let pad = 5.0 * dp;
            ov.rects.push(Rect { x: b[0] - pad, y: b[1] - pad, w: b[2] - b[0] + 2.0 * pad, h: b[3] - b[1] + 2.0 * pad, fill: 0, stroke: path_red(255), stroke_w: 2.5 * dp, radius: 4.0 * dp, shape: Shape::Rect });
        }
        // Close buttons: a small grey cross on the top left corner.
        for (_, c, r) in self.close_spots() {
            ov.rects.push(Rect { x: c.0 - r, y: c.1 - r, w: 2.0 * r, h: 2.0 * r, fill: rgba(0x8a, 0x8a, 0x8a, 235), stroke: rgba(255, 255, 255, 255), stroke_w: 1.5 * self.px(), radius: 0.0, shape: Shape::Ellipse });
            let a = r * 0.42;
            for (sx, sy) in [(1.0, 1.0), (1.0, -1.0)] {
                ov.lines.push(Line { x0: c.0 - a * sx, y0: c.1 - a * sy, x1: c.0 + a * sx, y1: c.1 + a * sy, color: rgba(255, 255, 255, 255), width: 1.6 * self.px(), head: false });
            }
        }
        for id in t.out() {
            let k = t.unopened(&self.board, &id);
            let Some(n) = board.get(&id).filter(|_| k > 0) else { continue };
            let size = 11.0 * dp;
            let text = format!("+{k}");
            let w = size * (0.62 * text.chars().count() as f32 + 1.0);
            let h = size * 1.5;
            let (x, y) = (n.x + n.w - w * 0.6, n.y - h * 0.55);
            ov.rects.push(Rect { x, y, w, h, fill: path_red(255), stroke: 0, stroke_w: 0.0, radius: h * 0.5, shape: Shape::Rect });
            ov.labels.push(Label { x, y: y + (h - size * LINE_H) * 0.5, w, size, color: rgba(255, 255, 255, 255), align: Align::Center, wrap: false, text });
        }
    }

    fn handles_visible(&self) -> bool {
        matches!(self.mode, Mode::Idle | Mode::Resize { .. } | Mode::Press { .. }) && !self.frozen && !self.path_active()
    }

    /// How far the hover outline has thickened, 0..1 over 140 ms.
    fn hover_k(&self) -> f32 {
        let t = (crate::clock::since(self.hover_at).as_secs_f32() / 0.14).min(1.0);
        1.0 - (1.0 - t) * (1.0 - t)
    }

    /// True while the overlay changes by itself (caret blink).
    pub fn animating(&self) -> bool {
        matches!(self.mode, Mode::Text { .. }) || self.flight.is_some() || self.trail.as_ref().is_some_and(|t| t.animating()) || (self.hover.is_some() && self.hover_k() < 1.0) || self.edge_push().is_some()
    }

    // ------------------------------------------------------------ path mode

    /// The walk is on (not just sliding back after it ended).
    pub fn path_active(&self) -> bool {
        self.trail.as_ref().is_some_and(|t| !t.leaving)
    }

    /// "Path": starts walking from the selected concept (one of the last
    /// walk's links brings that walk back).
    pub fn path_enter(&mut self) {
        self.commit_text();
        let [one] = &self.sel[..] else { return };
        let one = one.clone();
        if self.board.get(&one).is_none() {
            return;
        }
        let (links, kept) = match self.walks.iter().find(|w| w.links.contains(&one)) {
            Some(w) => (w.links.iter().filter(|l| self.board.get(l).is_some()).cloned().collect(), w.kept.iter().filter(|(id, _)| self.board.get(id).is_some()).cloned().collect()),
            None => (vec![one], Vec::new()),
        };
        self.trail = Some(Trail::new(&self.board, links, &kept, self.undo_stack.len(), self.layout));
        self.after_step();
    }

    /// Back arrow or Esc: everything slides home; the walk is remembered.
    pub fn path_exit(&mut self) {
        self.commit_text();
        let Some(t) = &mut self.trail else { return };
        if t.leaving {
            return;
        }
        let w = Walk { links: t.links.clone(), kept: t.kept() };
        t.leave();
        self.remember_walk(w);
        self.mode = Mode::Idle;
        self.rebuild();
    }

    fn remember_walk(&mut self, w: Walk) {
        self.walks.retain(|o| !o.links.iter().any(|l| w.links.contains(l)));
        self.walks.insert(0, w);
        self.walks.truncate(WALKS_KEPT);
    }

    /// Recent walks and the one going on, for the shell to keep on disk.
    pub fn walks_json(&self) -> String {
        let mut walks = self.walks.clone();
        let active = self.trail.as_ref().filter(|t| !t.leaving).map(|t| Walk { links: t.links.clone(), kept: t.kept() });
        if let Some(w) = &active {
            walks.retain(|o| !o.links.iter().any(|l| w.links.contains(l)));
            walks.insert(0, w.clone());
        }
        let pose = |p: &crate::trail::Pose| serde_json::json!([p.dx, p.dy, p.dw, p.dh, p.df]);
        let list: Vec<_> = walks
            .iter()
            .take(WALKS_KEPT)
            .map(|w| serde_json::json!({ "links": w.links, "kept": w.kept.iter().map(|(id, p)| serde_json::json!([id, pose(p)])).collect::<Vec<_>>() }))
            .collect();
        serde_json::json!({ "active": active.is_some(), "walks": list }).to_string()
    }

    /// Brings back walks kept by `walks_json`; the walk that was going on
    /// when the app closed goes on again.
    pub fn set_walks_json(&mut self, s: &str) {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(s) else { return };
        let f = |x: &serde_json::Value| x.as_f64().unwrap_or(0.0) as f32;
        self.walks = v["walks"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|w| {
                let links: Vec<String> = w["links"].as_array()?.iter().filter_map(|l| l.as_str().map(String::from)).collect();
                let kept = w["kept"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|k| {
                        let p = k[1].as_array()?;
                        Some((k[0].as_str()?.to_string(), crate::trail::Pose { dx: f(&p[0]), dy: f(&p[1]), dw: f(&p[2]), dh: f(&p[3]), df: f(&p[4]) }))
                    })
                    .collect();
                (!links.is_empty()).then_some(Walk { links, kept })
            })
            .collect();
        if v["active"].as_bool() == Some(true) && self.trail.is_none() {
            let last = self.walks.first().and_then(|w| w.links.iter().rev().find(|l| self.board.get(l).is_some()).cloned());
            if let Some(id) = last {
                self.sel = vec![id];
                // The camera opens on the walk, as close as it fits.
                self.engine.cam.s = f64::MAX;
                self.path_enter();
                // Back where it was at once, not sliding out again.
                if let Some(t) = &mut self.trail {
                    t.finish();
                }
                if let Some(f) = self.flight.take() {
                    (self.engine.cam.cx, self.engine.cam.cy, self.engine.cam.s) = f.to;
                }
                self.rebuild();
            }
        }
    }

    /// "Remove from path": a concept out goes back to its place.
    pub fn path_close(&mut self, id: &str) {
        self.commit_text();
        let Some(t) = self.trail.as_mut().filter(|t| !t.leaving) else { return };
        if t.close(&self.board, id) {
            let cur = t.current().to_string();
            self.sel = vec![cur];
            self.sel_edge = None;
            self.rebuild();
        }
    }

    /// Whether `id` can be closed by hand now.
    pub fn path_closable(&self, id: &str) -> bool {
        self.trail.as_ref().filter(|t| !t.leaving).is_some_and(|t| t.closable().iter().any(|c| c == id))
    }

    /// Where the close button of each concept that can be closed sits:
    /// (id, centre, radius) in world units.
    fn close_spots(&self) -> Vec<(String, Pt, f32)> {
        let Some(t) = self.trail.as_ref().filter(|t| !t.leaving) else { return Vec::new() };
        let board = self.shown();
        let r = 8.0 * self.dp();
        t.closable().into_iter().filter_map(|id| board.get(&id).map(|n| (id.clone(), (n.x, n.y), r))).collect()
    }

    /// The close button under the pointer, if any.
    fn close_hit(&self) -> Option<String> {
        let p = self.world();
        let grab = if self.touch { 2.0 } else { 1.3 };
        self.close_spots().into_iter().find(|(_, c, r)| (p.0 - c.0).hypot(p.1 - c.1) <= r * grab).map(|s| s.0)
    }

    /// Switches the walk's layout (`None`: the next one); the next walk
    /// starts in it too.
    pub fn path_layout(&mut self, l: Option<Layout>) {
        self.commit_text();
        let Some(t) = self.trail.as_mut().filter(|t| !t.leaving) else { return };
        let l = l.unwrap_or(t.layout.next());
        t.set_layout(&self.board, l);
        self.layout = l;
        self.after_step();
    }

    /// After the walk changed: the current concept selected, the camera
    /// takes in it and what came out.
    fn after_step(&mut self) {
        let Some(t) = &self.trail else { return };
        self.sel = vec![t.current().to_string()];
        self.sel_edge = None;
        let mut b: Option<Bbox> = None;
        let mut ids: Vec<String> = t.open.clone();
        ids.push(t.current().to_string());
        for id in &ids {
            let Some(nb) = t.target_box(&self.board, id) else { continue };
            b = Some(b.map_or(nb, |a| [a[0].min(nb[0]), a[1].min(nb[1]), a[2].max(nb[2]), a[3].max(nb[3])]));
        }
        self.rebuild();
        let Some(b) = b else { return };
        // Keep the zoom unless it all does not fit; centre on it.
        let (w, h) = self.engine.size();
        let cam = &self.engine.cam;
        let fit = ((w as f64 / (b[2] - b[0]).max(1.0) as f64).min((h as f64 - self.ui_top as f64) / (b[3] - b[1]).max(1.0) as f64)) * 0.8;
        let s = cam.s.min(fit);
        let to = ((b[0] + b[2]) as f64 * 0.5, (b[1] + b[3]) as f64 * 0.5 - self.ui_top as f64 * 0.25 / s, s);
        self.flight = Some(Flight { from: (cam.cx, cam.cy, cam.s), to, start: crate::clock::now(), ms: crate::trail::EASE_MS });
    }

    /// Middle of the path bar's link `k` in screen pixels (scripts tap it).
    pub fn bar_spot(&self, k: i32) -> Option<(f32, f32)> {
        self.bar_hits.iter().find(|h| h.1 == k).map(|(b, _)| ((b[0] + b[2]) * 0.5, (b[1] + b[3]) * 0.5))
    }

    /// The path bar in screen pixels: back arrow, then the links. Shells
    /// put it in their screen layer each frame; taps on it go to `press`.
    pub fn path_ui(&mut self, top: f32) -> Scene {
        self.ui_top = top;
        self.bar_hits.clear();
        let mut ui = Scene::default();
        let Some(t) = self.trail.as_ref().filter(|t| !t.leaving) else { return ui };
        let s = self.ui;
        let (w, _) = self.engine.size();
        let (h, size) = (36.0 * s, 14.0 * s);
        let (x0, y) = (10.0 * s, top + 8.0 * s);
        let ink = rgba(0x2a, 0x2a, 0x2a, 255);
        let name = |id: &str| {
            let t = self.board.get(id).map_or(String::new(), |n| n.text.split('\n').next().unwrap_or("").trim().to_string());
            let t = if t.is_empty() { crate::i18n::tr("untitled", "без названия").to_string() } else { t };
            if t.chars().count() > 18 {
                format!("{}…", t.chars().take(17).collect::<String>())
            } else {
                t
            }
        };
        let sep = "  ›  ";
        let sep_w = self.engine.measure(sep, size).0;
        let pad = 12.0 * s;
        let mode = t.layout.name();
        let mode_w = self.engine.measure(mode, size).0 + 2.0 * pad;
        let room = w as f32 - x0 * 2.0 - h - mode_w - pad;
        // Links from the end while they fit; the rest fold into "…".
        let mut items: Vec<(i32, String, f32)> = Vec::new();
        let mut used = 0.0;
        for (k, id) in t.links.iter().enumerate().rev() {
            let n = name(id);
            let nw = self.engine.measure(&n, size).0;
            let add = nw + if items.is_empty() { 0.0 } else { sep_w };
            if used + add > room && !items.is_empty() {
                let ew = self.engine.measure("…", size).0;
                items.insert(0, (-2, "…".into(), ew));
                break;
            }
            used += add;
            items.insert(0, (k as i32, n, nw));
        }
        let total = h + mode_w + items.iter().map(|i| i.2).sum::<f32>() + sep_w * (items.len().saturating_sub(1)) as f32 + pad;
        ui.rects.push(Rect { x: x0, y, w: total, h, fill: rgba(255, 255, 255, 245), stroke: rgba(0, 0, 0, 35), stroke_w: 1.0 * s, radius: 10.0 * s, shape: Shape::Rect });
        let ty = y + (h - size * LINE_H) * 0.5;
        ui.labels.push(Label { x: x0, y: y + (h - 18.0 * s * LINE_H) * 0.5, w: h, size: 18.0 * s, color: path_red(255), align: Align::Center, wrap: false, text: "←".into() });
        self.bar_hits.push(([x0, y, x0 + h, y + h], BAR_BACK));
        // The layout switch: a tap goes to the next one.
        let (mx, inset) = (x0 + h, 5.0 * s);
        ui.rects.push(Rect { x: mx, y: y + inset, w: mode_w - pad * 0.5, h: h - 2.0 * inset, fill: rgba(0, 0, 0, 14), stroke: 0, stroke_w: 0.0, radius: 7.0 * s, shape: Shape::Rect });
        ui.labels.push(Label { x: mx, y: ty, w: mode_w - pad * 0.5, size, color: ink, align: Align::Center, wrap: false, text: mode.into() });
        self.bar_hits.push(([mx, y, mx + mode_w - pad * 0.5, y + h], BAR_LAYOUT));
        let mut x = x0 + h + mode_w;
        let last = items.len().saturating_sub(1);
        for (i, (k, text, tw)) in items.into_iter().enumerate() {
            let color = if i == last { path_red(255) } else { ink };
            ui.labels.push(Label { x, y: ty, w: tw + 2.0 * s, size, color, align: Align::Left, wrap: false, text });
            if k >= 0 {
                self.bar_hits.push(([x - sep_w * 0.5, y, x + tw + sep_w * 0.5, y + h], k));
            }
            x += tw;
            if i != last {
                ui.labels.push(Label { x, y: ty, w: sep_w, size, color: rgba(0, 0, 0, 90), align: Align::Left, wrap: false, text: sep.into() });
                x += sep_w;
            }
        }
        ui
    }

    /// A press on the path bar, if it is there.
    fn bar_hit(&self) -> Option<i32> {
        if !self.path_active() {
            return None;
        }
        let (x, y) = (self.cursor.x as f32, self.cursor.y as f32);
        self.bar_hits.iter().find(|(b, _)| x >= b[0] && x <= b[2] && y >= b[1] && y <= b[3]).map(|h| h.1)
    }

    fn bar_press(&mut self, k: i32) {
        if k == BAR_BACK {
            self.path_exit();
            return;
        }
        if k == BAR_LAYOUT {
            self.path_layout(None);
            return;
        }
        if let Some(t) = &mut self.trail {
            t.back_to(&self.board, k as usize);
        }
        self.after_step();
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
        self.flight = Some(Flight { from: (cam.cx, cam.cy, cam.s), to, start: crate::clock::now(), ms: 450.0 });
    }

    /// Moves the camera along a flight; called once per frame.
    fn fly(&mut self) {
        let Some(f) = &self.flight else { return };
        let t = (crate::clock::since(f.start).as_secs_f64() * 1000.0 / f.ms).min(1.0);
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
            if let (Some(path), Some(e)) = (self.routes.get(eid), board.edge(eid)) {
                let near = |q: Pt| (q.0 - p.0).hypot(q.1 - p.1) < reach;
                // An end sits on its concept's border: a press inside the
                // concept is meant for the concept, not the arrowhead.
                let off = |id: &str| board.get(id).is_none_or(|n| !inside(&n.bbox(), p, -tol * 0.5));
                if near(path[0]) && off(&e.from) {
                    return Hit::EdgeEnd(eid.clone(), true);
                }
                if near(path[path.len() - 1]) && off(&e.to) {
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
        // In the walk only what is out in front can be hit; the dimmed
        // board behind is background.
        let front = self.front();
        let live = |id: &str| front.as_ref().is_none_or(|f| f.contains(id));
        // Text concepts sit on top of whatever they overlap.
        for n in board.nodes.iter().rev() {
            if n.shape == NShape::Text && live(&n.id) && inside(&n.bbox(), p, tol) {
                return Hit::Node(n.id.clone());
            }
        }
        // A concept under the pointer wins over a connection crossing it:
        // a press usually means grabbing, and zoomed out connections cover
        // much of the screen.
        let under = match self.deepest(p) {
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
        for e in board.edges.iter().filter(|e| e.kind == Kind::Link && live(&e.from) && live(&e.to)) {
            if let Some(path) = self.routes.get(&e.id) {
                if path.windows(2).any(|w| seg_dist(p, w[0], w[1]) < tol) {
                    return Hit::Edge(e.id.clone());
                }
            }
        }
        under
    }

    /// In the walk: the concepts out in front (the rest is background).
    fn front(&self) -> Option<HashSet<String>> {
        self.trail.as_ref().filter(|t| !t.leaving).map(|t| t.bright(&self.board))
    }

    /// The smallest concept under `p` that can be hit (in the walk: only
    /// those out in front, whatever dimmed lies under them).
    fn deepest(&self, p: Pt) -> Option<String> {
        let board = self.shown();
        let Some(front) = self.front() else { return board.deepest_at(p.0, p.1, &HashSet::new()) };
        board
            .nodes
            .iter()
            .filter(|n| n.shape != NShape::Text && front.contains(&n.id) && inside(&n.bbox(), p, 0.0))
            .min_by(|a, b| (a.w * a.h).total_cmp(&(b.w * b.h)))
            .map(|n| n.id.clone())
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
        let front = self.front();
        let t = board
            .nodes
            .iter()
            .rev()
            .find(|n| n.shape == NShape::Text && front.as_ref().is_none_or(|f| f.contains(&n.id)) && inside(&n.bbox(), p, tol))
            .map(|n| n.id.clone())
            .or_else(|| self.deepest(p))?;
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
            _ => None,
        };
        if new != self.hover {
            self.hover = new;
            self.hover_at = crate::clock::now();
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
        // While typing, Ctrl+Z undoes the typing, never more.
        if self.editing_text() {
            self.text_undo_step(redo);
            return;
        }
        self.commit_text();
        // In the walk Ctrl+Z first undoes edits made since the last step,
        // then the step itself.
        if !redo {
            if let Some(t) = &mut self.trail {
                if !t.leaving && self.undo_stack.len() <= t.marks.last().copied().unwrap_or(0) {
                    if t.pop(&self.board) {
                        self.after_step();
                    }
                    return;
                }
            }
        }
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
        (self.press_at, self.press_moved) = (self.cursor, false);
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
                self.caret_epoch = crate::clock::now();
                return;
            }
        }
        self.commit_text();
        if let Some(k) = self.bar_hit() {
            self.bar_press(k);
            self.mode = Mode::Idle;
            return;
        }
        if let Some(id) = self.close_hit() {
            self.path_close(&id);
            self.mode = Mode::Idle;
            return;
        }
        // Hover is not tracked while typing: find what is under the pointer now.
        self.update_hover();
        if self.frozen {
            self.mode = Mode::Pan { moved: true, at: self.cursor };
            return;
        }
        // In the walk `hit` skips the dimmed board: pressing it pans.
        let hit = self.hit();
        let at = self.cursor;
        if double {
            if let Some((id, t)) = self.last_step.take() {
                if self.path_active() && crate::clock::since(t).as_millis() < 600 && self.board.get(&id).is_some() {
                    self.flight = None;
                    self.start_text(TextTarget::Node(id), false);
                    return;
                }
            }
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
            Hit::EdgeEnd(e, from) => self.mode = Mode::EdgeEnd { edge: e, moving_from: from, at, moved: false },
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
                    self.sel_before = Some(std::mem::replace(&mut self.sel, vec![id.clone()]));
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
        (self.press_at, self.press_moved) = (self.cursor, false);
        if self.frozen {
            return;
        }
        let w = self.engine.screen_to_world(self.cursor.x, self.cursor.y);
        self.mode = Mode::Boxing { from: w, add: shift };
    }

    /// The mouse is selecting text being edited.
    pub fn selecting_text(&self) -> bool {
        self.text_drag
    }

    /// Ends a text selection whose button release never arrived.
    pub fn end_text_drag(&mut self) {
        self.text_drag = false;
    }

    pub fn panning(&self) -> bool {
        matches!(self.mode, Mode::Pan { .. })
    }

    /// Pointer moved while a button is down. Returns true if it needs a redraw.
    pub fn drag_to(&mut self, prev: Pos) -> bool {
        if (self.press_at.x - self.cursor.x).hypot(self.press_at.y - self.cursor.y) > 4.0 {
            self.press_moved = true;
        }
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
            Mode::Move { .. } | Mode::Resize { .. } | Mode::PathDrag { .. } => {
                self.drag_pending = true;
                true
            }
            Mode::EdgeEnd { at, moved, .. } => {
                if (at.x - cur.x).hypot(at.y - cur.y) > 4.0 {
                    *moved = true;
                }
                true
            }
            Mode::Boxing { .. } | Mode::Connect { .. } => true,
            _ => false,
        }
    }

    fn start_move(&mut self, at: Pos) {
        // In the walk a drag only moves where the concept is drawn.
        if let Some(t) = self.trail.as_ref().filter(|t| !t.leaving) {
            let Some(id) = self.sel.last().cloned() else {
                self.mode = Mode::Idle;
                return;
            };
            let base = t.own(&id).unwrap_or_else(|| t.target_of(&self.board, &id));
            let start = self.engine.screen_to_world(at.x, at.y);
            self.mode = Mode::PathDrag { id, start, base };
            self.drag_pending = true;
            return;
        }
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

    fn drag_path(&mut self) {
        let w = self.engine.screen_to_world(self.cursor.x, self.cursor.y);
        let Mode::PathDrag { id, start, base } = &self.mode else { return };
        let o = (base.0 + (w.0 - start.0) as f32, base.1 + (w.1 - start.1) as f32);
        let id = id.clone();
        if let Some(t) = &mut self.trail {
            t.drag(&id, o);
        }
        self.rebuild_moving();
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
        // The walk draws a copy of the board: rebuild that whole (boards
        // walked by hand are small).
        if self.trail.is_some() {
            self.rebuild();
            return;
        }
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
        let skip = view::Skip { nodes: &self.hidden_nodes, edges: &self.hidden_edges, bright: None, moving: None };
        let scene = view::build(&self.board, &self.routes, &m, &skip, view::bg_color());
        self.engine.set_scene(scene);
    }

    pub fn release(&mut self) {
        self.text_drag = false;
        self.flush_drag();
        let walking = self.path_active();
        let before = self.sel_before.take();
        if self.touch && matches!(self.mode, Mode::Move { .. } | Mode::PathDrag { .. }) {
            if let Some(b) = before {
                self.sel = b;
            }
        }
        match std::mem::replace(&mut self.mode, Mode::Idle) {
            // In the walk a tap on empty canvas folds untouched neighbours.
            Mode::Pan { moved: false, .. } if walking => {
                if let Some(t) = &mut self.trail {
                    t.fold(&self.board);
                }
                if let Some(t) = &self.trail {
                    self.sel = vec![t.current().to_string()];
                }
                self.sel_edge = None;
            }
            // A tap on a concept out: a step to it (on the current one:
            // fold or unfold its neighbours).
            Mode::Press { node: Some(id), double: false, .. } if walking && !self.shift => {
                let undo_len = self.undo_stack.len();
                if let Some(t) = &mut self.trail {
                    if t.current() == id {
                        t.toggle(&self.board);
                    } else {
                        t.step(&self.board, &id, undo_len);
                    }
                }
                self.last_step = Some((id, crate::clock::now()));
                self.after_step();
                return;
            }
            Mode::PathDrag { id, .. } => {
                if let Some(t) = &mut self.trail {
                    t.touch(&id);
                }
                if self.trail.as_ref().is_some_and(|t| t.born(&id)) {
                    self.begin();
                    self.home_born(&id);
                    self.commit();
                }
            }
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
            Mode::EdgeEnd { edge, moving_from, moved: false, .. } => {
                // A click on an end turns its arrowhead on or off; the next
                // connection is drawn the same way.
                self.begin();
                if let Some(e) = self.board.edge_mut(&edge) {
                    if moving_from {
                        e.arrow_from = !e.arrow_from;
                    } else {
                        e.arrow_to = !e.arrow_to;
                    }
                    self.next_arrows = arrow_kind(e.arrow_from, e.arrow_to);
                }
                self.commit();
            }
            Mode::EdgeEnd { edge, moving_from, .. } => {
                let fixed = self.board.edge(&edge).map(|e| if moving_from { e.to.clone() } else { e.from.clone() });
                if let Some(fixed) = fixed {
                    if let Some(t) = self.connect_target(&fixed) {
                        let (a, b) = if moving_from { (t.clone(), fixed.clone()) } else { (fixed.clone(), t.clone()) };
                        if self.link_free(&a, &b) {
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
            if self.link_free(&from, &t) {
                self.begin();
                let e = self.add_link(&from, &t);
                self.commit();
                self.sel.clear();
                self.sel_edge = Some(e);
            }
            return;
        }
        let Some(a) = self.shown().get(&from).cloned() else { return };
        if inside(&a.bbox(), p, self.tol()) {
            return;
        }
        if self.path_active() {
            if let Some(id) = self.walk_new(&from, Some(p)) {
                self.edit_new(id);
            }
            return;
        }
        // Released on empty space: a new concept there, already connected.
        // In the walk the pointer is where things are drawn, not where they
        // lie: the new concept stays top level (the walk lays it out).
        let parent = if self.trail.is_some() { None } else { self.board.deepest_at(p.0, p.1, &HashSet::new()).filter(|t| !self.board.is_ancestor(&from, t) && *t != from) };
        self.begin();
        let id = self.place_new(p, parent.as_deref());
        if !self.board.is_ancestor(&id, &from) {
            self.add_link(&from, &id);
        }
        self.sel = vec![id.clone()];
        self.text_box = self.board.get(&id).map(|n| (n.w, n.h));
        self.mode = Mode::Text { target: TextTarget::Node(id), text: String::new(), created: true };
        (self.caret, self.anchor) = (0, 0);
        self.caret_epoch = crate::clock::now();
    }

    /// A new connection drawn the way the last one was.
    fn add_link(&mut self, from: &str, to: &str) -> String {
        let id = self.board.add_edge(from, to, Kind::Link);
        let (af, at) = self.next_arrows;
        if let Some(e) = self.board.edge_mut(&id) {
            (e.arrow_from, e.arrow_to) = (af, at);
        }
        id
    }

    /// Whether a connection `from` -> `to` may be added: two concepts hold
    /// at most two, one each way.
    fn link_free(&self, from: &str, to: &str) -> bool {
        let between: Vec<_> = self.board.edges.iter().filter(|e| (e.from == from && e.to == to) || (e.from == to && e.to == from)).collect();
        between.len() < 2 && !between.iter().any(|e| e.kind != Kind::Link || (e.from == from && e.to == to))
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
        let walking = self.trail.is_some();
        let mut board = std::mem::take(&mut self.board);
        board.fit_ancestors(&id, &one);
        // In the walk `p` is where things are drawn: overlaps on the board there mean nothing.
        if !walking {
            board.resolve_overlap(&id);
            board.fit_ancestors(&id, &one);
        }
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

    /// A new concept in the walk, made from `anchor` and connected to it:
    /// laid out as one more neighbour (or centred at `at`, where it was
    /// dropped). On the board it goes beside `anchor`, in the same frame,
    /// the way it sits beside it here; the walk keeps it drawn where it is.
    fn walk_new(&mut self, anchor: &str, at: Option<Pt>) -> Option<String> {
        let a = self.board.get(anchor).cloned()?;
        let parent = self.board.parent(anchor).map(String::from);
        self.begin();
        let id = self.board.new_id();
        // The look of the concept it continues; a frame gives a plain leaf.
        let (w, h, shape, fill) = if a.shape != NShape::Text && !self.board.has_children(anchor) { (a.w, a.h, a.shape, a.fill.clone()) } else { (a.font * 8.0, a.font * 4.0, NShape::Box, None) };
        self.board.add_node(Node { id: id.clone(), x: a.x + a.w + a.font * 2.0, y: a.y, w, h, text: String::new(), font: a.font, shape, color: None, fill, extra: Map::new() });
        if let Some(q) = &parent {
            self.board.set_parent(&id, Some(q));
        }
        self.add_link(anchor, &id);
        if let Some(t) = &mut self.trail {
            t.adopt(&self.board, &id, anchor, at);
        }
        self.home_born(&id);
        Some(id)
    }

    /// Puts a concept made in the walk on the board where it is drawn
    /// (beside what it came from), clear of others; it stays drawn there.
    fn home_born(&mut self, id: &str) {
        let Some(t) = &self.trail else { return };
        let (Some(home), Some((bx, f))) = (t.home_for(&self.board, id), t.drawn(&self.board, id)) else { return };
        if let Some(n) = self.board.get_mut(id) {
            (n.x, n.y, n.w, n.h, n.font) = home;
        }
        let eng = &self.engine;
        let one = move |n: &Node| view::title_lines(n, &|t: &str, s: f32, w: f32| eng.wrapped_lines(t, s, w));
        let mut board = std::mem::take(&mut self.board);
        board.resolve_overlap(id);
        board.fit_ancestors(id, &one);
        self.board = board;
        if let Some(t) = &mut self.trail {
            t.set_drawn(&self.board, id, bx, f);
        }
    }

    /// Text editing of a concept just made.
    fn edit_new(&mut self, id: String) {
        self.sel = vec![id.clone()];
        self.sel_edge = None;
        self.text_box = self.board.get(&id).map(|n| (n.w, n.h));
        self.mode = Mode::Text { target: TextTarget::Node(id), text: String::new(), created: true };
        (self.caret, self.anchor) = (0, 0);
        self.caret_epoch = crate::clock::now();
        self.text_undo.clear();
        self.text_redo.clear();
    }

    /// Double-click on empty canvas (or a parent's empty inside).
    fn create_at(&mut self, p: Pt, parent: Option<String>) {
        if let Some(cur) = self.trail.as_ref().filter(|t| !t.leaving).map(|t| t.current().to_string()) {
            if let Some(id) = self.walk_new(&cur, Some(p)) {
                self.edit_new(id);
                self.rebuild();
            }
            return;
        }
        let walking = self.trail.is_some();
        let parent = if walking { parent } else { parent.or_else(|| self.board.deepest_at(p.0, p.1, &HashSet::new())) };
        self.begin();
        let id = self.place_new(p, parent.as_deref());
        if walking {
            self.rebuild();
        }
        self.sel = vec![id.clone()];
        self.sel_edge = None;
        self.text_box = self.board.get(&id).map(|n| (n.w, n.h));
        self.mode = Mode::Text { target: TextTarget::Node(id), text: String::new(), created: true };
        (self.caret, self.anchor) = (0, 0);
        self.caret_epoch = crate::clock::now();
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
        self.caret_epoch = crate::clock::now();
        self.caret = text.chars().count();
        self.anchor = self.caret;
        self.mode = Mode::Text { target, text, created };
        self.text_undo.clear();
        self.text_redo.clear();
        self.text_last = None;
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
            // Off the text (on the concept's border or empty room) the press
            // leaves typing and picks the concept itself: handles, dots.
            let on = match target {
                TextTarget::Node(id) => board.get(id).is_some_and(|n| {
                    // An empty new concept would vanish on leaving: the whole of it is text.
                    if n.shape == NShape::Text || text.trim().is_empty() {
                        inside(&n.bbox(), p, self.tol())
                    } else if board.has_children(id) {
                        inside(&[n.x, n.y, n.x + n.w, n.y + board.title_h(n, self.title_lines(n))], p, 0.0)
                    } else {
                        self.on_text(n, p)
                    }
                }),
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
        self.caret_epoch = crate::clock::now();
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
        self.caret_epoch = crate::clock::now();
    }

    /// Remembers the text before an edit, one state per word or pause.
    fn text_mark(&mut self, deleting: bool, s: &str) {
        let Mode::Text { text, .. } = &self.mode else { return };
        let fresh = match self.text_last {
            None => true,
            Some((at, del)) => crate::clock::since(at).as_millis() > 900 || del != deleting || s.contains(char::is_whitespace),
        };
        if fresh && self.text_undo.last().is_none_or(|l| l.0 != *text) {
            self.text_undo.push((text.clone(), self.caret));
        }
        self.text_redo.clear();
        self.text_last = Some((crate::clock::now(), deleting));
    }

    /// Ctrl+Z / Ctrl+Y while typing.
    fn text_undo_step(&mut self, redo: bool) {
        let Mode::Text { text, .. } = &self.mode else { return };
        let now = (text.clone(), self.caret);
        let Some((t, c)) = (if redo { self.text_redo.pop() } else { self.text_undo.pop() }) else { return };
        if redo {
            self.text_undo.push(now);
        } else {
            self.text_redo.push(now);
        }
        self.text_last = None;
        self.set_text_raw(&t);
        self.caret = c.min(t.chars().count());
        self.anchor = self.caret;
        self.caret_epoch = crate::clock::now();
    }

    /// Replaces characters `from..to` of the text being edited with `s` and
    /// puts the caret after it.
    fn replace_range(&mut self, from: usize, to: usize, s: &str) {
        self.text_mark(s.is_empty(), s);
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
        let Mode::Text { text, .. } = &self.mode else { return };
        if text == s {
            return;
        }
        let deleting = s.chars().count() < text.chars().count();
        self.text_mark(deleting, if s.ends_with(char::is_whitespace) { " " } else { "" });
        self.set_text_raw(s);
    }

    fn set_text_raw(&mut self, s: &str) {
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
        self.caret_epoch = crate::clock::now();
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
                    // Typing widens the concept a little, then wraps and
                    // grows it downward.
                    let want_w = (tw + 2.0 * pad).min(n.font * 10.0).max(bw);
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
        let made = self.text_box.take();
        self.text_undo.clear();
        self.text_redo.clear();
        self.text_last = None;
        if let TextTarget::Node(id) = &target {
            if created && text.trim().is_empty() {
                self.board.delete(&[id.clone()], false);
                self.sel.clear();
            } else if created {
                // The concept as made, then its text: Ctrl+Z takes back the
                // text first, the concept with the next one.
                if let Some(done) = self.board.get(id).cloned() {
                    if let Some(n) = self.board.get_mut(id) {
                        n.text.clear();
                        if let Some((w, h)) = made {
                            (n.w, n.h) = (w, h);
                        }
                    }
                    self.commit();
                    self.begin();
                    *self.board.get_mut(id).unwrap() = done;
                }
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
            NODE_PATH => self.path_enter(),
            NODE_CLOSE => {
                if let [one] = &sel[..] {
                    self.path_close(one);
                }
            }
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
        if self.path_active() {
            let one = one.clone();
            if let Some(id) = self.walk_new(&one, None) {
                self.edit_new(id.clone());
                self.rebuild();
                self.show(&id);
            }
            return;
        }
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
        self.add_link(&n.id, &id);
        self.sel = vec![id.clone()];
        self.sel_edge = None;
        self.text_box = self.board.get(&id).map(|n| (n.w, n.h));
        self.mode = Mode::Text { target: TextTarget::Node(id.clone()), text: String::new(), created: true };
        (self.caret, self.anchor) = (0, 0);
        self.caret_epoch = crate::clock::now();
        self.rebuild();
        self.show(&id);
    }

    /// Pans just enough to bring concept `id` into view.
    fn show(&mut self, id: &str) {
        let Some(b) = self.shown().get(id).map(|n| n.bbox()) else { return };
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
        self.caret_epoch = crate::clock::now();
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
                    self.next_arrows = arrow_kind(e.arrow_from, e.arrow_to);
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

/// The kind of a connection for the next one: a line, an arrow (always
/// toward where it is dragged) or both ways.
fn arrow_kind(from: bool, to: bool) -> (bool, bool) {
    if from != to {
        (false, true)
    } else {
        (from, to)
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
