//! Windows shell: one window, opens the last scheme, touchpad through
//! DirectManipulation, frames paced by the shell.
//!
//! Usage: hyperschema [file.excalidraw(.md)]
//! Dev:   --bench N | --autobench | --vsync | --gpu low | --shot out.png [--cam x,y,scale]

#![windows_subsystem = "windows"]

mod dm;

use hs_core::{bench, excalidraw, wgpu, Engine};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{Window, WindowId};

const DEFAULT_SCHEME: &str = "board.excalidraw.md";

struct Args {
    bench: Option<usize>,
    file: Option<PathBuf>,
    autobench: bool,
    vsync: bool,
    high_perf: bool,
    shot: Option<String>,
    cam: Option<(f64, f64, f64)>,
}

fn parse_args() -> Args {
    // The discrete GPU by default: the 2016 Intel OpenGL driver on this
    // laptop crashes (access violation in ig75icd64.dll).
    let mut a = Args { bench: None, file: None, autobench: false, vsync: false, high_perf: true, shot: None, cam: None };
    let mut it = std::env::args().skip(1);
    while let Some(s) = it.next() {
        match s.as_str() {
            "--bench" => a.bench = Some(it.next().and_then(|v| v.parse().ok()).unwrap_or(500_000)),
            "--autobench" => a.autobench = true,
            "--vsync" => a.vsync = true,
            "--gpu" => a.high_perf = it.next().as_deref() != Some("low"),
            "--shot" => a.shot = it.next(),
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

fn data_dir() -> PathBuf {
    let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    base.join("Hyperschema")
}

fn last_file() -> Option<PathBuf> {
    let p = std::fs::read_to_string(data_dir().join("last.txt")).ok()?;
    let p = PathBuf::from(p.trim());
    p.exists().then_some(p)
}

fn remember(path: &Path) {
    let _ = std::fs::create_dir_all(data_dir());
    let _ = std::fs::write(data_dir().join("last.txt"), path.to_string_lossy().as_bytes());
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

struct State {
    window: Arc<Window>,
    engine: Engine,
    dm: Option<dm::Dm>,
    mods: ModifiersState,
    cursor: PhysicalPosition<f64>,
    drag: Option<PhysicalPosition<f64>>,
    touchpad_until: Instant,
    auto: Option<AutoBench>,
    frame_interval: Duration,
    last_frame: Instant,
    /// A redraw was requested too early and waits for the next slot.
    pending: bool,
    name: String,
}

impl State {
    fn set_title(&self) {
        // Set once per file: retitling a window costs a compositor round
        // trip that showed up as 400 ms stalls when done every 0.5 s.
        self.window.set_title(&format!("{} — Hyperschema", self.name));
    }

    fn open(&mut self, path: &Path) {
        match std::fs::read_to_string(path).map_err(|e| e.to_string()).and_then(|t| excalidraw::load_str(&t)) {
            Ok(scene) => {
                self.engine.set_scene(scene);
                self.name = path.file_name().map(|n| n.to_string_lossy().replace(".excalidraw.md", "").replace(".excalidraw", "")).unwrap_or_default();
                remember(path);
            }
            Err(e) => self.name = format!("не открылось: {e}"),
        }
        self.set_title();
        self.window.request_redraw();
    }

    fn start_bench(&mut self) {
        self.engine.fit();
        let c = self.engine.cam;
        self.auto = Some(AutoBench {
            start: Instant::now(),
            from: (c.cx, c.cy, c.s),
            last: None,
            intervals: Vec::new(),
            cpu: Vec::new(),
            text: Vec::new(),
            rows: Vec::new(),
        });
    }

    /// Advances the scripted camera; returns false when done.
    fn step_bench(&mut self) -> bool {
        let Some(b) = &mut self.auto else { return false };
        let t = b.start.elapsed().as_secs_f64();
        let (cx, cy, s0) = b.from;
        let deep = 1.5f64;
        let cam = &mut self.engine.cam;
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
            self.engine.scene().element_count(),
            self.engine.adapter_info,
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
}

struct App {
    args: Args,
    state: Option<State>,
}

impl ApplicationHandler for App {
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
        let window = Arc::new(
            el.create_window(Window::default_attributes().with_title("Hyperschema").with_inner_size(LogicalSize::new(1280, 800)))
                .expect("window"),
        );
        let size = window.inner_size();
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let surface = instance.create_surface(window.clone()).expect("surface");
        let font = std::fs::read(r"C:\Windows\Fonts\segoeui.ttf").expect("font");
        let mut engine = pollster::block_on(Engine::new(&instance, surface, size.width, size.height, font, self.args.high_perf));
        // Driver vsync on this class of laptop (GL on Intel, Vulkan through
        // Optimus) paces at ~3 refreshes; we pace frames ourselves instead.
        engine.set_vsync(self.args.vsync);
        let refresh = window.current_monitor().and_then(|m| m.refresh_rate_millihertz()).unwrap_or(60_000);
        let frame_interval = Duration::from_micros(1_000_000_000 / refresh as u64);

        let dm = match window.window_handle().map(|h| h.as_raw()) {
            Ok(RawWindowHandle::Win32(h)) => dm::Dm::new(windows::Win32::Foundation::HWND(h.hwnd.get() as *mut _)).ok(),
            _ => None,
        };
        let mut st = State {
            window,
            engine,
            dm,
            mods: ModifiersState::default(),
            cursor: PhysicalPosition::new(0.0, 0.0),
            drag: None,
            touchpad_until: Instant::now(),
            auto: None,
            frame_interval,
            last_frame: Instant::now() - Duration::from_secs(1),
            pending: false,
            name: String::new(),
        };

        if let Some(n) = self.args.bench {
            st.engine.set_scene(bench::generate(n));
            st.name = format!("тест {n} блоков");
            st.set_title();
        } else {
            let path = self.args.file.clone().or_else(last_file).unwrap_or_else(|| PathBuf::from(DEFAULT_SCHEME));
            st.open(&path);
        }
        if let Some((x, y, sc)) = self.args.cam {
            st.engine.cam.cx = x;
            st.engine.cam.cy = y;
            st.engine.cam.s = sc;
        }
        if let Some(path) = &self.args.shot {
            let (w, h, rgba) = st.engine.capture();
            let file = std::fs::File::create(path).expect("create png");
            let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w, h);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            enc.write_header().and_then(|mut wr| wr.write_image_data(&rgba)).expect("write png");
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
            WindowEvent::CloseRequested => el.exit(),
            WindowEvent::Resized(sz) => {
                st.engine.resize(sz.width, sz.height);
                st.window.request_redraw();
            }
            WindowEvent::DroppedFile(path) => st.open(&path),
            WindowEvent::ModifiersChanged(m) => st.mods = m.state(),
            WindowEvent::CursorMoved { position, .. } => {
                if let Some(d) = st.drag {
                    st.engine.pan(position.x - d.x, position.y - d.y);
                    st.drag = Some(position);
                    st.window.request_redraw();
                }
                st.cursor = position;
            }
            WindowEvent::MouseInput { state, button, .. } => {
                if matches!(button, MouseButton::Left | MouseButton::Middle) {
                    st.drag = (state == ElementState::Pressed).then_some(st.cursor);
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                // Touchpads normally arrive through DirectManipulation; this
                // is the mouse wheel and a fallback if DM is unavailable.
                let (x, y) = (st.cursor.x, st.cursor.y);
                match delta {
                    MouseScrollDelta::PixelDelta(p) => {
                        if st.mods.control_key() {
                            st.engine.zoom_at((p.y * 0.01).exp(), x, y);
                        } else {
                            st.engine.pan(p.x, p.y);
                        }
                    }
                    MouseScrollDelta::LineDelta(dx, dy) => {
                        let fractional = dx.fract() != 0.0 || dy.fract() != 0.0 || dx != 0.0;
                        if fractional {
                            st.touchpad_until = Instant::now() + Duration::from_millis(400);
                        }
                        let touchpad = Instant::now() < st.touchpad_until;
                        if touchpad && !st.mods.control_key() {
                            st.engine.pan(dx as f64 * 60.0, dy as f64 * 60.0);
                        } else {
                            st.engine.zoom_at(1.18f64.powf(dy as f64), x, y);
                        }
                    }
                }
                st.window.request_redraw();
            }
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                let (w, h) = st.engine.size();
                let (cx, cy) = (w as f64 / 2.0, h as f64 / 2.0);
                let ctrl = st.mods.control_key();
                match event.logical_key.as_ref() {
                    Key::Character("o") | Key::Character("щ") if ctrl => {
                        let pick = rfd::FileDialog::new()
                            .add_filter("Excalidraw", &["excalidraw", "md", "json"])
                            .set_title("Открыть схему")
                            .pick_file();
                        if let Some(p) = pick {
                            st.open(&p);
                        }
                    }
                    Key::Named(NamedKey::Home) | Key::Character("f") | Key::Character("а") => st.engine.fit(),
                    Key::Character("=") | Key::Character("+") => st.engine.zoom_at(1.25, cx, cy),
                    Key::Character("-") => st.engine.zoom_at(0.8, cx, cy),
                    _ => {}
                }
                st.window.request_redraw();
            }
            WindowEvent::RedrawRequested => {
                let next = st.last_frame + st.frame_interval;
                if !self.args.vsync && Instant::now() + Duration::from_micros(500) < next {
                    st.pending = true;
                    el.set_control_flow(ControlFlow::WaitUntil(next));
                    return;
                }
                st.last_frame = Instant::now();
                let mut touchpad_busy = false;
                if let Some(dm) = &st.dm {
                    for g in dm.poll() {
                        match g {
                            dm::Gesture::Pan(dx, dy) => st.engine.pan(dx, dy),
                            dm::Gesture::Zoom(f) => st.engine.zoom_at(f, st.cursor.x, st.cursor.y),
                        }
                    }
                    touchpad_busy = dm.busy();
                }
                let benching = st.step_bench();
                if !benching && st.auto.is_some() {
                    st.finish_bench();
                    if self.args.autobench {
                        el.exit();
                        return;
                    }
                }
                st.engine.render();
                let now = Instant::now();
                if let Some(b) = &mut st.auto {
                    if let Some(last) = b.last {
                        let s = st.engine.stats;
                        b.intervals.push((now - last).as_secs_f32() * 1000.0);
                        b.cpu.push(s.frame_ms);
                        b.text.push(s.text_ms);
                        b.rows.push(format!(
                            "{:.2},{:.2},{:.5},{},{},{},{:.2},{:.2}",
                            b.start.elapsed().as_secs_f32(),
                            (now - last).as_secs_f32() * 1000.0,
                            st.engine.cam.s,
                            s.rects,
                            s.lines,
                            s.glyphs,
                            s.frame_ms,
                            s.text_ms
                        ));
                    }
                    b.last = Some(now);
                }
                if st.auto.is_some() || touchpad_busy {
                    st.window.request_redraw();
                }
            }
            _ => {}
        }
    }
}

impl App {
    fn wake(&mut self, el: &ActiveEventLoop) {
        if let Some(st) = &mut self.state {
            if st.pending && Instant::now() + Duration::from_micros(500) >= st.last_frame + st.frame_interval {
                st.pending = false;
                el.set_control_flow(ControlFlow::Wait);
                st.window.request_redraw();
            }
        }
    }
}

fn main() {
    log_panics();
    let el = EventLoop::new().expect("event loop");
    el.set_control_flow(ControlFlow::Wait);
    let mut app = App { args: parse_args(), state: None };
    el.run_app(&mut app).expect("run");
}
