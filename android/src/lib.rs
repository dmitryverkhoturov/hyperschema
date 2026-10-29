//! JNI bridge for the Android shell. All calls come from the UI thread;
//! the engine and the board live behind an opaque `long` handle.
//!
//! Editing on the phone: drag a concept (it nests where its centre lands,
//! leaving a link behind when it leaves a parent), new concept, edit text,
//! delete. The shell saves `serialize()` back to the file it opened.
#![cfg(target_os = "android")]

use hs_core::board::{Board, Kind, Node, Shape};
use hs_core::route::{self, Pt};
use hs_core::scene::Scene;
use hs_core::{view, wgpu, Engine};
use jni::objects::{JClass, JObject, JString};
use jni::sys::{jboolean, jfloat, jint, jlong, jstring};
use jni::JNIEnv;
use ndk::native_window::NativeWindow;
use raw_window_handle::{AndroidDisplayHandle, AndroidNdkWindowHandle, RawDisplayHandle, RawWindowHandle};
use serde_json::Map;
use std::collections::{HashMap, HashSet};

struct Drag {
    id: String,
    base: Board,
    start: (f64, f64),
}

struct App {
    instance: wgpu::Instance,
    engine: Engine,
    /// Keeps the ANativeWindow referenced while the surface uses it.
    _window: NativeWindow,
    board: Board,
    routes: HashMap<String, Vec<Pt>>,
    drag: Option<Drag>,
    /// A `.hyperschema` board (editable) rather than an imported drawing.
    editable: bool,
    density: f32,
}

const FONTS: &[&str] = &[
    "/system/fonts/Roboto-Regular.ttf",
    "/system/fonts/RobotoStatic-Regular.ttf",
    "/system/fonts/NotoSans-Regular.ttf",
    "/system/fonts/DroidSans.ttf",
];

unsafe fn make_surface(instance: &wgpu::Instance, env: &JNIEnv, surface: &JObject) -> Option<(NativeWindow, wgpu::Surface<'static>)> {
    let window = NativeWindow::from_surface(env.get_raw() as _, surface.as_raw() as _)?;
    let handle = AndroidNdkWindowHandle::new(window.ptr().cast());
    let sf = instance
        .create_surface_unsafe(wgpu::SurfaceTargetUnsafe::RawHandle {
            raw_display_handle: Some(RawDisplayHandle::Android(AndroidDisplayHandle::new())),
            raw_window_handle: RawWindowHandle::AndroidNdk(handle),
        })
        .ok()?;
    Some((window, sf))
}

fn app<'a>(h: jlong) -> &'a mut App {
    unsafe { &mut *(h as *mut App) }
}

fn jstr(env: &mut JNIEnv, s: &JString) -> String {
    env.get_string(s).map(|s| s.into()).unwrap_or_default()
}

fn out(env: &mut JNIEnv, s: Option<String>) -> jstring {
    match s {
        Some(s) => env.new_string(s).map(|s| s.into_raw()).unwrap_or(std::ptr::null_mut()),
        None => std::ptr::null_mut(),
    }
}

impl App {
    fn rebuild(&mut self) {
        self.routes = route::route_all(&self.board);
        let eng = &self.engine;
        let m = |t: &str, s: f32, w: f32| eng.wrapped_lines(t, s, w);
        let none = HashSet::new();
        let skip = view::Skip { nodes: &none, edges: &none };
        let scene = view::build(&self.board, &self.routes, &m, &skip, view::bg_color());
        self.engine.set_scene(scene);
    }

    fn world(&self, x: f32, y: f32) -> (f32, f32) {
        let (wx, wy) = self.engine.screen_to_world(x as f64, y as f64);
        (wx as f32, wy as f32)
    }

    fn title_lines(&self, n: &Node) -> usize {
        view::title_lines(n, &|t, s, w| self.engine.wrapped_lines(t, s, w))
    }

    /// A concept a finger at (x, y) grabs: a leaf anywhere, a parent by its
    /// title strip or border (its empty inside pans like the canvas).
    fn grab(&self, x: f32, y: f32) -> Option<String> {
        let p = self.world(x, y);
        let tol = (12.0 * self.density as f64 / self.engine.cam.s) as f32;
        if let Some(n) = self.board.nodes.iter().rev().find(|n| n.shape == Shape::Text && inside(n, p, tol)) {
            return Some(n.id.clone());
        }
        let id = self.board.deepest_at(p.0, p.1, &HashSet::new())?;
        let n = self.board.node(&id);
        if !self.board.has_children(&id) {
            return Some(id);
        }
        let b = n.bbox();
        let border = p.0 - b[0] < tol || b[2] - p.0 < tol || p.1 - b[1] < tol || b[3] - p.1 < tol;
        let title = p.1 < n.y + self.board.title_h(n, self.title_lines(n));
        (border || title).then_some(id)
    }

    fn fit_and_settle(&mut self, id: &str) {
        let eng = &self.engine;
        let one = move |n: &Node| view::title_lines(n, &|t: &str, s: f32, w: f32| eng.wrapped_lines(t, s, w));
        let mut board = std::mem::take(&mut self.board);
        board.fit_ancestors(id, &one);
        board.resolve_overlap(id);
        board.fit_ancestors(id, &one);
        self.board = board;
    }
}

fn inside(n: &Node, p: (f32, f32), tol: f32) -> bool {
    let b = n.bbox();
    p.0 >= b[0] - tol && p.0 <= b[2] + tol && p.1 >= b[1] - tol && p.1 <= b[3] + tol
}

#[no_mangle]
pub extern "system" fn Java_com_hyperschema_app_Native_create(env: JNIEnv, _c: JClass, surface: JObject, w: jint, h: jint, density: jfloat) -> jlong {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let Some((window, sf)) = (unsafe { make_surface(&instance, &env, &surface) }) else { return 0 };
    let font = FONTS.iter().find_map(|p| std::fs::read(p).ok()).expect("no system font");
    let mut engine = pollster::block_on(Engine::new(&instance, sf, w as u32, h as u32, font, true));
    engine.set_scene(Scene::default());
    let a = App { instance, engine, _window: window, board: Board::new(), routes: HashMap::new(), drag: None, editable: false, density: density.max(1.0) };
    Box::into_raw(Box::new(a)) as jlong
}

#[no_mangle]
pub extern "system" fn Java_com_hyperschema_app_Native_attach(env: JNIEnv, _c: JClass, h: jlong, surface: JObject, w: jint, hgt: jint) {
    let a = app(h);
    if let Some((window, sf)) = unsafe { make_surface(&a.instance, &env, &surface) } {
        a.engine.attach_surface(sf, w as u32, hgt as u32);
        a._window = window;
    }
}

#[no_mangle]
pub extern "system" fn Java_com_hyperschema_app_Native_detach(_env: JNIEnv, _c: JClass, h: jlong) {
    app(h).engine.detach_surface();
}

#[no_mangle]
pub extern "system" fn Java_com_hyperschema_app_Native_resize(_env: JNIEnv, _c: JClass, h: jlong, w: jint, hgt: jint) {
    app(h).engine.resize(w as u32, hgt as u32);
}

#[no_mangle]
pub extern "system" fn Java_com_hyperschema_app_Native_pan(_env: JNIEnv, _c: JClass, h: jlong, dx: jfloat, dy: jfloat) {
    app(h).engine.pan(dx as f64, dy as f64);
}

#[no_mangle]
pub extern "system" fn Java_com_hyperschema_app_Native_zoom(_env: JNIEnv, _c: JClass, h: jlong, f: jfloat, x: jfloat, y: jfloat) {
    app(h).engine.zoom_at(f as f64, x as f64, y as f64);
}

#[no_mangle]
pub extern "system" fn Java_com_hyperschema_app_Native_fit(_env: JNIEnv, _c: JClass, h: jlong) {
    app(h).engine.fit();
}

#[no_mangle]
pub extern "system" fn Java_com_hyperschema_app_Native_render(_env: JNIEnv, _c: JClass, h: jlong) -> jboolean {
    app(h).engine.render() as jboolean
}

/// Loads a `.hyperschema` board, or imports an Excalidraw drawing (then
/// read-only). Returns an error message or null.
#[no_mangle]
pub extern "system" fn Java_com_hyperschema_app_Native_load(mut env: JNIEnv, _c: JClass, h: jlong, text: JString) -> jstring {
    let text = jstr(&mut env, &text);
    let a = app(h);
    let ours = text.trim_start().starts_with('{') && text.contains("\"hyperschema\"");
    let result = if ours { Board::parse(&text) } else { hs_core::import::from_excalidraw(&text).map(|x| x.0) };
    match result {
        Ok(b) => {
            a.board = b;
            a.editable = ours;
            a.drag = None;
            a.rebuild();
            a.engine.fit();
            std::ptr::null_mut()
        }
        Err(e) => out(&mut env, Some(e)),
    }
}

#[no_mangle]
pub extern "system" fn Java_com_hyperschema_app_Native_editable(_env: JNIEnv, _c: JClass, h: jlong) -> jboolean {
    app(h).editable as jboolean
}

#[no_mangle]
pub extern "system" fn Java_com_hyperschema_app_Native_serialize(mut env: JNIEnv, _c: JClass, h: jlong) -> jstring {
    let s = app(h).board.serialize();
    out(&mut env, Some(s))
}

/// Finger down: true if it grabbed a concept to move.
#[no_mangle]
pub extern "system" fn Java_com_hyperschema_app_Native_dragStart(_env: JNIEnv, _c: JClass, h: jlong, x: jfloat, y: jfloat) -> jboolean {
    let a = app(h);
    if !a.editable {
        return 0;
    }
    match a.grab(x, y) {
        Some(id) => {
            let start = a.engine.screen_to_world(x as f64, y as f64);
            a.drag = Some(Drag { id, base: a.board.clone(), start });
            1
        }
        None => 0,
    }
}

#[no_mangle]
pub extern "system" fn Java_com_hyperschema_app_Native_dragMove(_env: JNIEnv, _c: JClass, h: jlong, x: jfloat, y: jfloat) {
    let a = app(h);
    let Some(d) = &a.drag else { return };
    let w = a.engine.screen_to_world(x as f64, y as f64);
    let mut b = d.base.clone();
    b.move_tree(&d.id, (w.0 - d.start.0) as f32, (w.1 - d.start.1) as f32);
    a.board = b;
    a.rebuild();
}

/// Finger up after a drag: nest where the centre landed. True if anything moved.
#[no_mangle]
pub extern "system" fn Java_com_hyperschema_app_Native_dragEnd(_env: JNIEnv, _c: JClass, h: jlong) -> jboolean {
    let a = app(h);
    let Some(d) = a.drag.take() else { return 0 };
    let moved = a.board.node(&d.id).bbox() != d.base.node(&d.id).bbox();
    if !moved {
        a.board = d.base;
        return 0;
    }
    let c = a.board.node(&d.id).center();
    let skip: HashSet<String> = std::iter::once(d.id.clone()).collect();
    let target = a.board.deepest_at(c.0, c.1, &skip);
    a.board.set_parent(&d.id, target.as_deref());
    a.fit_and_settle(&d.id);
    a.rebuild();
    1
}

/// The concept under (x, y) for editing its text: "id\ntext", or null.
#[no_mangle]
pub extern "system" fn Java_com_hyperschema_app_Native_nodeAt(mut env: JNIEnv, _c: JClass, h: jlong, x: jfloat, y: jfloat) -> jstring {
    let a = app(h);
    let p = a.world(x, y);
    let id = a.grab(x, y).or_else(|| a.board.deepest_at(p.0, p.1, &HashSet::new()));
    let s = id.map(|id| format!("{id}\n{}", a.board.node(&id).text));
    out(&mut env, s)
}

#[no_mangle]
pub extern "system" fn Java_com_hyperschema_app_Native_setText(mut env: JNIEnv, _c: JClass, h: jlong, id: JString, text: JString) {
    let (id, text) = (jstr(&mut env, &id), jstr(&mut env, &text));
    let a = app(h);
    let Some(n) = a.board.get(&id).cloned() else { return };
    let mut nn = n.clone();
    nn.text = text.clone();
    if !a.board.has_children(&id) && n.shape != Shape::Text {
        let pad = n.pad();
        let (tw, _) = a.engine.measure(&text, n.font);
        nn.w = (tw.min(n.font * 14.0) + 2.0 * pad).max(n.w);
        let lines = a.engine.wrapped_lines(&text, n.font, nn.w - 2.0 * pad) as f32;
        nn.h = n.h.max(lines * hs_core::text::LINE_H * n.font + 2.0 * pad);
    }
    *a.board.get_mut(&id).unwrap() = nn;
    a.fit_and_settle(&id);
    a.rebuild();
}

/// A new concept centred at (x, y) with `text`, nested where it lands.
#[no_mangle]
pub extern "system" fn Java_com_hyperschema_app_Native_createAt(mut env: JNIEnv, _c: JClass, h: jlong, x: jfloat, y: jfloat, text: JString) {
    let text = jstr(&mut env, &text);
    let a = app(h);
    let p = a.world(x, y);
    let px = (a.density as f64 / a.engine.cam.s) as f32;
    let (mut w, mut hh, mut font) = (130.0 * px, 64.0 * px, 15.0 * px);
    let parent = a.board.deepest_at(p.0, p.1, &HashSet::new());
    if let Some(pn) = parent.as_ref().and_then(|q| a.board.get(q)).cloned() {
        let area = a.board.content_area(&pn, a.title_lines(&pn));
        let k = ((area[2] - area[0]) * 0.45 / w).min(1.0).max(0.05);
        w *= k;
        hh *= k;
        font *= k;
    }
    let id = a.board.new_id();
    a.board.add_node(Node { id: id.clone(), x: p.0 - w * 0.5, y: p.1 - hh * 0.5, w, h: hh, text: String::new(), font, shape: Shape::Box, color: None, fill: None, extra: Map::new() });
    if let Some(q) = &parent {
        a.board.set_parent(&id, Some(q));
    }
    a.fit_and_settle(&id);
    let idj = env.new_string(&id).unwrap();
    let tj = env.new_string(&text).unwrap();
    Java_com_hyperschema_app_Native_setText(env, _c, h, idj, tj);
}

/// Deletes a concept; its children move up a level.
#[no_mangle]
pub extern "system" fn Java_com_hyperschema_app_Native_delete(mut env: JNIEnv, _c: JClass, h: jlong, id: JString) {
    let id = jstr(&mut env, &id);
    let a = app(h);
    a.board.delete(&[id], false);
    a.rebuild();
}

#[no_mangle]
pub extern "system" fn Java_com_hyperschema_app_Native_stats(mut env: JNIEnv, _c: JClass, h: jlong) -> jstring {
    let a = app(h);
    let links = a.board.edges.iter().filter(|e| e.kind == Kind::Link).count();
    let msg = format!("{} концептов · {} связей", a.board.nodes.len(), links);
    out(&mut env, Some(msg))
}
