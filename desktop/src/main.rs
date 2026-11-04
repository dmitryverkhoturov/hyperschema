//! Windows shell: one window with our own title strip (chrome.rs), opens
//! the last board, touchpad through DirectManipulation, frames paced by
//! the shell, editing in edit.rs, files and history in store.rs.
//!
//! Usage: hyperschema [file.hyperschema | file.excalidraw(.md)]
//! Dev:   --bench N | --autobench | --vsync | --gpu low | --shot out.png [--cam x,y,scale]
//!        --script steps.txt (see script.rs)

#![windows_subsystem = "windows"]

mod chrome;
mod cli;
mod clip;
mod dm;
mod edit;
mod script;
mod store;

use hs_core::board::Change;
use hs_core::editor::{Editor, Pos};
use hs_core::{bench, wgpu, Engine};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use hs_core::editor::Mode;
use hs_core::touch::Touch;
use winit::window::{Window, WindowId};

const DEFAULT_SCHEME: &str = "board.excalidraw.md";

struct Args {
    bench: Option<usize>,
    file: Option<PathBuf>,
    autobench: bool,
    vsync: bool,
    high_perf: bool,
    shot: Option<String>,
    script: Option<String>,
    cam: Option<(f64, f64, f64)>,
}

fn parse_args() -> Args {
    // The discrete GPU by default: the 2016 Intel OpenGL driver on this
    // laptop crashes (access violation in ig75icd64.dll).
    let mut a = Args { bench: None, file: None, autobench: false, vsync: false, high_perf: true, shot: None, script: None, cam: None };
    let mut it = std::env::args().skip(1);
    while let Some(s) = it.next() {
        match s.as_str() {
            "--bench" => a.bench = Some(it.next().and_then(|v| v.parse().ok()).unwrap_or(500_000)),
            "--autobench" => a.autobench = true,
            "--vsync" => a.vsync = true,
            "--gpu" => a.high_perf = it.next().as_deref() != Some("low"),
            "--shot" => a.shot = it.next(),
            "--script" => a.script = it.next(),
            "--cam" => {
                let v: Vec<f64> = it.next().unwrap_or_default().split(',').filter_map(|x| x.parse().ok()).collect();
                if v.len() == 3 {
                    a.cam = Some((v[0], v[1], v[2]));
                }
            }
            _ => a.file = Some(PathBuf::from(s)),
        }
    }
    if a.autobench && a.bench.is_none() && a.file.is_none() {
        a.bench = Some(500_000);
    }
    a
}

pub fn data_dir() -> PathBuf {
    let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    base.join("Hyperschema")
}

fn last_file() -> Option<PathBuf> {
    let p = std::fs::read_to_string(data_dir().join("last.txt")).ok()?;
    let p = PathBuf::from(p.trim());
    p.exists().then_some(p)
}

pub fn remember(path: &Path) {
    let _ = std::fs::create_dir_all(data_dir());
    let _ = std::fs::write(data_dir().join("last.txt"), path.to_string_lossy().as_bytes());
}

/// Window position and outer size (physical px, not maximised) and maximised flag.
fn load_placement() -> Option<(i32, i32, u32, u32, bool)> {
    let t = std::fs::read_to_string(data_dir().join("window.txt")).ok()?;
    let v: Vec<i64> = t.split_whitespace().filter_map(|x| x.parse().ok()).collect();
    (v.len() == 5 && v[2] > 200 && v[3] > 150).then(|| (v[0] as i32, v[1] as i32, v[2] as u32, v[3] as u32, v[4] != 0))
}

fn save_placement(st: &State) {
    let Some((x, y, w, h)) = st.restore else { return };
    let _ = std::fs::create_dir_all(data_dir());
    let _ = std::fs::write(data_dir().join("window.txt"), format!("{x} {y} {w} {h} {}", chrome::maximized() as u8));
}

/// Test harness run (HS_SHOT=prefix): F9 writes prefix-N.png and
/// prefix-N.txt (state and tree); the remembered file and window stay put.
pub fn harness() -> Option<String> {
    std::env::var("HS_SHOT").ok()
}

static DEV_RUN: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

/// A run for testing, a screenshot or a benchmark: boards it opens go to a
/// throwaway library, nothing of the user's is touched.
pub fn dev_run() -> bool {
    (*DEV_RUN.get().unwrap_or(&false) || harness().is_some()) && std::env::var_os("HS_SYNC").is_none()
}

/// Debug trace into %APPDATA%\Hyperschema\debug.log when HS_DEBUG is set.
pub fn dlog(msg: String) {
    if std::env::var_os("HS_DEBUG").is_some() {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(data_dir().join("debug.log")) {
            let _ = writeln!(f, "{msg}");
        }
    }
}

pub fn write_png(engine: &mut Engine, path: &str) {
    let (w, h, rgba) = engine.capture();
    let file = std::fs::File::create(path).expect("create png");
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w, h);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().and_then(|mut wr| wr.write_image_data(&rgba)).expect("write png");
}

/// A mouse message Windows made up from a touch (the touch itself arrives
/// as Touch events).
fn from_touch() -> bool {
    use windows::Win32::UI::WindowsAndMessaging::GetMessageExtraInfo;
    (unsafe { GetMessageExtraInfo() }.0 as u32 & 0xFFFF_FF00) == 0xFF51_5700
}

fn log_panics() {
    std::panic::set_hook(Box::new(|info| {
        let _ = std::fs::create_dir_all(data_dir());
        let _ = std::fs::write(data_dir().join("crash.log"), format!("{info}\n{}", std::backtrace::Backtrace::force_capture()));
    }));
}

/// Scripted camera flight: overview -> deep zoom -> pan -> back out.
struct AutoBench {
    start: Instant,
    from: (f64, f64, f64),
    last: Option<Instant>,
    intervals: Vec<f32>,
    cpu: Vec<f32>,
    text: Vec<f32>,
    rows: Vec<String>,
}

const BENCH_SECS: f64 = 12.0;

pub struct State {
    window: Arc<Window>,
    /// Board, selection, gestures and the engine (shared with Android).
    ed: Editor,
    dm: Option<dm::Dm>,
    /// Touch screen gestures (shared with Android).
    fingers: Touch,
    /// Finger that went down on the menu button.
    menu_finger: Option<u64>,
    started: Instant,
    mods: ModifiersState,
    cursor_in: bool,
    touchpad_until: Instant,

    /// Boards of this device (store.rs).
    lib: hs_core::library::Library,
    /// The open board.
    live: Option<hs_core::library::Live>,
    /// Wakes the window when sync has news.
    proxy: winit::event_loop::EventLoopProxy<()>,
    /// What the title strip says about the server: None = all good.
    sync_note: Option<String>,
    /// Changes from another device wait for the current gesture to end.
    remote_waiting: bool,
    /// Boards the server has, fetched in the background.
    cards: Arc<std::sync::Mutex<Vec<(String, String)>>>,
    name: String,
    /// Time (GetMessageTime, ms) and place of the last press, for double clicks.
    last_press: Option<(u32, Pos)>,
    unsaved: Vec<Change>,
    save_at: Option<Instant>,
    timeline: Option<store::Timeline>,

    script: Option<script::Script>,
    /// Answer the next popup menu gets in a scripted run.
    script_choice: Option<i32>,
    auto: Option<AutoBench>,
    frame_interval: Duration,
    last_frame: Instant,
    /// A redraw was requested too early and waits for the next slot.
    pending: bool,
    /// The window is cloaked until its first frame is drawn.
    shown: bool,
    /// Last non-maximised position and size.
    restore: Option<(i32, i32, u32, u32)>,
    /// The board's walks as last written (`walks/…json`).
    walks_saved: String,
    /// Layout of walks as last saved (`path-layout.txt`).
    layout_saved: hs_core::trail::Layout,
    /// Arrowheads of the next connection as last saved (`arrows.txt`).
    arrows_saved: (bool, bool),
}

impl std::ops::Deref for State {
    type Target = Editor;
    fn deref(&self) -> &Editor {
        &self.ed
    }
}

impl std::ops::DerefMut for State {
    fn deref_mut(&mut self) -> &mut Editor {
        &mut self.ed
    }
}

impl State {
    fn move_to(&mut self, position: PhysicalPosition<f64>) {
        let position = Pos { x: position.x, y: position.y };
        let prev = std::mem::replace(&mut self.ed.cursor, position);
        let was_menu = chrome::part_at(prev.x, prev.y) == Some(chrome::Part::Menu);
        let is_menu = chrome::part_at(position.x, position.y) == Some(chrome::Part::Menu);
        self.cursor_in = true;
        // A popup menu can swallow the button's release: a gesture still
        // running with no button down ends here instead of dragging the
        // concept along with the pointer.
        let gesture = !matches!(self.ed.mode, Mode::Idle | Mode::Text { .. });
        if self.script.is_none() && harness().is_none() && !self.ed.touch && (gesture || self.ed.selecting_text()) {
            use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON, VK_MBUTTON, VK_RBUTTON};
            let held = unsafe { [VK_LBUTTON, VK_RBUTTON, VK_MBUTTON].iter().any(|k| GetAsyncKeyState(k.0 as i32) as u16 & 0x8000 != 0) };
            if !held && gesture {
                self.release();
            } else if !held {
                self.ed.end_text_drag();
            }
        }
        if self.timeline.as_ref().is_some_and(|t| t.dragging) {
            self.timeline_drag();
            self.window.request_redraw();
        } else if self.drag_to(prev) {
            self.window.request_redraw();
        } else {
            self.update_hover();
        }
        if was_menu != is_menu {
            self.window.request_redraw();
        }
    }

    fn start_bench(&mut self) {
        self.ed.engine.fit();
        let c = self.ed.engine.cam;
        self.auto = Some(AutoBench { start: Instant::now(), from: (c.cx, c.cy, c.s), last: None, intervals: Vec::new(), cpu: Vec::new(), text: Vec::new(), rows: Vec::new() });
    }

    /// Advances the scripted camera; returns false when done.
    fn step_bench(&mut self) -> bool {
        let Some(b) = &mut self.auto else { return false };
        let t = b.start.elapsed().as_secs_f64();
        let (cx, cy, s0) = b.from;
        let deep = 1.5f64;
        let cam = &mut self.ed.engine.cam;
        if t < 4.0 {
            let k = t / 4.0;
            cam.s = s0 * (deep / s0).powf(k);
            cam.cx = cx;
            cam.cy = cy;
        } else if t < 8.0 {
            let k = (t - 4.0) / 4.0;
            cam.s = deep;
            cam.cx = cx + k * 6000.0;
            cam.cy = cy + (k * 6.28).sin() * 1500.0;
        } else if t < BENCH_SECS {
            let k = (t - 8.0) / 4.0;
            cam.s = deep * (s0 / deep).powf(k);
            cam.cx = cx + 6000.0 * (1.0 - k);
            cam.cy = cy;
        } else {
            return false;
        }
        true
    }

    fn finish_bench(&mut self) {
        let Some(b) = self.auto.take() else { return };
        let mut iv = b.intervals.clone();
        iv.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let n = iv.len().max(1);
        let avg = iv.iter().sum::<f32>() / n as f32;
        let p = |q: f32| iv.get(((n as f32 * q) as usize).min(n - 1)).copied().unwrap_or(0.0);
        let slow = iv.iter().filter(|&&x| x > 20.0).count();
        let cpu_avg = b.cpu.iter().sum::<f32>() / b.cpu.len().max(1) as f32;
        let cpu_max = b.cpu.iter().cloned().fold(0.0, f32::max);
        let text_max = b.text.iter().cloned().fold(0.0, f32::max);
        let report = format!(
            "{} — {} элементов — {}\nкадров {}, средний интервал {:.2} мс ({:.1} fps), p50 {:.2}, p95 {:.2}, p99 {:.2}, max {:.2} мс\nкадров >20 мс: {} ({:.1}%)\nCPU на кадр: среднее {:.2} мс, max {:.2} мс; пересборка текста max {:.2} мс\n",
            self.name,
            self.ed.engine.scene().element_count(),
            self.ed.engine.adapter_info,
            n,
            avg,
            1000.0 / avg,
            p(0.5),
            p(0.95),
            p(0.99),
            p(1.0),
            slow,
            slow as f32 * 100.0 / n as f32,
            cpu_avg,
            cpu_max,
            text_max
        );
        let _ = std::fs::write("bench-result.txt", &report);
        let _ = std::fs::write("bench-frames.csv", format!("t,interval,scale,rects,lines,glyphs,cpu,text\n{}", b.rows.join("\n")));
    }

    fn quit(&mut self, el: &ActiveEventLoop) {
        self.commit_text();
        self.flush();
        self.save_walks();
        if self.script.is_none() && self.auto.is_none() && harness().is_none() {
            save_placement(self);
        }
        el.exit();
    }
}

struct App {
    args: Args,
    state: Option<State>,
    proxy: winit::event_loop::EventLoopProxy<()>,
}

fn window_icon() -> Option<winit::window::Icon> {
    use winit::platform::windows::IconExtWindows;
    winit::window::Icon::from_resource(1, None).ok()
}

impl ApplicationHandler for App {
    /// Sync has news (store.rs `on_sync`).
    fn user_event(&mut self, _el: &ActiveEventLoop, _: ()) {
        if let Some(st) = &mut self.state {
            st.on_sync();
        }
    }

    fn new_events(&mut self, el: &ActiveEventLoop, _cause: winit::event::StartCause) {
        self.wake(el);
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        self.wake(el);
    }

    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.state.is_some() {
            return;
        }
        // Hidden until the first frame: no small window growing, no white
        // or black flashes while the swapchain catches up.
        let mut attrs = Window::default_attributes().with_title("Hyperschema").with_visible(false).with_window_icon(window_icon());
        let place = load_placement().filter(|&(x, y, w, _, _)| {
            el.available_monitors().any(|m| {
                let (p, s) = (m.position(), m.size());
                x + (w as i32) / 2 >= p.x && x + (w as i32) / 2 < p.x + s.width as i32 && y + 20 >= p.y && y + 20 < p.y + s.height as i32
            })
        });
        attrs = attrs.with_inner_size(LogicalSize::new(1280, 800));
        let window = Arc::new(el.create_window(attrs).expect("window"));
        let hwnd = match window.window_handle().map(|h| h.as_raw()) {
            Ok(RawWindowHandle::Win32(h)) => Some(windows::Win32::Foundation::HWND(h.hwnd.get() as *mut _)),
            _ => None,
        };
        if let Some(h) = hwnd {
            chrome::install(h);
            chrome::cloak(true);
            // Outer size: our client area includes the caption, so an inner
            // size would grow by a caption height on every launch.
            if let Some((x, y, w, hh, _)) = place {
                chrome::place(x, y, w, hh);
            }
        }
        let restore = window.outer_position().ok().map(|p| {
            let s = window.outer_size();
            (p.x, p.y, s.width, s.height)
        });
        window.set_visible(true);
        if place.is_some_and(|p| p.4) {
            window.set_maximized(true);
        }
        let size = window.inner_size();
        chrome::set_metrics(window.scale_factor(), size.width);
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let surface = instance.create_surface(window.clone()).expect("surface");
        let font = std::fs::read(r"C:\Windows\Fonts\segoeui.ttf").expect("font");
        let mut engine = pollster::block_on(Engine::new(&instance, surface, size.width, size.height, font, self.args.high_perf));
        // Driver vsync on this class of laptop (GL on Intel, Vulkan through
        // Optimus) paces at ~3 refreshes; we pace frames ourselves instead.
        engine.set_vsync(self.args.vsync);
        let refresh = window.current_monitor().and_then(|m| m.refresh_rate_millihertz()).unwrap_or(60_000);
        let frame_interval = Duration::from_micros(1_000_000_000 / refresh as u64);
        let dm = hwnd.and_then(|h| dm::Dm::new(h).ok());
        let scale = window.scale_factor();
        let mut st = State {
            ed: Editor::new(engine, window.scale_factor() as f32, false),
            window,
            dm,
            fingers: Touch::new(scale),
            menu_finger: None,
            started: Instant::now(),
            mods: ModifiersState::default(),
            cursor_in: false,
            touchpad_until: Instant::now(),
            lib: store::library(dev_run()),
            live: None,
            proxy: self.proxy.clone(),
            sync_note: None,
            remote_waiting: false,
            cards: Default::default(),
            name: String::new(),
            last_press: None,
            unsaved: Vec::new(),
            save_at: None,
            timeline: None,
            script: self.args.script.as_deref().map(script::Script::load),
            script_choice: None,
            auto: None,
            frame_interval,
            last_frame: Instant::now() - Duration::from_secs(1),
            pending: false,
            shown: false,
            restore,
            arrows_saved: (false, true),
            walks_saved: String::new(),
            layout_saved: Default::default(),
        };
        if let Some(l) = std::fs::read_to_string(data_dir().join("path-layout.txt")).ok().and_then(|s| hs_core::trail::Layout::from_key(&s)) {
            st.ed.layout = l;
            st.layout_saved = l;
        }
        st.ed.show_nested_links = std::fs::read_to_string(data_dir().join("show-nested-links.txt")).is_ok_and(|s| s.trim() == "1");
        if let Ok(a) = std::fs::read_to_string(data_dir().join("arrows.txt")) {
            let b: Vec<bool> = a.trim().chars().map(|c| c == '1').collect();
            if let [f, t] = b[..] {
                st.ed.next_arrows = (f, t);
                st.arrows_saved = (f, t);
            }
        }

        if let Some(n) = self.args.bench {
            st.ed.engine.set_scene(bench::generate(n));
            st.ed.engine.fit();
            st.name = format!("тест {n} блоков");
        } else {
            match self.args.file.clone() {
                Some(p) => st.open(&p),
                None => st.open_last(last_file().or_else(|| Some(PathBuf::from(DEFAULT_SCHEME)))),
            }
            st.fetch_cards();
        }
        if let Some((x, y, sc)) = self.args.cam {
            st.ed.engine.cam.cx = x;
            st.ed.engine.cam.cy = y;
            st.ed.engine.cam.s = sc;
        }
        if let Some(path) = &self.args.shot {
            st.refresh_overlay();
            write_png(&mut st.ed.engine, path);
            el.exit();
            return;
        }
        if self.args.autobench {
            st.start_bench();
        }
        st.window.request_redraw();
        self.state = Some(st);
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(st) = &mut self.state else { return };
        match event {
            WindowEvent::CloseRequested => st.quit(el),
            WindowEvent::Resized(sz) => {
                st.ed.engine.resize(sz.width, sz.height);
                chrome::set_metrics(st.window.scale_factor(), sz.width);
                // Full screen (F11) is not the window's own size: keep the last one.
                if !chrome::maximized() && !st.window.is_minimized().unwrap_or(false) && st.window.fullscreen().is_none() {
                    if let Ok(p) = st.window.outer_position() {
                        let o = st.window.outer_size();
                        st.restore = Some((p.x, p.y, o.width, o.height));
                    }
                }
                st.window.request_redraw();
            }
            WindowEvent::Moved(p) => {
                if !chrome::maximized() && !st.window.is_minimized().unwrap_or(false) && st.window.fullscreen().is_none() {
                    let s = st.window.outer_size();
                    st.restore = Some((p.x, p.y, s.width, s.height));
                }
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => chrome::set_metrics(scale_factor, st.window.inner_size().width),
            WindowEvent::DroppedFile(path) => st.open(&path),
            WindowEvent::ModifiersChanged(m) => st.mods = m.state(),
            WindowEvent::CursorMoved { .. } | WindowEvent::MouseInput { .. } if from_touch() => {}
            WindowEvent::Touch(t) => {
                use winit::event::TouchPhase;
                let ms = st.started.elapsed().as_millis() as u64;
                let (x, y) = (t.location.x, t.location.y);
                match t.phase {
                    // A finger on the menu button opens the menu when lifted.
                    TouchPhase::Started if chrome::part_at(x, y) == Some(chrome::Part::Menu) => st.menu_finger = Some(t.id),
                    _ if st.menu_finger == Some(t.id) => {
                        if t.phase == TouchPhase::Ended {
                            st.menu_finger = None;
                            if !chrome::menu_just_closed() {
                                st.commit_text();
                                st.main_menu();
                            }
                        } else if t.phase == TouchPhase::Cancelled {
                            st.menu_finger = None;
                        }
                    }
                    TouchPhase::Started => st.fingers.down(&mut st.ed, t.id, x, y, ms),
                    TouchPhase::Moved => st.fingers.moved(&mut st.ed, t.id, x, y, ms),
                    TouchPhase::Ended => st.fingers.up(&mut st.ed, t.id, x, y, ms),
                    TouchPhase::Cancelled => st.fingers.cancel(&mut st.ed),
                }
                st.settle();
                st.window.request_redraw();
            }
            WindowEvent::CursorMoved { position, .. } => {
                st.ed.touch = false;
                st.move_to(position)
            }
            WindowEvent::CursorLeft { .. } => {
                st.cursor_in = false;
                st.ed.hover = None;
                st.window.request_redraw();
            }
            WindowEvent::MouseInput { state, button, .. } => {
                // Ask Windows directly: modifier events can lag behind the click.
                use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, VK_MENU, VK_SHIFT};
                let down = |k: windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY| unsafe { GetKeyState(k.0 as i32) } < 0;
                let (alt, shift) = (st.mods.alt_key() || down(VK_MENU), st.mods.shift_key() || down(VK_SHIFT));
                // After a popup menu the last mouse move is from before it
                // opened: a press would land where the menu was asked for.
                if state == ElementState::Pressed && st.script.is_none() && harness().is_none() {
                    if let Some((x, y)) = chrome::cursor_client() {
                        st.ed.cursor = Pos { x, y };
                    }
                }
                match (button, state) {
                    (MouseButton::Left, ElementState::Pressed) => {
                        if chrome::part_at(st.ed.cursor.x, st.ed.cursor.y) == Some(chrome::Part::Menu) {
                            if !chrome::menu_just_closed() {
                                st.commit_text();
                                st.main_menu();
                            }
                        } else if chrome::menu_just_closed() {
                            // The click that closed a menu only closes it.
                        } else if !st.timeline_press() {
                            st.press(alt, shift);
                        }
                    }
                    (MouseButton::Right, ElementState::Pressed) => st.press_right(shift),
                    (MouseButton::Middle, ElementState::Pressed) => st.ed.mode = Mode::Pan { moved: true, at: st.ed.cursor },
                    (_, ElementState::Released) => {
                        st.timeline_release();
                        st.release();
                    }
                    _ => {}
                }
                st.window.request_redraw();
            }
            WindowEvent::MouseWheel { delta, .. } => {
                // Touchpads normally arrive through DirectManipulation; this
                // is the mouse wheel and a fallback if DM is unavailable.
                let (x, y) = (st.ed.cursor.x, st.ed.cursor.y);
                match delta {
                    MouseScrollDelta::PixelDelta(p) => {
                        if st.mods.control_key() {
                            st.ed.engine.zoom_at((p.y * 0.01).exp(), x, y);
                        } else {
                            st.ed.engine.pan(p.x, p.y);
                        }
                    }
                    MouseScrollDelta::LineDelta(dx, dy) => {
                        let fractional = dx.fract() != 0.0 || dy.fract() != 0.0 || dx != 0.0;
                        if fractional {
                            st.touchpad_until = Instant::now() + Duration::from_millis(400);
                        }
                        let touchpad = Instant::now() < st.touchpad_until;
                        if touchpad && !st.mods.control_key() {
                            st.ed.engine.pan(dx as f64 * 60.0, dy as f64 * 60.0);
                        } else {
                            st.ed.engine.zoom_at(1.18f64.powf(dy as f64), x, y);
                        }
                    }
                }
                st.window.request_redraw();
            }
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                dlog(format!("key {:?} text={:?}", event.logical_key, event.text));
                let (w, h) = st.ed.engine.size();
                let (cx, cy) = (w as f64 / 2.0, h as f64 / 2.0);
                let ctrl = st.mods.control_key();
                let shift = st.mods.shift_key();
                let key = event.logical_key.as_ref();
                if let (Some(pre), Key::Named(NamedKey::F9)) = (harness(), &key) {
                    st.harness_shot(&pre);
                    return;
                }
                if let (Some(_), Key::Named(NamedKey::F8)) = (harness(), &key) {
                    // World coordinates = window pixels, for writing tests.
                    let (w, h) = st.ed.engine.size();
                    st.ed.engine.cam.cx = w as f64 / 2.0;
                    st.ed.engine.cam.cy = h as f64 / 2.0;
                    st.ed.engine.cam.s = 1.0;
                    st.window.request_redraw();
                    return;
                }
                // Layout-independent letters for shortcuts (Ctrl+Я is Ctrl+Z).
                let letter = match &event.physical_key {
                    winit::keyboard::PhysicalKey::Code(c) => Some(*c),
                    _ => None,
                };
                use winit::keyboard::KeyCode;
                if ctrl && letter == Some(KeyCode::KeyF) {
                    st.ed.search_open();
                    st.settle();
                    st.window.request_redraw();
                    return;
                }
                if let Some(sr) = &st.ed.search {
                    let mut q = sr.query.clone();
                    match key {
                        Key::Named(NamedKey::Escape) => st.ed.search_close(),
                        Key::Named(NamedKey::Enter) => st.ed.search_step(shift),
                        Key::Named(NamedKey::Backspace) => {
                            if ctrl {
                                q.clear();
                            } else {
                                q.pop();
                            }
                            st.ed.search_set(&q);
                        }
                        _ if ctrl && letter == Some(KeyCode::KeyV) => {
                            if let Some(t) = clip::get() {
                                q.push_str(&t.replace('\n', " "));
                                st.ed.search_set(&q);
                            }
                        }
                        _ => {
                            if let Some(t) = event.text.as_ref().filter(|t| !ctrl && !t.chars().any(char::is_control)) {
                                q.push_str(t);
                                st.ed.search_set(&q);
                            }
                        }
                    }
                    st.window.request_redraw();
                    return;
                }
                if st.editing_text() && ctrl && matches!(letter, Some(KeyCode::KeyZ) | Some(KeyCode::KeyY)) {
                    st.undo(letter == Some(KeyCode::KeyY) || shift);
                    st.window.request_redraw();
                    return;
                }
                if st.editing_text() {
                    use hs_core::editor::TextKey;
                    let tk = |st: &mut State, k: TextKey| st.ed.text_key(k, shift, ctrl);
                    match key {
                        Key::Named(NamedKey::Escape) => st.commit_text(),
                        Key::Named(NamedKey::Enter) if ctrl => st.commit_text(),
                        Key::Named(NamedKey::Backspace) => tk(st, TextKey::Backspace),
                        Key::Named(NamedKey::Delete) => tk(st, TextKey::Delete),
                        Key::Named(NamedKey::ArrowLeft) => tk(st, TextKey::Left),
                        Key::Named(NamedKey::ArrowRight) => tk(st, TextKey::Right),
                        Key::Named(NamedKey::ArrowUp) => tk(st, TextKey::Up),
                        Key::Named(NamedKey::ArrowDown) => tk(st, TextKey::Down),
                        Key::Named(NamedKey::Home) => tk(st, TextKey::Home),
                        Key::Named(NamedKey::End) => tk(st, TextKey::End),
                        Key::Named(NamedKey::Enter) => st.type_text("\n"),
                        Key::Named(NamedKey::Tab) => {
                            st.ed.tab_new();
                            st.settle();
                        }
                        _ if ctrl && letter == Some(KeyCode::KeyA) => st.ed.select_all(),
                        _ if ctrl && matches!(letter, Some(KeyCode::KeyC) | Some(KeyCode::KeyX)) => {
                            if let Some(t) = st.ed.selected_text() {
                                clip::set(&t);
                                if letter == Some(KeyCode::KeyX) {
                                    tk(st, TextKey::Delete);
                                }
                            }
                        }
                        _ if ctrl && letter == Some(KeyCode::KeyV) => {
                            if let Some(t) = clip::get() {
                                st.type_text(&t);
                            }
                        }
                        _ => {
                            if let Some(t) = &event.text {
                                if !ctrl {
                                    st.type_text(t);
                                }
                            }
                        }
                    }
                    st.window.request_redraw();
                    return;
                }
                if let Some(t) = &st.timeline {
                    let (pos, n) = (t.pos, t.steps.len());
                    match key {
                        Key::Named(NamedKey::Escape) => st.close_timeline(),
                        Key::Named(NamedKey::Enter) => st.timeline_restore(),
                        Key::Named(NamedKey::ArrowLeft) => st.timeline_seek(pos.saturating_sub(1)),
                        Key::Named(NamedKey::ArrowRight) => st.timeline_seek((pos + 1).min(n)),
                        Key::Named(NamedKey::Home) => st.timeline_seek(0),
                        Key::Named(NamedKey::End) => st.timeline_seek(n),
                        _ => {}
                    }
                    st.window.request_redraw();
                    return;
                }
                if ctrl {
                    match letter {
                        Some(KeyCode::KeyS) => st.flush(),
                        Some(KeyCode::KeyZ) => st.undo(shift),
                        Some(KeyCode::KeyY) => st.undo(true),
                        Some(KeyCode::KeyG) => st.wrap_selection(),
                        Some(KeyCode::KeyO) => st.open_dialog(),
                        Some(KeyCode::KeyN) => st.new_board(),
                        Some(KeyCode::KeyH) => st.open_timeline(),
                        Some(KeyCode::KeyA) => st.ed.select_all_nodes(),
                        Some(KeyCode::KeyC) | Some(KeyCode::KeyX) => st.copy(letter == Some(KeyCode::KeyX)),
                        Some(KeyCode::KeyV) => st.paste(),
                        Some(KeyCode::KeyD) => {
                            st.ed.duplicate();
                            st.settle();
                        }
                        _ => {}
                    }
                }
                match key {
                    Key::Named(NamedKey::Delete) | Key::Named(NamedKey::Backspace) => st.delete_selection(shift),
                    Key::Named(NamedKey::Escape) if st.ed.path_active() => st.ed.path_exit(),
                    Key::Named(NamedKey::Escape) => {
                        st.ed.sel.clear();
                        st.ed.sel_edge = None;
                    }
                    Key::Named(NamedKey::Enter) | Key::Named(NamedKey::F2) => st.edit_selected(),
                    _ if !ctrl && letter == Some(KeyCode::KeyP) => st.ed.path_enter(),
                    Key::Named(NamedKey::Tab) => {
                        st.ed.tab_new();
                        st.settle();
                    }
                    Key::Named(NamedKey::Home) => st.ed.engine.fit(),
                    Key::Named(NamedKey::F11) => {
                        let full = st.window.fullscreen().is_some();
                        st.window.set_fullscreen(if full { None } else { Some(winit::window::Fullscreen::Borderless(None)) });
                    }
                    Key::Character("f") | Key::Character("а") if !ctrl => st.ed.engine.fit(),
                    Key::Character("=") | Key::Character("+") => st.ed.engine.zoom_at(1.25, cx, cy),
                    Key::Character("-") => st.ed.engine.zoom_at(0.8, cx, cy),
                    _ => {}
                }
                st.window.request_redraw();
            }
            WindowEvent::RedrawRequested => {
                let next = st.last_frame + st.frame_interval;
                if !self.args.vsync && st.shown && Instant::now() + Duration::from_micros(500) < next {
                    st.pending = true;
                    return;
                }
                st.last_frame = Instant::now();
                let mut touchpad_busy = false;
                if let Some(dm) = &st.dm {
                    for g in dm.poll() {
                        match g {
                            dm::Gesture::Pan(dx, dy) => st.ed.engine.pan(dx, dy),
                            dm::Gesture::Zoom(f) => st.ed.engine.zoom_at(f, st.ed.cursor.x, st.ed.cursor.y),
                        }
                    }
                    touchpad_busy = dm.busy();
                }
                if st.script.is_some() {
                    if !st.script_step() {
                        st.quit(el);
                        return;
                    }
                    st.window.request_redraw();
                }
                let ms = st.started.elapsed().as_millis() as u64;
                let gliding = st.fingers.tick(&mut st.ed, ms);
                let benching = st.step_bench();
                if !benching && st.auto.is_some() {
                    st.finish_bench();
                    if self.args.autobench {
                        el.exit();
                        return;
                    }
                }
                st.refresh_overlay();
                let drawn = st.ed.engine.render();
                if drawn && !st.shown {
                    st.shown = true;
                    chrome::cloak(false);
                }
                let now = Instant::now();
                if let Some(b) = &mut st.auto {
                    if let Some(last) = b.last {
                        let s = st.ed.engine.stats;
                        b.intervals.push((now - last).as_secs_f32() * 1000.0);
                        b.cpu.push(s.frame_ms);
                        b.text.push(s.text_ms);
                        b.rows.push(format!(
                            "{:.2},{:.2},{:.5},{},{},{},{:.2},{:.2}",
                            b.start.elapsed().as_secs_f32(),
                            (now - last).as_secs_f32() * 1000.0,
                            st.ed.engine.cam.s,
                            s.rects,
                            s.lines,
                            s.glyphs,
                            s.frame_ms,
                            s.text_ms
                        ));
                    }
                    b.last = Some(now);
                }
                if st.auto.is_some() || touchpad_busy || gliding || st.animating() || st.ed.search.is_some() || !drawn {
                    st.window.request_redraw();
                }
            }
            _ => {}
        }
    }
}

impl App {
    fn wake(&mut self, el: &ActiveEventLoop) {
        let Some(st) = &mut self.state else { return };
        st.maybe_save();
        let mut until = st.save_at;
        if st.pending {
            let next = st.last_frame + st.frame_interval;
            if Instant::now() + Duration::from_micros(500) >= next {
                st.pending = false;
                st.window.request_redraw();
            } else {
                until = Some(until.map_or(next, |u| u.min(next)));
            }
        }
        el.set_control_flow(match until {
            Some(t) => ControlFlow::WaitUntil(t),
            None => ControlFlow::Wait,
        });
    }
}

fn main() {
    log_panics();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().is_some_and(|a| cli::COMMANDS.contains(&a.as_str())) {
        std::process::exit(cli::run(args));
    }
    let el = EventLoop::<()>::with_user_event().build().expect("event loop");
    el.set_control_flow(ControlFlow::Wait);
    let args = parse_args();
    let _ = DEV_RUN.set(args.script.is_some() || args.shot.is_some() || args.autobench || args.bench.is_some());
    let mut app = App { args, state: None, proxy: el.create_proxy() };
    el.run_app(&mut app).expect("run");
}
