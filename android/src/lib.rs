//! JNI bridge for the Android shell. All calls come from the UI thread;
//! the shared editor (`hs_core::editor`) and finger gestures
//! (`hs_core::touch`) live behind an opaque `long` handle, so the phone
//! edits exactly like Windows: select, move, nest, resize, connect, undo.
//!
//! The shell forwards raw pointer events and frame ticks, and after each
//! call asks `poll()` what to show: a hidden text field while a concept's
//! text is edited (the engine draws the text and caret), a small bar for
//! the selection or a connection, whether to save.
#![cfg(target_os = "android")]

use hs_core::board::{Board, Kind, Shape as NShape};
use hs_core::editor::{Editor, Mode, Pos, Request, PALETTE};
use hs_core::scene::Scene;
use hs_core::touch::Touch;
use hs_core::couch::{Config, Couch};
use hs_core::library::{Library, Live};
use hs_core::{wgpu, Engine};
use std::sync::{Arc, Mutex};
use jni::objects::{JClass, JObject, JString};
use jni::sys::{jboolean, jfloat, jint, jlong, jstring};
use jni::JNIEnv;
use ndk::native_window::NativeWindow;
use raw_window_handle::{AndroidDisplayHandle, AndroidNdkWindowHandle, RawDisplayHandle, RawWindowHandle};

struct App {
    instance: wgpu::Instance,
    ed: Editor,
    fingers: Touch,
    /// Keeps the ANativeWindow referenced while the surface uses it.
    _window: NativeWindow,
    /// A `.hyperschema` board (editable) rather than an imported drawing.
    editable: bool,
    /// Steps committed since the last poll (the shell saves).
    dirty: bool,
    held_seen: bool,
    menu: Option<(String, f64, f64)>,
    /// Boards of this phone (`filesDir/boards`) and the open one.
    lib: Option<Library>,
    live: Option<Live>,
    /// Boards the server has, fetched in the background.
    cards: Arc<Mutex<Vec<(String, String)>>>,
    /// Other devices' changes wait for the current gesture to end.
    remote_waiting: bool,
    /// What went wrong talking to the server, if anything.
    offline: Option<String>,
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
    /// After an editor call: note committed steps, take requests.
    fn settle(&mut self) {
        if !self.ed.done.is_empty() {
            let steps = std::mem::take(&mut self.ed.done);
            if let Some(l) = &mut self.live {
                l.record(&steps);
            }
            self.dirty = true;
        }
        if let Some(Request::EdgeMenu { edge, x, y }) = self.ed.request.take() {
            self.menu = Some((edge, x, y));
        }
    }

    /// Paste and the like land in the middle of the screen.
    fn aim_center(&mut self) {
        let (w, hh) = self.ed.engine.size();
        self.ed.cursor = Pos { x: w as f64 * 0.5, y: hh as f64 * 0.45 };
    }
}

/// The selection bar's choices: `NODE_*` in `hs_core::editor` (colour,
/// shape, duplicate, wrap, delete).
#[no_mangle]
pub extern "system" fn Java_com_hyperschema_app_Native_nodeChoice(_env: JNIEnv, _c: JClass, h: jlong, choice: jint) {
    let a = app(h);
    if a.editable {
        a.ed.apply_node_choice(choice);
        a.settle();
    }
}

/// The selected concepts as a board (JSON) for the clipboard, or null.
#[no_mangle]
pub extern "system" fn Java_com_hyperschema_app_Native_copySelection(mut env: JNIEnv, _c: JClass, h: jlong) -> jstring {
    let s = app(h).ed.copy_selection().map(|b| b.serialize());
    out(&mut env, s)
}

/// Plain text of the selected concepts, for other apps.
#[no_mangle]
pub extern "system" fn Java_com_hyperschema_app_Native_selectionText(mut env: JNIEnv, _c: JClass, h: jlong) -> jstring {
    let s = app(h).ed.selection_text();
    out(&mut env, s)
}

/// Pastes copied concepts (board JSON) or, failing that, plain text as a
/// new concept, in the middle of the screen.
#[no_mangle]
pub extern "system" fn Java_com_hyperschema_app_Native_paste(mut env: JNIEnv, _c: JClass, h: jlong, board: JString, text: JString) {
    let (board, text) = (jstr(&mut env, &board), jstr(&mut env, &text));
    let a = app(h);
    if !a.editable {
        return;
    }
    a.aim_center();
    match Board::parse(&board) {
        Ok(b) if !board.is_empty() => a.ed.paste_board(b),
        _ if !text.trim().is_empty() => a.ed.paste_concept(text.trim()),
        _ => {}
    }
    a.settle();
}

/// Search: sets the query (or steps with `step` = 1 next, -1 previous) and
/// returns `<current>\t<count>`; the camera flies to the match.
#[no_mangle]
pub extern "system" fn Java_com_hyperschema_app_Native_search(mut env: JNIEnv, _c: JClass, h: jlong, query: JString, step: jint) -> jstring {
    let q = jstr(&mut env, &query);
    let a = app(h);
    a.ed.search_open();
    match step {
        0 => a.ed.search_set(&q),
        s => a.ed.search_step(s < 0),
    }
    a.menu = None;
    a.settle();
    let sr = a.ed.search.as_ref().unwrap();
    out(&mut env, Some(format!("{}\t{}", sr.at, sr.hits.len())))
}

#[no_mangle]
pub extern "system" fn Java_com_hyperschema_app_Native_searchClose(_env: JNIEnv, _c: JClass, h: jlong) {
    app(h).ed.search_close();
}

#[no_mangle]
pub extern "system" fn Java_com_hyperschema_app_Native_create(env: JNIEnv, _c: JClass, surface: JObject, w: jint, h: jint, density: jfloat) -> jlong {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let Some((window, sf)) = (unsafe { make_surface(&instance, &env, &surface) }) else { return 0 };
    let font = FONTS.iter().find_map(|p| std::fs::read(p).ok()).expect("no system font");
    let mut engine = pollster::block_on(Engine::new(&instance, sf, w as u32, h as u32, font, true));
    engine.set_scene(Scene::default());
    let density = density.max(1.0);
    let a = App {
        instance,
        ed: Editor::new(engine, density, true),
        fingers: Touch::new(density as f64),
        _window: window,
        editable: false,
        dirty: false,
        held_seen: false,
        menu: None,
        lib: None,
        live: None,
        cards: Default::default(),
        remote_waiting: false,
        offline: None,
    };
    Box::into_raw(Box::new(a)) as jlong
}

#[no_mangle]
pub extern "system" fn Java_com_hyperschema_app_Native_attach(env: JNIEnv, _c: JClass, h: jlong, surface: JObject, w: jint, hgt: jint) {
    let a = app(h);
    if let Some((window, sf)) = unsafe { make_surface(&a.instance, &env, &surface) } {
        a.ed.engine.attach_surface(sf, w as u32, hgt as u32);
        a._window = window;
    }
}

#[no_mangle]
pub extern "system" fn Java_com_hyperschema_app_Native_detach(_env: JNIEnv, _c: JClass, h: jlong) {
    app(h).ed.engine.detach_surface();
}

#[no_mangle]
pub extern "system" fn Java_com_hyperschema_app_Native_resize(_env: JNIEnv, _c: JClass, h: jlong, w: jint, hgt: jint) {
    app(h).ed.engine.resize(w as u32, hgt as u32);
}

#[no_mangle]
pub extern "system" fn Java_com_hyperschema_app_Native_pan(_env: JNIEnv, _c: JClass, h: jlong, dx: jfloat, dy: jfloat) {
    app(h).ed.engine.pan(dx as f64, dy as f64);
}

#[no_mangle]
pub extern "system" fn Java_com_hyperschema_app_Native_editSelected(_env: JNIEnv, _c: JClass, h: jlong) {
    let a = app(h);
    if a.editable {
        a.ed.edit_selected();
    }
}

/// Mouse wheel (emulator, a mouse on the phone): zoom at the pointer.
#[no_mangle]
pub extern "system" fn Java_com_hyperschema_app_Native_zoom(_env: JNIEnv, _c: JClass, h: jlong, f: jfloat, x: jfloat, y: jfloat) {
    app(h).ed.engine.zoom_at(f as f64, x as f64, y as f64);
}

#[no_mangle]
pub extern "system" fn Java_com_hyperschema_app_Native_fit(_env: JNIEnv, _c: JClass, h: jlong) {
    app(h).ed.engine.fit();
}

/// Show connections between a concept and what is nested in it.
#[no_mangle]
pub extern "system" fn Java_com_hyperschema_app_Native_setNested(_env: JNIEnv, _c: JClass, h: jlong, on: jboolean) {
    let a = app(h);
    a.ed.show_nested_links = on != 0;
    a.ed.rebuild();
}

#[no_mangle]
pub extern "system" fn Java_com_hyperschema_app_Native_render(_env: JNIEnv, _c: JClass, h: jlong) -> jboolean {
    let a = app(h);
    a.ed.refresh_overlay();
    let top = a.ed.ui_top;
    let ui = a.ed.path_ui(top);
    a.ed.engine.set_ui(&ui);
    a.ed.engine.render() as jboolean
}

/// Pointer event: action 0 down, 1 move, 2 up, 3 cancel; `t` = event time, ms.
#[no_mangle]
pub extern "system" fn Java_com_hyperschema_app_Native_touch(_env: JNIEnv, _c: JClass, h: jlong, action: jint, id: jlong, x: jfloat, y: jfloat, t: jlong) {
    let a = app(h);
    let (x, y, t, id) = (x as f64, y as f64, t as u64, id as u64);
    if !a.editable {
        a.ed.frozen = true;
    }
    match action {
        0 => {
            a.menu = None;
            a.fingers.down(&mut a.ed, id, x, y, t)
        }
        1 => a.fingers.moved(&mut a.ed, id, x, y, t),
        2 => a.fingers.up(&mut a.ed, id, x, y, t),
        _ => a.fingers.cancel(&mut a.ed),
    }
    a.settle();
}

/// Frame tick: long press, glide, animated zoom. True while it needs frames.
#[no_mangle]
pub extern "system" fn Java_com_hyperschema_app_Native_tick(_env: JNIEnv, _c: JClass, h: jlong, t: jlong) -> jboolean {
    let a = app(h);
    let busy = a.fingers.tick(&mut a.ed, t as u64);
    a.settle();
    (busy || a.ed.animating()) as jboolean
}

/// What the shell should show now, one fact per line:
/// `dirty` (save), `held` (long press fired: haptic), `text\t<text>` (a
/// concept's text is being edited), `menu\t<x>\t<y>\t<from>\t<to>` (a
/// connection's choices), `sel\t<x0>\t<y0>\t<x1>\t<y1>` (screen box of the
/// selection, for its bar).
#[no_mangle]
pub extern "system" fn Java_com_hyperschema_app_Native_poll(mut env: JNIEnv, _c: JClass, h: jlong) -> jstring {
    let a = app(h);
    let mut lines = Vec::new();
    if std::mem::take(&mut a.dirty) {
        lines.push("dirty".to_string());
    }
    let held = a.fingers.held();
    if held && !a.held_seen {
        lines.push("held".to_string());
    }
    a.held_seen = held;
    if let Mode::Text { text, .. } = &a.ed.mode {
        lines.push(format!("text\t{}", text.replace('\n', "\u{1}")));
    }
    if let Some((eid, x, y)) = &a.menu {
        if let Some(e) = a.ed.board.edge(eid) {
            lines.push(format!("menu\t{x:.0}\t{y:.0}\t{}\t{}", e.arrow_from as u8, e.arrow_to as u8));
        }
    }
    if matches!(a.ed.mode, Mode::Idle | Mode::Text { .. }) && !a.ed.sel.is_empty() {
        if let Some(b) = a.ed.shown().bounds_of(a.ed.sel.iter()) {
            let e = &a.ed.engine;
            let (s, (w, hh)) = (e.cam.s, e.size());
            let sx = |x: f32| (x as f64 - e.cam.cx) * s + w as f64 * 0.5;
            let sy = |y: f32| (y as f64 - e.cam.cy) * s + hh as f64 * 0.5;
            lines.push(format!("sel\t{:.0}\t{:.0}\t{:.0}\t{:.0}", sx(b[0]), sy(b[1]), sx(b[2]), sy(b[3])));
            // The first selected concept's fill and shape, and whether any
            // holds others (no bare-text shape for those).
            if let Some(n) = a.ed.sel.first().and_then(|id| a.ed.board.get(id)) {
                let fill = PALETTE.iter().position(|p| p.1 == n.fill.as_deref()).map_or(-1, |k| k as i32);
                let shape = [NShape::Box, NShape::Ellipse, NShape::Diamond, NShape::Text].iter().position(|s| *s == n.shape).unwrap_or(0);
                let parent = a.ed.sel.iter().any(|id| a.ed.board.has_children(id));
                lines.push(format!("style\t{fill}\t{shape}\t{}\t{}", parent as u8, a.ed.sel.len()));
            }
        }
    }
    out(&mut env, Some(lines.join("\n")))
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
            a.ed.load(b);
            a.ed.frozen = !ours;
            a.editable = ours;
            a.menu = None;
            a.ed.engine.fit();
            std::ptr::null_mut()
        }
        Err(e) => out(&mut env, Some(e)),
    }
}

/// The file changed elsewhere (sync) while we were away: take the new
/// version, keep where the camera looks.
#[no_mangle]
pub extern "system" fn Java_com_hyperschema_app_Native_reload(mut env: JNIEnv, _c: JClass, h: jlong, text: JString) -> jboolean {
    let text = jstr(&mut env, &text);
    let a = app(h);
    if !a.editable {
        return 0;
    }
    match Board::parse(&text) {
        Ok(b) => {
            let cam = a.ed.engine.cam;
            a.ed.load(b);
            a.ed.engine.cam = cam;
            a.menu = None;
            1
        }
        Err(_) => 0,
    }
}

#[no_mangle]
pub extern "system" fn Java_com_hyperschema_app_Native_editable(_env: JNIEnv, _c: JClass, h: jlong) -> jboolean {
    app(h).editable as jboolean
}

#[no_mangle]
pub extern "system" fn Java_com_hyperschema_app_Native_serialize(mut env: JNIEnv, _c: JClass, h: jlong) -> jstring {
    let s = app(h).ed.board.serialize();
    out(&mut env, Some(s))
}

/// Recent walks of the open board, for the shell to keep (per device).
#[no_mangle]
pub extern "system" fn Java_com_hyperschema_app_Native_walks(mut env: JNIEnv, _c: JClass, h: jlong) -> jstring {
    let s = app(h).ed.walks_json();
    out(&mut env, Some(s))
}

/// Walks kept by `walks`; the one going on when the app closed goes on.
#[no_mangle]
pub extern "system" fn Java_com_hyperschema_app_Native_setWalks(mut env: JNIEnv, _c: JClass, h: jlong, s: JString) {
    let s = jstr(&mut env, &s);
    let a = app(h);
    a.ed.set_walks_json(&s);
    a.settle();
}

/// Layout of walks (kept between runs by the shell).
#[no_mangle]
pub extern "system" fn Java_com_hyperschema_app_Native_layout(mut env: JNIEnv, _c: JClass, h: jlong) -> jstring {
    let s = app(h).ed.layout.key().to_string();
    out(&mut env, Some(s))
}

#[no_mangle]
pub extern "system" fn Java_com_hyperschema_app_Native_setLayout(mut env: JNIEnv, _c: JClass, h: jlong, s: JString) {
    let s = jstr(&mut env, &s);
    if let Some(l) = hs_core::trail::Layout::from_key(&s) {
        app(h).ed.layout = l;
    }
}

/// The edited text changed in the shell's text field.
#[no_mangle]
pub extern "system" fn Java_com_hyperschema_app_Native_setText(mut env: JNIEnv, _c: JClass, h: jlong, text: JString) {
    let text = jstr(&mut env, &text);
    app(h).ed.set_text(&text);
}

#[no_mangle]
pub extern "system" fn Java_com_hyperschema_app_Native_commitText(_env: JNIEnv, _c: JClass, h: jlong) {
    let a = app(h);
    a.ed.commit_text();
    a.settle();
}

#[no_mangle]
pub extern "system" fn Java_com_hyperschema_app_Native_undo(_env: JNIEnv, _c: JClass, h: jlong, redo: jboolean) {
    let a = app(h);
    if a.editable {
        a.ed.undo(redo != 0);
        a.menu = None;
        a.settle();
    }
}

#[no_mangle]
pub extern "system" fn Java_com_hyperschema_app_Native_canUndo(_env: JNIEnv, _c: JClass, h: jlong, redo: jboolean) -> jboolean {
    let a = app(h);
    let walk = a.ed.trail.as_ref().is_some_and(|t| !t.leaving && t.links.len() > 1);
    (a.editable && if redo != 0 { !a.ed.redo_stack.is_empty() } else { !a.ed.undo_stack.is_empty() || a.ed.editing_text() || walk }) as jboolean
}

/// Screen y (px) under the shell's top buttons, where the path bar goes.
#[no_mangle]
pub extern "system" fn Java_com_hyperschema_app_Native_setTop(_env: JNIEnv, _c: JClass, h: jlong, top: jfloat) {
    app(h).ed.ui_top = top;
}

/// System back: leaves the walk if one is on (true), else does nothing.
#[no_mangle]
pub extern "system" fn Java_com_hyperschema_app_Native_pathExit(_env: JNIEnv, _c: JClass, h: jlong) -> jboolean {
    let a = app(h);
    if !a.ed.path_active() {
        return 0;
    }
    a.ed.path_exit();
    a.settle();
    1
}

#[no_mangle]
pub extern "system" fn Java_com_hyperschema_app_Native_deleteSelection(_env: JNIEnv, _c: JClass, h: jlong) {
    let a = app(h);
    a.ed.delete_selection(false);
    a.settle();
}

/// A connection's bar: 1 none, 2 →, 3 ←, 4 ↔, 5 label, 6 delete.
#[no_mangle]
pub extern "system" fn Java_com_hyperschema_app_Native_edgeChoice(_env: JNIEnv, _c: JClass, h: jlong, choice: jint) {
    let a = app(h);
    if let Some((eid, _, _)) = a.menu.take() {
        a.ed.apply_edge_choice(&eid, choice);
    }
    a.settle();
}

/// The + button: a new concept in the middle of the screen, typing at once.
#[no_mangle]
pub extern "system" fn Java_com_hyperschema_app_Native_createCenter(_env: JNIEnv, _c: JClass, h: jlong) {
    let a = app(h);
    if !a.editable {
        return;
    }
    let (w, hh) = a.ed.engine.size();
    a.ed.cursor = Pos { x: w as f64 * 0.5, y: hh as f64 * 0.45 };
    a.ed.press(false, false, true);
    a.ed.release();
    a.settle();
}

#[no_mangle]
pub extern "system" fn Java_com_hyperschema_app_Native_stats(mut env: JNIEnv, _c: JClass, h: jlong) -> jstring {
    let a = app(h);
    let links = a.ed.board.edges.iter().filter(|e| e.kind == Kind::Link).count();
    let msg = format!("{} concepts · {} links", a.ed.board.nodes.len(), links);
    out(&mut env, Some(msg))
}

// ------------------------------------------------------------ boards and sync

fn config(lib: &Library) -> Option<Config> {
    std::fs::read_to_string(lib.dir.join("sync.json")).ok().and_then(|s| Config::parse(&s))
}

impl App {
    fn fetch_cards(&self) {
        let Some(cfg) = self.lib.as_ref().and_then(config) else { return };
        let cards = self.cards.clone();
        std::thread::spawn(move || {
            if let Ok(list) = Couch::new(cfg).boards() {
                *cards.lock().unwrap() = list.into_iter().map(|b| (b.id, b.name)).collect();
            }
        });
    }

    fn open_board(&mut self, id: &str) -> Result<(), String> {
        let lib = self.lib.as_ref().ok_or("no library")?;
        if !lib.exists(id) {
            let name = self.cards.lock().unwrap().iter().find(|c| c.0 == id).map(|c| c.1.clone()).unwrap_or_else(|| id.to_string());
            lib.adopt(id, &name).map_err(|e| e.to_string())?;
        }
        let mut live = lib.load(id)?;
        if let Some(cfg) = config(lib) {
            // The shell polls (`syncPoll`) about once a second.
            live.connect(cfg, || {});
        }
        self.ed.load(live.board());
        self.ed.frozen = false;
        self.editable = true;
        self.menu = None;
        self.remote_waiting = false;
        self.offline = None;
        self.live = Some(live);
        self.ed.engine.fit();
        Ok(())
    }

    /// Shows the document's current board (other devices' changes);
    /// camera, selection, walk and undo stay.
    fn take_remote(&mut self) {
        let Some(l) = &self.live else { return };
        self.remote_waiting = false;
        // A board opened before anything arrived fits once it fills.
        let was_empty = self.ed.board.nodes.is_empty();
        self.ed.board = l.board();
        let b = &self.ed.board;
        self.ed.sel.retain(|id| b.get(id).is_some());
        if self.ed.sel_edge.as_ref().is_some_and(|e| b.edge(e).is_none()) {
            self.ed.sel_edge = None;
        }
        if self.menu.as_ref().is_some_and(|m| b.edge(&m.0).is_none()) {
            self.menu = None;
        }
        self.ed.hover = None;
        self.ed.rebuild();
        if was_empty {
            self.ed.engine.fit();
        }
    }
}

/// Opens the phone's library in `dir` (filesDir/boards).
#[no_mangle]
pub extern "system" fn Java_com_hyperschema_app_Native_setLibrary(mut env: JNIEnv, _c: JClass, h: jlong, dir: JString) {
    let dir = jstr(&mut env, &dir);
    let a = app(h);
    a.lib = Some(Library::open(std::path::PathBuf::from(dir), false));
    a.fetch_cards();
}

/// True when the server connection is set up.
#[no_mangle]
pub extern "system" fn Java_com_hyperschema_app_Native_syncReady(_env: JNIEnv, _c: JClass, h: jlong) -> jboolean {
    app(h).lib.as_ref().and_then(config).is_some() as jboolean
}

/// Takes a connection string (the contents of `sync.json`); false if it is
/// not one. The open board connects right away.
#[no_mangle]
pub extern "system" fn Java_com_hyperschema_app_Native_setSync(mut env: JNIEnv, _c: JClass, h: jlong, text: JString) -> jboolean {
    let text = jstr(&mut env, &text);
    let a = app(h);
    let Some(cfg) = Config::parse(text.trim()) else { return 0 };
    let Some(lib) = &a.lib else { return 0 };
    if std::fs::write(lib.dir.join("sync.json"), text.trim()).is_err() {
        return 0;
    }
    if let Some(l) = &mut a.live {
        if !l.connected() {
            l.connect(cfg, || {});
        }
    }
    a.fetch_cards();
    1
}

/// Boards to pick from: "id\tname\topen" lines, this phone's and the
/// server's, by name.
#[no_mangle]
pub extern "system" fn Java_com_hyperschema_app_Native_boards(mut env: JNIEnv, _c: JClass, h: jlong) -> jstring {
    let a = app(h);
    let mut all = a.lib.as_ref().map(|l| l.list()).unwrap_or_default();
    for (id, name) in a.cards.lock().unwrap().iter() {
        if !all.iter().any(|b| &b.0 == id) {
            all.push((id.clone(), name.clone()));
        }
    }
    all.sort_by(|x, y| x.1.to_lowercase().cmp(&y.1.to_lowercase()));
    let open = a.live.as_ref().map(|l| l.id.clone());
    let lines: Vec<String> = all.iter().map(|(id, name)| format!("{id}\t{name}\t{}", (Some(id) == open.as_ref()) as u8)).collect();
    a.fetch_cards();
    out(&mut env, Some(lines.join("\n")))
}

/// Opens a board by id. Returns its name, or null if it did not open.
#[no_mangle]
pub extern "system" fn Java_com_hyperschema_app_Native_openBoard(mut env: JNIEnv, _c: JClass, h: jlong, id: JString) -> jstring {
    let id = jstr(&mut env, &id);
    let a = app(h);
    if let Some(l) = &mut a.live {
        let _ = l.save();
    }
    match a.open_board(&id) {
        Ok(()) => {
            let name = a.live.as_ref().map(|l| l.name.clone());
            out(&mut env, name)
        }
        Err(_) => std::ptr::null_mut(),
    }
}

/// A new board from a file's text (ours or Excalidraw; empty = blank),
/// opened. Returns its id, or "!error".
#[no_mangle]
pub extern "system" fn Java_com_hyperschema_app_Native_importBoard(mut env: JNIEnv, _c: JClass, h: jlong, name: JString, text: JString) -> jstring {
    let (name, text) = (jstr(&mut env, &name), jstr(&mut env, &text));
    let a = app(h);
    let ours = text.trim_start().starts_with('{') && text.contains("\"hyperschema\"");
    let board = if text.trim().is_empty() { Ok(Board::new()) } else if ours { Board::parse(&text) } else { hs_core::import::from_excalidraw(&text).map(|x| x.0) };
    let r = board.and_then(|b| a.lib.as_ref().ok_or("no library".to_string())?.create(&name, &b).map_err(|e| e.to_string()));
    let r = r.and_then(|id| a.open_board(&id).map(|_| id));
    out(&mut env, Some(r.unwrap_or_else(|e| format!("!{e}"))))
}

/// Writes the open board's document to the phone.
#[no_mangle]
pub extern "system" fn Java_com_hyperschema_app_Native_saveBoard(_env: JNIEnv, _c: JClass, h: jlong) -> jboolean {
    app(h).live.as_mut().map_or(true, |l| l.save().is_ok()) as jboolean
}

/// Takes in what sync brought. Returns 1 if the board changed (redraw),
/// plus 2 while the server is out of reach.
#[no_mangle]
pub extern "system" fn Java_com_hyperschema_app_Native_syncPoll(_env: JNIEnv, _c: JClass, h: jlong) -> jint {
    let a = app(h);
    let Some(l) = &mut a.live else { return 0 };
    let (merged, status) = l.poll();
    if merged {
        let _ = l.save();
        a.remote_waiting = true;
    }
    if let Some(s) = status {
        a.offline = s;
    }
    let mut changed = 0;
    if a.remote_waiting && matches!(a.ed.mode, Mode::Idle) && !a.fingers.held() {
        a.take_remote();
        changed = 1;
    }
    changed | if a.offline.is_some() { 2 } else { 0 }
}
