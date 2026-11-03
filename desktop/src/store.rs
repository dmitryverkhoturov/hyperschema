//! Boards: kept in this device's library (`%APPDATA%\Hyperschema\boards`,
//! one Automerge document each, see core `library.rs`) and synced through
//! the server when `sync.json` is set up. Saved by itself half a second
//! after each finished action (no Save button, no question on close).
//! Files (`.hyperschema`, Excalidraw) are imported once into a new board
//! and never written to; opening the same file again opens that board.

use hs_core::editor::Mode;
use crate::State;
use hs_core::board::Board;
use hs_core::library::Library;
use hs_core::scene::{rgba, Align, Label, Line, Rect, Scene, Shape};
use hs_core::sync::{heads_of, Step};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

pub const EXT: &str = "hyperschema";
const SAVE_DELAY: Duration = Duration::from_millis(500);

pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// The library: the real one, or a scratch one for scripted test runs (so
/// tests never touch real boards or the server).
pub fn library(test: bool) -> Library {
    let test = test && std::env::var_os("HS_SYNC").is_none();
    let dir = if test { std::env::temp_dir().join(format!("hyperschema-test-{}", std::process::id())) } else { crate::data_dir().join("boards") };
    Library::open(dir, false)
}

fn is_excalidraw(p: &Path) -> bool {
    let n = p.to_string_lossy().to_lowercase();
    n.ends_with(".excalidraw") || n.ends_with(".excalidraw.md") || n.ends_with(".excalidraw.json")
}

/// "Board.excalidraw.md" -> "Board".
pub fn stem(p: &Path) -> String {
    let n = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    for suf in [".excalidraw.md", ".excalidraw.json", ".excalidraw", ".hyperschema", ".md", ".json"] {
        if let Some(s) = n.strip_suffix(suf) {
            return s.to_string();
        }
    }
    n
}

/// Local date and time for the CLI: "2026-11-03 15:25".
pub fn when(secs: u64) -> String {
    use windows::Win32::Foundation::{FILETIME, SYSTEMTIME};
    use windows::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};
    let ft = (secs + 11_644_473_600) * 10_000_000;
    let ft = FILETIME { dwLowDateTime: ft as u32, dwHighDateTime: (ft >> 32) as u32 };
    let (mut utc, mut local) = (SYSTEMTIME::default(), SYSTEMTIME::default());
    unsafe {
        let _ = FileTimeToSystemTime(&ft, &mut utc);
        let _ = SystemTimeToTzSpecificLocalTime(None, &utc, &mut local);
    }
    format!("{:04}-{:02}-{:02} {:02}:{:02}", local.wYear, local.wMonth, local.wDay, local.wHour, local.wMinute)
}

/// Which board each imported file became: `imports.json`, path -> id.
fn imports() -> Value {
    std::fs::read_to_string(crate::data_dir().join("imports.json")).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(json!({}))
}

fn remember_import(path: &Path, id: &str) {
    let mut m = imports();
    m[path.to_string_lossy().to_lowercase()] = json!(id);
    let _ = std::fs::write(crate::data_dir().join("imports.json"), m.to_string());
}

/// Where a board's walks are kept: per device, not in the board (two
/// devices walking the same board would fight over them in sync).
fn walks_file(id: &str) -> PathBuf {
    crate::data_dir().join("walks").join(format!("{id}.json"))
}

/// A name no board of this device has yet.
fn fresh_name(lib: &Library, name: &str) -> String {
    let taken: Vec<String> = lib.list().into_iter().map(|x| x.1.to_lowercase()).collect();
    let mut k = 1;
    loop {
        let n = if k == 1 { name.to_string() } else { format!("{name} {k}") };
        if !taken.contains(&n.to_lowercase()) {
            return n;
        }
        k += 1;
    }
}

/// Browsing history: every device's changes, oldest first.
pub struct Timeline {
    pub steps: Vec<Step>,
    /// How many steps are applied: steps.len() is "now".
    pub pos: usize,
    pub preview: Option<Board>,
    pub dragging: bool,
}

impl State {
    /// Scripted runs stay off the real library and the server, unless
    /// HS_SYNC=1 asks to test sync itself (with a test APPDATA).
    fn test_run(&self) -> bool {
        (self.script.is_some() || crate::harness().is_some()) && std::env::var_os("HS_SYNC").is_none()
    }

    /// Opens a file: the board it was imported into, or a new import.
    pub fn open(&mut self, path: &Path) {
        if let Some(id) = imports()[path.to_string_lossy().to_lowercase()].as_str().filter(|id| self.lib.exists(id)).map(String::from) {
            self.open_board(&id);
            return;
        }
        let result = std::fs::read_to_string(path).map_err(|e| e.to_string()).and_then(|text| {
            if is_excalidraw(path) {
                hs_core::import::from_excalidraw(&text).map(|x| x.0)
            } else {
                Board::parse(&text)
            }
        });
        match result {
            Ok(board) => {
                let name = fresh_name(&self.lib, &stem(path));
                match self.lib.create(&name, &board) {
                    Ok(id) => {
                        if !self.test_run() {
                            remember_import(path, &id);
                        }
                        self.open_board(&id);
                    }
                    Err(e) => self.name = format!("не открылось: {e}"),
                }
            }
            Err(e) => self.name = format!("не открылось: {e}"),
        }
        self.window.request_redraw();
    }

    /// At start: the board open last time; before the library existed, the
    /// file open last time (imported once).
    pub fn open_last(&mut self, old_file: Option<PathBuf>) {
        let last = std::fs::read_to_string(crate::data_dir().join("last-board.txt")).unwrap_or_default();
        let last = last.trim();
        if !last.is_empty() && self.lib.exists(last) {
            let id = last.to_string();
            self.open_board(&id);
        } else if let Some(p) = old_file.filter(|p| p.exists()) {
            self.open(&p);
        } else if let Some((id, _)) = self.lib.list().into_iter().next() {
            self.open_board(&id);
        } else {
            self.new_board();
        }
    }

    pub fn open_board(&mut self, id: &str) {
        self.commit_text();
        self.flush();
        self.save_walks();
        let live = match self.lib.load(id) {
            Ok(l) => l,
            Err(e) => {
                self.name = format!("не открылось: {e}");
                return;
            }
        };
        self.ed.board = live.board();
        self.ed.undo_stack.clear();
        self.ed.redo_stack.clear();
        self.unsaved.clear();
        self.save_at = None;
        self.ed.step_base = None;
        self.ed.mode = Mode::Idle;
        self.ed.sel.clear();
        self.ed.sel_edge = None;
        self.ed.hover = None;
        self.timeline = None;
        self.ed.frozen = false;
        self.ed.preview = None;
        self.ed.hidden_nodes.clear();
        self.ed.hidden_edges.clear();
        self.remote_waiting = false;
        self.sync_note = None;
        self.name = live.name.clone();
        self.window.set_title(&self.name);
        self.live = Some(live);
        if !self.test_run() {
            let _ = std::fs::write(crate::data_dir().join("last-board.txt"), id);
            if let Some(cfg) = crate::cli::config() {
                let proxy = self.proxy.clone();
                self.live.as_mut().unwrap().connect(cfg, move || {
                    let _ = proxy.send_event(());
                });
            }
        }
        self.rebuild();
        self.ed.engine.fit();
        // The walks of this board, the one going on when it closed resumed.
        self.ed.trail = None;
        let walks = std::fs::read_to_string(walks_file(id)).unwrap_or_default();
        self.ed.set_walks_json(&walks);
        self.walks_saved = self.ed.walks_json();
        self.window.request_redraw();
    }

    /// Keeps the board's walks on disk when they changed.
    pub fn save_walks(&mut self) {
        let Some(live) = &self.live else { return };
        let now = self.ed.walks_json();
        if now != self.walks_saved {
            let f = walks_file(&live.id);
            let _ = std::fs::create_dir_all(f.parent().unwrap());
            if std::fs::write(&f, &now).is_ok() {
                self.walks_saved = now;
            }
        }
    }

    /// Queues a finished step for saving.
    pub fn record(&mut self, mut c: hs_core::board::Change) {
        if c.t == 0 {
            c.t = now_ms();
        }
        self.unsaved.push(c);
        self.save_at = Some(Instant::now() + SAVE_DELAY);
    }

    /// Saves now if a save is due and nothing is mid-gesture; takes in
    /// changes from other devices that waited for the gesture to end.
    pub fn maybe_save(&mut self) {
        let idle = matches!(self.ed.mode, Mode::Idle);
        if self.save_at.is_some_and(|t| Instant::now() >= t) && idle {
            self.flush();
        }
        if self.remote_waiting && idle && self.unsaved.is_empty() {
            self.take_remote();
        }
    }

    /// Writes pending steps into the board's document (and so to the server).
    pub fn flush(&mut self) {
        self.save_at = None;
        if self.unsaved.is_empty() {
            return;
        }
        let Some(live) = &mut self.live else { return };
        let steps = std::mem::take(&mut self.unsaved);
        live.record(&steps);
        if let Err(e) = live.save() {
            crate::dlog(format!("save failed: {e}"));
            self.name = format!("{} — не сохраняется: {e}", live.name);
            self.save_at = Some(Instant::now() + Duration::from_secs(5));
        }
    }

    /// Sync woke the window: merge what came, show the connection state.
    pub fn on_sync(&mut self) {
        let Some(live) = &mut self.live else { return };
        let (merged, status) = live.poll();
        let _ = live.save();
        if let Some(s) = status {
            if s != self.sync_note {
                self.sync_note = s;
                self.window.request_redraw();
            }
        }
        if merged {
            self.remote_waiting = true;
            self.maybe_save();
        }
    }

    /// Shows the board as the document has it now, with other devices'
    /// changes; the camera, selection and walk stay.
    fn take_remote(&mut self) {
        self.remote_waiting = false;
        let Some(live) = &self.live else { return };
        if self.timeline.is_some() {
            self.remote_waiting = true;
            return;
        }
        let board = live.board();
        self.ed.board = board;
        let b = &self.ed.board;
        self.ed.sel.retain(|id| b.get(id).is_some());
        if self.ed.sel_edge.as_ref().is_some_and(|e| b.edge(e).is_none()) {
            self.ed.sel_edge = None;
        }
        self.ed.hover = None;
        self.rebuild();
        self.window.request_redraw();
    }

    /// Boards the server has, for the menu; fetched off the window thread.
    pub fn fetch_cards(&mut self) {
        let Some(cfg) = crate::cli::config().filter(|_| !self.test_run()) else { return };
        let cards = self.cards.clone();
        std::thread::spawn(move || {
            if let Ok(list) = hs_core::couch::Couch::new(cfg).boards() {
                *cards.lock().unwrap() = list.into_iter().map(|b| (b.id, b.name)).collect();
            }
        });
    }

    pub fn new_board(&mut self) {
        let name = fresh_name(&self.lib, "Новая схема");
        match self.lib.create(&name, &Board::new()) {
            Ok(id) => self.open_board(&id),
            Err(e) => self.name = format!("не создалось: {e}"),
        }
    }

    pub fn open_dialog(&mut self) {
        let pick = rfd::FileDialog::new().set_directory(self.dialog_dir()).add_filter("Схемы", &[EXT, "excalidraw", "md"]).set_title("Открыть файл схемы").pick_file();
        if let Some(p) = pick {
            Self::remember_dir(&p);
            self.open(&p);
        }
    }

    /// Where a file dialog starts: the folder last picked in one.
    fn dialog_dir(&self) -> PathBuf {
        std::fs::read_to_string(crate::data_dir().join("dialog-dir.txt")).ok().map(|s| PathBuf::from(s.trim())).filter(|d| d.is_dir()).unwrap_or_default()
    }

    fn remember_dir(picked: &Path) {
        if let Some(d) = picked.parent() {
            let _ = std::fs::write(crate::data_dir().join("dialog-dir.txt"), d.to_string_lossy().as_bytes());
        }
    }

    /// Adds another board (ours or Excalidraw) to the open one.
    pub fn import_dialog(&mut self) {
        let pick = rfd::FileDialog::new()
            .set_directory(self.dialog_dir())
            .add_filter("Схемы", &[EXT, "excalidraw", "md", "json"])
            .set_title("Добавить схему")
            .pick_file();
        let Some(src) = pick else { return };
        Self::remember_dir(&src);
        if self.live.is_none() {
            self.open(&src);
            return;
        }
        self.add_board(&src);
    }

    /// Adds the board in `src` (ours or Excalidraw) to the open one.
    pub fn add_board(&mut self, src: &Path) {
        let other = std::fs::read_to_string(src).map_err(|e| e.to_string()).and_then(|text| {
            if text.trim_start().starts_with('{') && text.contains("\"hyperschema\"") {
                Board::parse(&text)
            } else {
                hs_core::import::from_excalidraw(&text).map(|x| x.0)
            }
        });
        match other {
            Ok(b) => {
                self.ed.merge_in(b);
                self.settle();
            }
            Err(e) => self.name = format!("{} — не добавилось: {e}", self.live.as_ref().map(|l| l.name.clone()).unwrap_or_default()),
        }
    }

    /// Writes the open board to a `.hyperschema` file (a copy; the board
    /// itself stays in the library).
    pub fn export_dialog(&mut self) {
        let Some(live) = &self.live else { return };
        let pick = rfd::FileDialog::new().set_directory(self.dialog_dir()).set_file_name(format!("{}.{EXT}", live.name)).add_filter("Hyperschema", &[EXT]).set_title("Сохранить копию в файл").save_file();
        if let Some(p) = pick {
            Self::remember_dir(&p);
            let _ = std::fs::write(&p, self.ed.board.serialize());
        }
    }

    pub fn main_menu(&mut self) {
        let h = crate::chrome::strip_h();
        let mut items: Vec<(i32, String, bool)> = vec![(1, "Новая схема".into(), false), (2, "Открыть файл…".into(), false)];
        let mut boards = self.lib.list();
        for (id, name) in self.cards.lock().unwrap().iter() {
            if !boards.iter().any(|b| &b.0 == id) {
                boards.push((id.clone(), name.clone()));
            }
        }
        boards.sort_by(|a, b| a.1.to_lowercase().cmp(&b.1.to_lowercase()));
        let open_id = self.live.as_ref().map(|l| l.id.clone());
        if !boards.is_empty() {
            items.push((-2, "Схемы".into(), false));
            for (i, (id, name)) in boards.iter().enumerate() {
                items.push((100 + i as i32, name.clone(), Some(id) == open_id.as_ref()));
            }
            items.push((-1, String::new(), false));
        }
        items.extend([(3, "Добавить схему…".into(), false), (4, "Сохранить копию в файл…".into(), false), (0, String::new(), false), (5, "История…".into(), false), (0, String::new(), false), (6, "Связи с вложенными".into(), self.ed.show_nested_links)]);
        match crate::chrome::popup(0.0, h, &items) {
            1 => self.new_board(),
            2 => self.open_dialog(),
            3 => self.import_dialog(),
            4 => self.export_dialog(),
            5 => self.open_timeline(),
            6 => {
                self.ed.show_nested_links = !self.ed.show_nested_links;
                let _ = std::fs::write(crate::data_dir().join("show-nested-links.txt"), if self.ed.show_nested_links { "1" } else { "0" });
                self.ed.rebuild();
            }
            i if i >= 100 => {
                if let Some((id, name)) = boards.get((i - 100) as usize).cloned() {
                    if !self.lib.exists(&id) {
                        // Known only to the server: the first pull fills it.
                        let _ = self.lib.adopt(&id, &name);
                    }
                    self.open_board(&id);
                }
            }
            _ => {}
        }
        self.fetch_cards();
    }

    // ------------------------------------------------------------ timeline

    pub fn open_timeline(&mut self) {
        self.commit_text();
        self.flush();
        let steps = self.live.as_mut().map(|l| l.shared.steps()).unwrap_or_default();
        let pos = steps.len();
        self.ed.sel.clear();
        self.ed.sel_edge = None;
        self.timeline = Some(Timeline { steps, pos, preview: None, dragging: false });
        self.ed.frozen = true;
        self.window.request_redraw();
    }

    pub fn close_timeline(&mut self) {
        self.timeline = None;
        self.ed.frozen = false;
        self.ed.preview = None;
        self.rebuild();
    }

    pub fn timeline_seek(&mut self, pos: usize) {
        let Some(t) = &mut self.timeline else { return };
        let pos = pos.min(t.steps.len());
        if pos == t.pos && (t.preview.is_some() || pos == t.steps.len()) {
            return;
        }
        t.pos = pos;
        if pos == t.steps.len() {
            t.preview = None;
        } else {
            let heads = heads_of(&t.steps, pos);
            t.preview = match self.live.as_mut() {
                Some(l) if !heads.is_empty() => l.shared.board_at(&heads),
                _ => Some(Board::new()),
            };
        }
        self.ed.preview = t.preview.clone();
        self.rebuild();
    }

    /// Makes the previewed state current, as a new step.
    pub fn timeline_restore(&mut self) {
        let Some(prev) = self.timeline.as_mut().and_then(|t| t.preview.take()) else {
            self.close_timeline();
            return;
        };
        self.timeline = None;
        self.ed.frozen = false;
        self.ed.preview = None;
        self.begin();
        self.ed.board = prev;
        self.ed.board.reindex();
        self.commit();
        self.settle();
        self.rebuild();
    }

    /// Who made a change, for the timeline.
    fn who(&self, actor: &str) -> &'static str {
        if *actor == self.lib.actor.to_hex_string() {
            "этот компьютер"
        } else {
            "другое устройство"
        }
    }

    fn bar(&self) -> (f32, f32, f32, f32) {
        let (w, h) = self.ed.engine.size();
        let s = self.window.scale_factor() as f32;
        let bh = 64.0 * s;
        (0.0, h as f32 - bh, w as f32, bh)
    }

    fn track(&self) -> (f32, f32, f32) {
        let (_, y, w, bh) = self.bar();
        let s = self.window.scale_factor() as f32;
        (24.0 * s, w - 300.0 * s, y + bh * 0.62)
    }

    /// Mouse press in the timeline bar; true if it was handled there.
    pub fn timeline_press(&mut self) -> bool {
        let Some(t) = &self.timeline else { return false };
        let (_, y, w, _) = self.bar();
        let (cx, cy) = (self.ed.cursor.x as f32, self.ed.cursor.y as f32);
        if cy < y {
            return false;
        }
        let s = self.window.scale_factor() as f32;
        let n = t.steps.len();
        if cx > w - 280.0 * s && cx < w - 150.0 * s {
            self.timeline_restore();
        } else if cx >= w - 140.0 * s {
            self.close_timeline();
        } else {
            let (x0, x1, _) = self.track();
            let k = ((cx - x0) / (x1 - x0)).clamp(0.0, 1.0);
            let pos = (k * n as f32).round() as usize;
            if let Some(t) = &mut self.timeline {
                t.dragging = true;
            }
            self.timeline_seek(pos);
        }
        true
    }

    pub fn timeline_drag(&mut self) {
        let Some(t) = &self.timeline else { return };
        if !t.dragging {
            return;
        }
        let n = t.steps.len();
        let (x0, x1, _) = self.track();
        let k = ((self.ed.cursor.x as f32 - x0) / (x1 - x0)).clamp(0.0, 1.0);
        self.timeline_seek((k * n as f32).round() as usize);
    }

    pub fn timeline_release(&mut self) {
        if let Some(t) = &mut self.timeline {
            t.dragging = false;
        }
    }

    /// Strip on top, timeline bar at the bottom: the screen-space layer.
    pub fn refresh_ui(&mut self) {
        let mut ui = Scene::default();
        let ink = rgba(0x2a, 0x2a, 0x2a, 255);
        let menu_hover = matches!(crate::chrome::part_at(self.ed.cursor.x, self.ed.cursor.y), Some(crate::chrome::Part::Menu)) && self.cursor_in;
        let title = match &self.sync_note {
            Some(_) => format!("{} · нет связи с сервером", self.name),
            None => self.name.clone(),
        };
        crate::chrome::draw(&mut ui, &title, menu_hover, ink);
        let bar = self.ed.path_ui(crate::chrome::strip_h() as f32);
        ui.rects.extend(bar.rects);
        ui.lines.extend(bar.lines);
        ui.labels.extend(bar.labels);
        if let Some(sr) = &self.ed.search {
            // Ctrl+F bar: top centre, under the title strip.
            let s = self.window.scale_factor() as f32;
            let (w, _) = self.ed.engine.size();
            let (bw, bh) = ((440.0 * s).min(w as f32 - 32.0 * s), 38.0 * s);
            let (x, y) = ((w as f32 - bw) * 0.5, crate::chrome::strip_h() as f32 + 6.0 * s);
            ui.rects.push(Rect { x, y, w: bw, h: bh, fill: rgba(255, 255, 255, 250), stroke: rgba(0, 0, 0, 40), stroke_w: 1.0 * s, radius: 8.0 * s, shape: Shape::Rect });
            let size = 15.0 * s;
            let ty = y + (bh - size * hs_core::text::LINE_H) * 0.5;
            let count = match (sr.query.trim().is_empty(), sr.hits.len()) {
                (true, _) => String::new(),
                (false, 0) => "нет".to_string(),
                (false, n) => format!("{} из {n}", sr.at + 1),
            };
            let tx = x + 14.0 * s;
            let shown = if sr.query.is_empty() { "Поиск по схеме".to_string() } else { sr.query.clone() };
            let color = if sr.query.is_empty() { rgba(0, 0, 0, 90) } else { ink };
            let label = Label { x: tx, y: ty, w: bw - 110.0 * s, size, color, align: Align::Left, wrap: false, text: shown };
            if (self.started.elapsed().as_millis() / 530) % 2 == 0 {
                let cx = if sr.query.is_empty() { tx } else { tx + self.ed.engine.measure(&sr.query, size).0 };
                ui.rects.push(Rect { x: cx, y: ty, w: 1.5 * s, h: size * hs_core::text::LINE_H, fill: rgba(0x4a, 0x7d, 0xff, 255), stroke: 0, stroke_w: 0.0, radius: 0.0, shape: Shape::Rect });
            }
            ui.labels.push(label);
            ui.labels.push(Label { x: x + bw - 100.0 * s, y: ty, w: 86.0 * s, size: 13.0 * s, color: rgba(0, 0, 0, 120), align: Align::Right, wrap: false, text: count });
        }
        if let Some(t) = &self.timeline {
            let s = self.window.scale_factor() as f32;
            let (x, y, w, bh) = self.bar();
            ui.rects.push(Rect { x, y, w, h: bh, fill: rgba(255, 255, 255, 240), stroke: rgba(0, 0, 0, 30), stroke_w: 1.0, radius: 0.0, shape: Shape::Rect });
            let (x0, x1, ty) = self.track();
            let n = t.steps.len().max(1);
            ui.lines.push(Line { x0, y0: ty, x1, y1: ty, color: rgba(0, 0, 0, 60), width: 2.0 * s, head: false });
            if t.steps.len() <= 400 {
                for k in 0..=t.steps.len() {
                    let tx = x0 + (x1 - x0) * k as f32 / n as f32;
                    ui.lines.push(Line { x0: tx, y0: ty - 4.0 * s, x1: tx, y1: ty + 4.0 * s, color: rgba(0, 0, 0, 50), width: 1.0 * s, head: false });
                }
            }
            let kx = x0 + (x1 - x0) * t.pos as f32 / n as f32;
            let r = 7.0 * s;
            ui.rects.push(Rect { x: kx - r, y: ty - r, w: 2.0 * r, h: 2.0 * r, fill: rgba(0x4a, 0x7d, 0xff, 255), stroke: rgba(255, 255, 255, 255), stroke_w: 2.0 * s, radius: 0.0, shape: Shape::Ellipse });
            let when = if t.pos == t.steps.len() {
                "сейчас".to_string()
            } else {
                let at = t.steps.get(t.pos).map_or(0, |c| c.t.max(0) as u64);
                ago(now_ms().saturating_sub(at))
            };
            let dev = if t.pos < t.steps.len() { t.steps.get(t.pos).map(|c| format!(" · {}", self.who(&c.actor))).unwrap_or_default() } else { String::new() };
            ui.labels.push(Label { x: x0, y: y + 10.0 * s, w: x1 - x0, size: 14.0 * s, color: ink, align: Align::Left, wrap: false, text: format!("История: шаг {} из {} — {when}{dev}", t.pos, t.steps.len()) });
            let btn = |ui: &mut Scene, bx: f32, bw: f32, text: &str, primary: bool| {
                let by = y + bh * 0.5 - 16.0 * s;
                let (fill, color) = if primary { (rgba(0x4a, 0x7d, 0xff, 255), rgba(255, 255, 255, 255)) } else { (rgba(0, 0, 0, 14), ink) };
                ui.rects.push(Rect { x: bx, y: by, w: bw, h: 32.0 * s, fill, stroke: 0, stroke_w: 0.0, radius: 6.0 * s, shape: Shape::Rect });
                ui.labels.push(Label { x: bx, y: by + 7.0 * s, w: bw, size: 14.0 * s, color, align: Align::Center, wrap: false, text: text.into() });
            };
            btn(&mut ui, w - 280.0 * s, 130.0 * s, "Восстановить", t.preview.is_some());
            btn(&mut ui, w - 140.0 * s, 116.0 * s, "Закрыть", false);
        }
        self.ed.engine.set_ui(&ui);
    }
}

fn ago(ms: u64) -> String {
    let m = ms / 60_000;
    if m < 1 {
        "только что".into()
    } else if m < 60 {
        format!("{m} мин назад")
    } else if m < 48 * 60 {
        format!("{} ч назад", m / 60)
    } else {
        format!("{} дн назад", m / 1440)
    }
}
