//! Editing on top of the viewer: select, drag, create boxes, type labels,
//! draw arrows, undo, save back to the file.
//!
//! While something is being dragged or typed, those elements are left out
//! of the indexed scene and drawn in the overlay instead, so a frame never
//! rebuilds the whole scene.

use crate::State;
use hs_core::doc::Doc;
use hs_core::scene::{rgba, Line, Rect, Scene, Shape};
use serde_json::Value;
use std::path::Path;
use std::time::{Duration, Instant, SystemTime};
use winit::dpi::PhysicalPosition;

const BLUE: (u8, u8, u8) = (0x4a, 0x7d, 0xff);

pub enum Mode {
    Idle,
    Pan,
    /// Pressed on a selected element; becomes a drag after a few pixels.
    Move { start: (f64, f64), press: PhysicalPosition<f64>, saved: Vec<(usize, Value)>, set: Vec<usize>, moved: bool },
    /// Alt+drag: an arrow from `p0`, bound to `from` if it started on a shape.
    Arrow { p0: (f32, f32), from: Option<usize> },
    /// `created`: the label is new; `new_box`: so is its box (double-click on empty space).
    Text { idx: usize, text: String, created: bool, new_box: Option<usize> },
}

pub enum Click {
    Single,
    Double,
}

impl State {
    pub fn world(&self) -> (f32, f32) {
        let (x, y) = self.engine.screen_to_world(self.cursor.x, self.cursor.y);
        (x as f32, y as f32)
    }

    fn tol(&self) -> f32 {
        (6.0 / self.engine.cam.s) as f32
    }

    /// Rebuilds the indexed scene from the document, minus what the overlay draws.
    pub fn rebuild(&mut self) {
        if let Some(doc) = &self.doc {
            let scene = doc.scene(&self.hidden);
            self.engine.set_scene(scene);
        }
        self.update_title();
    }

    pub fn update_title(&mut self) {
        let dirty = self.doc.as_ref().is_some_and(|d| d.dirty);
        if dirty != self.shown_dirty {
            self.shown_dirty = dirty;
            self.set_title();
        }
    }

    fn hide(&mut self, set: impl IntoIterator<Item = usize>) {
        self.hidden = set.into_iter().collect();
        self.rebuild();
    }

    /// Selection outlines, live elements, caret and arrow preview.
    pub fn refresh_overlay(&mut self) {
        let mut ov = Scene::default();
        let Some(doc) = &self.doc else {
            self.engine.set_overlay(&ov);
            return;
        };
        let mut live: Vec<usize> = self.hidden.iter().copied().collect();
        live.sort_unstable();
        ov = doc.scene_of(&live);
        let px = (1.0 / self.engine.cam.s) as f32;
        let blue = rgba(BLUE.0, BLUE.1, BLUE.2, 255);
        for &i in &self.sel {
            if !doc.live(i) {
                continue;
            }
            if doc.is_linear(i) {
                for w in doc.points(i).windows(2) {
                    ov.lines.push(Line { x0: w[0].0, y0: w[0].1, x1: w[1].0, y1: w[1].1, color: rgba(BLUE.0, BLUE.1, BLUE.2, 110), width: 6.0 * px, head: false });
                }
                continue;
            }
            let b = doc.bbox(i);
            let pad = 4.0 * px;
            ov.rects.push(Rect {
                x: b[0] - pad,
                y: b[1] - pad,
                w: b[2] - b[0] + 2.0 * pad,
                h: b[3] - b[1] + 2.0 * pad,
                fill: 0,
                stroke: blue,
                stroke_w: 1.5 * px,
                radius: 0.0,
                shape: Shape::Rect,
            });
        }
        match &self.mode {
            Mode::Text { idx, text, .. } => {
                let e = &doc.els[*idx];
                let size = doc.font_size(*idx);
                let last = text.rsplit('\n').next().unwrap_or("");
                let (lw, _) = self.engine.measure(last, size);
                let lines = text.split('\n').count() as f32;
                let (x, y, w) = (e["x"].as_f64().unwrap_or(0.0) as f32, e["y"].as_f64().unwrap_or(0.0) as f32, e["width"].as_f64().unwrap_or(0.0) as f32);
                let x0 = match e["textAlign"].as_str() {
                    Some("center") => x + (w - lw) * 0.5,
                    Some("right") => x + w - lw,
                    _ => x,
                };
                let blink = (self.caret_epoch.elapsed().as_millis() / 530) % 2 == 0;
                if blink {
                    ov.rects.push(Rect {
                        x: x0 + lw + px,
                        y: y + (lines - 1.0) * 1.25 * size,
                        w: (1.5 * px).max(size * 0.06),
                        h: 1.25 * size,
                        fill: blue,
                        stroke: 0,
                        stroke_w: 0.0,
                        radius: 0.0,
                        shape: Shape::Rect,
                    });
                }
            }
            Mode::Arrow { p0, .. } => {
                let (x1, y1) = self.world();
                ov.lines.push(Line { x0: p0.0, y0: p0.1, x1, y1, color: blue, width: 2.0, head: true });
            }
            _ => {}
        }
        self.engine.set_overlay(&ov);
    }

    /// True while the overlay changes by itself (caret blink).
    pub fn animating(&self) -> bool {
        matches!(self.mode, Mode::Text { .. })
    }

    pub fn classify_press(&mut self) -> Click {
        let now = Instant::now();
        let double = self.last_press.is_some_and(|(t, p)| {
            now - t < Duration::from_millis(450) && (p.x - self.cursor.x).abs() < 6.0 && (p.y - self.cursor.y).abs() < 6.0
        });
        self.last_press = if double { None } else { Some((now, self.cursor)) };
        if double {
            Click::Double
        } else {
            Click::Single
        }
    }

    pub fn press(&mut self, alt: bool, shift: bool) {
        let click = self.classify_press();
        crate::dlog(format!("press {:?} alt={alt} double={}", self.cursor, matches!(click, Click::Double)));
        self.commit_text();
        let Some(doc) = &self.doc else {
            self.mode = Mode::Pan;
            return;
        };
        let p = self.world();
        let hit = doc.hit(p.0, p.1, self.tol());
        if let Click::Double = click {
            self.double_click(hit, p);
            return;
        }
        if alt {
            let from = hit.filter(|&i| !doc.is_linear(i));
            self.mode = Mode::Arrow { p0: from.map_or(p, |f| doc.center(f)), from };
            return;
        }
        match hit {
            Some(i) => {
                let unit = doc.unit(i);
                if shift {
                    if self.sel.contains(&i) {
                        self.sel.retain(|s| !unit.contains(s));
                    } else {
                        self.sel.extend(unit);
                    }
                } else if !self.sel.contains(&i) {
                    self.sel = unit;
                }
                let w = self.engine.screen_to_world(self.cursor.x, self.cursor.y);
                self.mode = Mode::Move { start: w, press: self.cursor, saved: Vec::new(), set: Vec::new(), moved: false };
            }
            None => {
                if !shift {
                    self.sel.clear();
                }
                self.mode = Mode::Pan;
            }
        }
    }

    fn double_click(&mut self, hit: Option<usize>, p: (f32, f32)) {
        let Some(doc) = &mut self.doc else { return };
        doc.checkpoint();
        let (idx, created, new_box) = match hit {
            Some(i) if doc.kind(i) == "text" => (i, false, None),
            Some(i) if !doc.is_linear(i) && !matches!(doc.kind(i), "frame" | "magicframe") => {
                let had = doc.bound_text(i).is_some();
                (doc.ensure_text(i), !had, None)
            }
            Some(_) => return,
            None => {
                let (b, t) = doc.add_box(p.0, p.1);
                (t, true, Some(b))
            }
        };
        self.start_text(idx, created, new_box);
    }

    /// Enter / F2: edit the label of the single selected element.
    pub fn edit_selected(&mut self) {
        let Some(doc) = &self.doc else { return };
        let [i] = self.sel[..] else { return };
        if doc.is_linear(i) || matches!(doc.kind(i), "frame" | "magicframe") {
            return;
        }
        let p = doc.center(i);
        let _ = doc;
        self.double_click(Some(i), p);
    }

    fn start_text(&mut self, idx: usize, created: bool, new_box: Option<usize>) {
        crate::dlog(format!("start_text {idx} created={created} box={new_box:?}"));
        let Some(doc) = &self.doc else { return };
        let text = doc.text(idx);
        let mut set = vec![idx];
        if let Some(c) = doc.container(idx) {
            set.push(c);
            set.extend(doc.attached_arrows(&[c]));
            self.sel = vec![c];
        } else {
            self.sel = vec![idx];
        }
        self.caret_epoch = Instant::now();
        self.mode = Mode::Text { idx, text, created, new_box };
        self.hide(set);
    }

    pub fn type_text(&mut self, s: &str) {
        crate::dlog(format!("type {s:?}"));
        let Mode::Text { idx, text, .. } = &mut self.mode else { return };
        for c in s.chars() {
            match c {
                '\u{8}' => {
                    text.pop();
                }
                '\r' | '\n' => text.push('\n'),
                c if !c.is_control() => text.push(c),
                _ => {}
            }
        }
        let (idx, text) = (*idx, text.clone());
        self.apply_text(idx, &text);
        self.caret_epoch = Instant::now();
    }

    fn apply_text(&mut self, idx: usize, text: &str) {
        let Some(doc) = &mut self.doc else { return };
        let size = doc.font_size(idx);
        let mut m = self.engine.measure(text, size);
        m.0 = m.0.max(1.0);
        doc.set_text(idx, text, m);
    }

    pub fn editing_text(&self) -> bool {
        matches!(self.mode, Mode::Text { .. })
    }

    /// Ends text editing; an empty new label is removed with its box.
    pub fn commit_text(&mut self) {
        let Mode::Text { idx, text, created, new_box } = std::mem::replace(&mut self.mode, Mode::Idle) else { return };
        if let Some(doc) = &mut self.doc {
            if text.trim().is_empty() && created {
                let gone: Vec<usize> = std::iter::once(idx).chain(new_box).collect();
                doc.delete(&gone);
                self.sel.clear();
            }
        }
        self.hide([]);
    }

    pub fn drag_to(&mut self) -> bool {
        match &mut self.mode {
            Mode::Pan => false,
            Mode::Arrow { .. } => true,
            Mode::Move { start, press, saved, set, moved } => {
                let Some(doc) = &mut self.doc else { return false };
                if !*moved {
                    if (press.x - self.cursor.x).hypot(press.y - self.cursor.y) < 4.0 {
                        return false;
                    }
                    doc.checkpoint();
                    *set = doc.closure(&self.sel);
                    let mut all = set.clone();
                    all.extend(doc.attached_arrows(set));
                    *saved = all.iter().map(|&i| (i, doc.els[i].clone())).collect();
                    *moved = true;
                    let hidden: Vec<usize> = all;
                    let (start, set, saved) = (*start, set.clone(), saved.clone());
                    self.hidden = hidden.into_iter().collect();
                    self.rebuild();
                    self.mode = Mode::Move { start, press: self.cursor, saved, set, moved: true };
                    return self.drag_to();
                }
                for (i, v) in saved.iter() {
                    doc.els[*i] = v.clone();
                }
                let w = self.engine.screen_to_world(self.cursor.x, self.cursor.y);
                let (dx, dy) = ((w.0 - start.0) as f32, (w.1 - start.1) as f32);
                doc.move_by(set, dx, dy);
                true
            }
            _ => false,
        }
    }

    pub fn panning(&self) -> bool {
        matches!(self.mode, Mode::Pan)
    }

    pub fn release(&mut self) {
        match std::mem::replace(&mut self.mode, Mode::Idle) {
            Mode::Move { moved: true, .. } => self.hide([]),
            Mode::Arrow { p0, from } => {
                let (p1, tol) = (self.world(), self.tol());
                let Some(doc) = &mut self.doc else { return };
                if (p1.0 - p0.0).hypot(p1.1 - p0.1) > 3.0 * tol {
                    let to = doc.hit(p1.0, p1.1, tol).filter(|&i| !doc.is_linear(i));
                    doc.checkpoint();
                    let a = doc.add_arrow(p0, p1, from, to);
                    self.sel = vec![a];
                    self.rebuild();
                }
            }
            m @ Mode::Text { .. } => self.mode = m,
            _ => {}
        }
    }

    pub fn delete_selection(&mut self) {
        let Some(doc) = &mut self.doc else { return };
        if self.sel.is_empty() {
            return;
        }
        doc.checkpoint();
        let set = doc.closure(&self.sel);
        doc.delete(&set);
        self.sel.clear();
        self.rebuild();
    }

    pub fn undo(&mut self, redo: bool) {
        self.commit_text();
        let Some(doc) = &mut self.doc else { return };
        let done = if redo { doc.redo() } else { doc.undo() };
        if done {
            self.sel.clear();
            self.rebuild();
        }
    }

    // ------------------------------------------------------------ files

    pub fn load_doc(&mut self, path: &Path) -> Result<(), String> {
        let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        let doc = Doc::parse(&text)?;
        self.mode = Mode::Idle;
        self.sel.clear();
        self.hidden.clear();
        self.mtime = std::fs::metadata(path).and_then(|m| m.modified()).ok();
        self.path = Some(path.to_path_buf());
        self.doc = Some(doc);
        self.shown_dirty = false;
        self.rebuild();
        Ok(())
    }

    /// Asks about unsaved changes. Returns false if the user cancelled.
    pub fn confirm_discard(&mut self) -> bool {
        self.commit_text();
        if !self.doc.as_ref().is_some_and(|d| d.dirty) {
            return true;
        }
        let r = rfd::MessageDialog::new()
            .set_title("Hyperschema")
            .set_description(format!("Сохранить изменения в «{}»?", self.name))
            .set_buttons(rfd::MessageButtons::YesNoCancel)
            .set_parent(&*self.window)
            .show();
        match r {
            rfd::MessageDialogResult::Yes => self.save(),
            rfd::MessageDialogResult::No => true,
            _ => false,
        }
    }

    pub fn save(&mut self) -> bool {
        self.commit_text();
        let (Some(doc), Some(path)) = (&mut self.doc, self.path.clone()) else { return false };
        let disk = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
        if disk.is_some() && disk != self.mtime {
            let r = rfd::MessageDialog::new()
                .set_title("Hyperschema")
                .set_description("Файл изменился на диске после открытия (например, в Obsidian). Перезаписать его своей версией?")
                .set_buttons(rfd::MessageButtons::YesNo)
                .set_level(rfd::MessageLevel::Warning)
                .set_parent(&*self.window)
                .show();
            if r != rfd::MessageDialogResult::Yes {
                return false;
            }
        }
        let text = doc.serialize();
        if let Err(e) = write_with_backup(&path, &text) {
            rfd::MessageDialog::new()
                .set_title("Hyperschema")
                .set_description(format!("Не сохранилось: {e}"))
                .set_level(rfd::MessageLevel::Error)
                .set_parent(&*self.window)
                .show();
            return false;
        }
        doc.saved_as(text);
        self.mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
        self.update_title();
        true
    }
}

/// Copies the current file into the backup folder, then replaces it atomically.
fn write_with_backup(path: &Path, text: &str) -> std::io::Result<()> {
    let dir = crate::data_dir().join("backups");
    std::fs::create_dir_all(&dir)?;
    let stamp = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    if path.exists() {
        std::fs::copy(path, dir.join(format!("{stamp}-{name}")))?;
    }
    // Keep the newest 50 backups.
    let mut old: Vec<_> = std::fs::read_dir(&dir)?.filter_map(|e| e.ok()).map(|e| e.path()).collect();
    old.sort();
    for p in old.iter().rev().skip(50) {
        let _ = std::fs::remove_file(p);
    }
    let tmp = path.with_extension("hs-tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}
