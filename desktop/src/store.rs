//! Files: our own `.hyperschema` board, saved by itself half a second after
//! each finished action (no Save button, no question on close), with its
//! history log next to it. Excalidraw drawings are imported once into a
//! board beside them and never written to.

use hs_core::editor::Mode;
use crate::State;
use hs_core::board::{Board, Change};
use hs_core::history::{hash, History};
use hs_core::scene::{rgba, Align, Label, Line, Rect, Scene, Shape};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

pub const EXT: &str = "hyperschema";
const SAVE_DELAY: Duration = Duration::from_millis(500);

pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
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

/// A board path next to `p` that does not exist yet.
fn fresh_path(dir: &Path, name: &str) -> PathBuf {
    let mut k = 1;
    loop {
        let n = if k == 1 { format!("{name}.{EXT}") } else { format!("{name} ({k}).{EXT}") };
        let p = dir.join(n);
        if !p.exists() {
            return p;
        }
        k += 1;
    }
}

fn mtime(p: &Path) -> Option<SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    let tmp = path.with_extension("hs-tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

/// Keeps a copy of a file someone else changed before we overwrite it.
fn backup(path: &Path) {
    let dir = crate::data_dir().join("backups");
    let _ = std::fs::create_dir_all(&dir);
    let stamp = now_ms() / 1000;
    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let _ = std::fs::copy(path, dir.join(format!("{stamp}-{name}")));
    let mut old: Vec<_> = std::fs::read_dir(&dir).map(|r| r.filter_map(|e| e.ok()).map(|e| e.path()).collect()).unwrap_or_default();
    old.sort();
    for p in old.iter().rev().skip(50) {
        let _ = std::fs::remove_file(p);
    }
}

pub fn recent() -> Vec<PathBuf> {
    std::fs::read_to_string(crate::data_dir().join("recent.txt")).unwrap_or_default().lines().map(PathBuf::from).filter(|p| p.exists()).collect()
}

fn push_recent(p: &Path) {
    let mut r = recent();
    r.retain(|x| x != p);
    r.insert(0, p.to_path_buf());
    r.truncate(10);
    let text: Vec<String> = r.iter().map(|p| p.to_string_lossy().to_string()).collect();
    let _ = std::fs::create_dir_all(crate::data_dir());
    let _ = std::fs::write(crate::data_dir().join("recent.txt"), text.join("\n"));
}

/// Browsing history: every device's steps merged, newest last.
pub struct Timeline {
    pub steps: Vec<Change>,
    /// How many steps are applied: steps.len() is "now".
    pub pos: usize,
    pub preview: Option<Board>,
    pub dragging: bool,
}

impl State {
    /// Opens a board, or imports an Excalidraw drawing into a board beside it.
    pub fn open(&mut self, path: &Path) {
        self.commit_text();
        self.flush();
        let result = if is_excalidraw(path) {
            let target = path.with_file_name(format!("{}.{EXT}", stem(path)));
            if target.exists() {
                self.load(&target)
            } else {
                self.import(path, target)
            }
        } else {
            self.load(path)
        };
        if let Err(e) = result {
            self.name = format!("не открылось: {e}");
        }
        self.window.request_redraw();
    }

    fn import(&mut self, src: &Path, target: PathBuf) -> Result<(), String> {
        let text = std::fs::read_to_string(src).map_err(|e| e.to_string())?;
        let (board, report) = hs_core::import::from_excalidraw(&text)?;
        crate::dlog(format!("imported {} nodes, {} edges, dropped {} arrows", board.nodes.len(), board.edges.len(), report.dropped_arrows));
        self.create(&target, board)
    }

    /// Writes a new board file (with its birth as the first history step) and opens it.
    fn create(&mut self, target: &Path, board: Board) -> Result<(), String> {
        let text = board.serialize();
        write_atomic(target, &text).map_err(|e| e.to_string())?;
        let mut birth = board.diff(&Board::new().snapshot()).unwrap_or(Change { t: 0, dev: String::new(), nodes: vec![], edges: vec![] });
        birth.t = now_ms();
        let _ = History::for_file(target).append(&birth, hash(&text));
        self.load(target)
    }

    fn load(&mut self, path: &Path) -> Result<(), String> {
        let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        let board = Board::parse(&text)?;
        let history = History::for_file(path);
        let _ = history.compact(now_ms());
        self.ed.undo_stack = history.recent_undo(&text, now_ms());
        self.ed.redo_stack.clear();
        self.unsaved.clear();
        self.save_at = None;
        self.ed.step_base = None;
        self.history = Some(history);
        self.ed.board = board;
        self.ed.mode = Mode::Idle;
        self.ed.sel.clear();
        self.ed.sel_edge = None;
        self.ed.hover = None;
        self.timeline = None;
        self.ed.frozen = false;
        self.ed.preview = None;
        self.ed.hidden_nodes.clear();
        self.ed.hidden_edges.clear();
        self.mtime = mtime(path);
        self.path = Some(path.to_path_buf());
        self.name = stem(path);
        self.window.set_title(&self.name);
        if self.script.is_none() && crate::harness().is_none() {
            crate::remember(path);
            push_recent(path);
        }
        self.rebuild();
        self.ed.engine.fit();
        Ok(())
    }

    /// Queues a finished step for saving.
    pub fn record(&mut self, mut c: Change) {
        if c.t == 0 {
            c.t = now_ms();
        }
        self.unsaved.push(c);
        self.save_at = Some(Instant::now() + SAVE_DELAY);
    }

    /// Saves now if a save is due and nothing is mid-gesture.
    pub fn maybe_save(&mut self) {
        if self.save_at.is_some_and(|t| Instant::now() >= t) && matches!(self.ed.mode, Mode::Idle) {
            self.flush();
        }
    }

    /// Writes the board and appends pending steps to the history.
    pub fn flush(&mut self) {
        self.save_at = None;
        if self.unsaved.is_empty() {
            return;
        }
        let Some(path) = self.path.clone() else { return };
        let text = self.ed.board.serialize();
        if path.exists() && mtime(&path) != self.mtime {
            backup(&path);
        }
        if let Err(e) = write_atomic(&path, &text) {
            crate::dlog(format!("save failed: {e}"));
            self.name = format!("{} — не сохраняется: {e}", stem(&path));
            self.save_at = Some(Instant::now() + Duration::from_secs(5));
            return;
        }
        self.mtime = mtime(&path);
        let h = hash(&text);
        if let Some(hist) = &self.history {
            for c in self.unsaved.drain(..) {
                let _ = hist.append(&c, h);
            }
        }
        self.unsaved.clear();
    }

    /// When the window comes back: pick up a file changed elsewhere (sync).
    pub fn check_external(&mut self) {
        let Some(path) = self.path.clone() else { return };
        if !self.unsaved.is_empty() || !matches!(self.ed.mode, Mode::Idle) || mtime(&path) == self.mtime {
            return;
        }
        let cam = self.ed.engine.cam;
        if self.load(&path).is_ok() {
            self.ed.engine.cam = cam;
        }
    }

    pub fn new_board(&mut self) {
        let dir = self.path.as_ref().and_then(|p| p.parent().map(Path::to_path_buf)).unwrap_or_default();
        let pick = rfd::FileDialog::new().set_directory(&dir).set_file_name(format!("Новая схема.{EXT}")).add_filter("Hyperschema", &[EXT]).set_title("Новая схема").save_file();
        if let Some(p) = pick {
            self.flush();
            if let Err(e) = self.create(&p, Board::new()) {
                self.name = format!("не создалось: {e}");
            }
        }
    }

    pub fn open_dialog(&mut self) {
        let pick = rfd::FileDialog::new().add_filter("Схемы", &[EXT, "excalidraw", "md"]).set_title("Открыть схему").pick_file();
        if let Some(p) = pick {
            self.open(&p);
        }
    }

    /// Import again on demand, into a new board so nothing is overwritten.
    pub fn import_dialog(&mut self) {
        let pick = rfd::FileDialog::new().add_filter("Excalidraw", &["excalidraw", "md", "json"]).set_title("Импорт из Excalidraw").pick_file();
        if let Some(src) = pick {
            self.flush();
            let target = fresh_path(src.parent().unwrap_or(Path::new(".")), &stem(&src));
            if let Err(e) = self.import(&src, target) {
                self.name = format!("не импортировалось: {e}");
            }
        }
    }

    pub fn reveal(&self) {
        if let Some(p) = &self.path {
            let _ = std::process::Command::new("explorer").arg(format!("/select,{}", p.display())).spawn();
        }
    }

    pub fn main_menu(&mut self) {
        let h = crate::chrome::strip_h();
        let mut items: Vec<(i32, String, bool)> = vec![(1, "Новая схема…".into(), false), (2, "Открыть…".into(), false)];
        let rec = recent();
        if !rec.is_empty() {
            items.push((-2, "Недавние".into(), false));
            for (i, p) in rec.iter().enumerate() {
                items.push((100 + i as i32, stem(p), Some(p) == self.path.as_ref()));
            }
            items.push((-1, String::new(), false));
        }
        items.extend([(3, "Импорт из Excalidraw…".into(), false), (0, String::new(), false), (4, "Показать в проводнике".into(), false), (5, "История…".into(), false)]);
        match crate::chrome::popup(0.0, h, &items) {
            1 => self.new_board(),
            2 => self.open_dialog(),
            3 => self.import_dialog(),
            4 => self.reveal(),
            5 => self.open_timeline(),
            i if i >= 100 => {
                if let Some(p) = rec.get((i - 100) as usize) {
                    let p = p.clone();
                    self.open(&p);
                }
            }
            _ => {}
        }
    }

    // ------------------------------------------------------------ timeline

    pub fn open_timeline(&mut self) {
        self.commit_text();
        self.flush();
        let steps = self.history.as_ref().map(|h| h.load_all()).unwrap_or_default();
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
            let mut b = self.ed.board.clone();
            for c in t.steps[pos..].iter().rev() {
                b.apply(c, false);
            }
            t.preview = Some(b);
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
        crate::chrome::draw(&mut ui, &self.name, menu_hover, ink);
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
                let at = t.steps.get(t.pos).map_or(0, |c| c.t);
                ago(now_ms().saturating_sub(at))
            };
            let dev = if t.pos < t.steps.len() { t.steps.get(t.pos).map(|c| format!(" · {}", c.dev)).unwrap_or_default() } else { String::new() };
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
