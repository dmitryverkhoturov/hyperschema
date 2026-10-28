//! Windows shell (winit window for now): input mapping, window title HUD
//! and a scripted benchmark.
//!
//! Usage: hyperschema [--bench N] [--autobench] [--vsync] [--gpu high] [--shot out.png [--cam x,y,scale]] [file.excalidraw(.md)]

use hs_core::{bench, excalidraw, wgpu, Engine};
use std::sync::Arc;
use std::time::{Duration, Instant};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{Window, WindowId};

struct Args {
    bench: usize,
    file: Option<String>,
    autobench: bool,
    vsync: bool,
    high_perf: bool,
    /// Off-screen capture: save PNG and exit.
    shot: Option<String>,
    /// Camera for the capture: centre x, centre y, scale (or fit if absent).
    cam: Option<(f64, f64, f64)>,
}

fn parse_args() -> Args {
    let mut a = Args { bench: 500_000, file: None, autobench: false, vsync: false, high_perf: false, shot: None, cam: None };
    let mut it = std::env::args().skip(1);
    while let Some(s) = it.next() {
        match s.as_str() {
            "--bench" => a.bench = it.next().and_then(|v| v.parse().ok()).unwrap_or(500_000),
            "--autobench" => a.autobench = true,
            "--vsync" => a.vsync = true,
            "--gpu" => a.high_perf = it.next().as_deref() == Some("high"),
            "--shot" => a.shot = it.next(),
            "--cam" => {
                let v: Vec<f64> = it.next().unwrap_or_default().split(',').filter_map(|x| x.parse().ok()).collect();
                if v.len() == 3 {
                    a.cam = Some((v[0], v[1], v[2]));
                }
            }
            _ => a.file = Some(s),
        }
    }
    a
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
    mods: ModifiersState,
    cursor: PhysicalPosition<f64>,
    drag: Option<PhysicalPosition<f64>>,
    touchpad_until: Instant,
    frames: u32,
    fps_t: Instant,
    fps: f32,
    title_base: String,
    auto: Option<AutoBench>,
    frame_interval: Duration,
    last_frame: Instant,
    /// A redraw was requested too early and waits for the next slot.
    pending: bool,
}

impl State {
    fn update_title(&self) {
        let s = self.engine.stats;
        self.window.set_title(&format!(
            "{} | {:.0} fps | кадр {:.1} мс | текст {:.1} мс | видно: {} фигур, {} линий, {} глифов | зум {:.4}",
            self.title_base, self.fps, s.frame_ms, s.text_ms, s.rects, s.lines, s.glyphs, self.engine.cam.s
        ));
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
            "{}\nкадров {}, средний интервал {:.2} мс ({:.1} fps), p50 {:.2}, p95 {:.2}, p99 {:.2}, max {:.2} мс\nкадров >20 мс: {} ({:.1}%)\nCPU на кадр: среднее {:.2} мс, max {:.2} мс; пересборка текста max {:.2} мс\n",
            self.title_base,
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
        println!("{report}");
        let _ = std::fs::write("bench-result.txt", &report);
        let _ = std::fs::write("bench-frames.csv", format!("t,interval,scale,rects,lines,glyphs,cpu,text
{}", b.rows.join("
")));
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
        let t = Instant::now();
        let (scene, name) = match &self.args.file {
            Some(path) => {
                let text = std::fs::read_to_string(path).expect("read file");
                (excalidraw::load_str(&text).expect("parse"), path.rsplit(['/', '\\']).next().unwrap_or(path).to_string())
            }
            None => (bench::generate(self.args.bench), format!("тест {} блоков", self.args.bench)),
        };
        let n = scene.element_count();
        engine.set_scene(scene);
        if let Some((x, y, sc)) = self.args.cam {
            engine.cam.cx = x;
            engine.cam.cy = y;
            engine.cam.s = sc;
        }
        if let Some(path) = &self.args.shot {
            let (w, h, rgba) = engine.capture();
            let file = std::fs::File::create(path).expect("create png");
            let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w, h);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            enc.write_header().and_then(|mut wr| wr.write_image_data(&rgba)).expect("write png");
            let b = engine.scene().bounds();
            println!("saved {path} {w}x{h}; bounds {:?}; cam {:?}", b, engine.cam);
            el.exit();
            return;
        }
        let title_base = format!("Hyperschema — {name}: {n} элементов, загрузка {:.1} с — {}", t.elapsed().as_secs_f32(), engine.adapter_info);
        let mut st = State {
            window,
            engine,
            mods: ModifiersState::default(),
            cursor: PhysicalPosition::new(0.0, 0.0),
            drag: None,
            touchpad_until: Instant::now(),
            frames: 0,
            fps_t: Instant::now(),
            fps: 0.0,
            title_base,
            auto: None,
            frame_interval,
            last_frame: Instant::now() - Duration::from_secs(1),
            pending: false,
        };
        if self.args.autobench {
            st.start_bench();
        }
        st.update_title();
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
                        // Precision touchpads send fractional wheel deltas; a
                        // mouse sends whole notches. Ctrl+wheel is the pinch.
                        let fractional = dx.fract() != 0.0 || dy.fract() != 0.0 || dx != 0.0;
                        if fractional {
                            st.touchpad_until = Instant::now() + Duration::from_millis(400);
                        }
                        let touchpad = Instant::now() < st.touchpad_until;
                        if st.mods.control_key() {
                            st.engine.zoom_at(1.18f64.powf(dy as f64), x, y);
                        } else if touchpad {
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
                match event.logical_key.as_ref() {
                    Key::Named(NamedKey::Home) | Key::Character("f") | Key::Character("а") => st.engine.fit(),
                    Key::Character("=") | Key::Character("+") => st.engine.zoom_at(1.25, cx, cy),
                    Key::Character("-") => st.engine.zoom_at(0.8, cx, cy),
                    Key::Character("b") | Key::Character("и") => st.start_bench(),
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
                        b.intervals.push((now - last).as_secs_f32() * 1000.0);
                        b.cpu.push(st.engine.stats.frame_ms);
                        b.text.push(st.engine.stats.text_ms);
                        let s = st.engine.stats;
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
                    st.window.request_redraw();
                }
                st.frames += 1;
                let dt = st.fps_t.elapsed().as_secs_f32();
                if dt > 0.5 {
                    st.fps = st.frames as f32 / dt;
                    st.frames = 0;
                    st.fps_t = Instant::now();
                    if st.auto.is_none() {
                        st.update_title();
                    }
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
    let el = EventLoop::new().expect("event loop");
    el.set_control_flow(ControlFlow::Wait);
    let mut app = App { args: parse_args(), state: None };
    el.run_app(&mut app).expect("run");
}
