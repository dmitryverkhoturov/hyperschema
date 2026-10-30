//! Scripted input for testing without touching the real mouse:
//! `hyperschema file --script steps.txt` plays one line per frame and
//! writes `steps.txt.log`. Coordinates are window pixels. Run it on a copy:
//! the board saves itself.
//!
//! move X Y | down [alt] [shift] | up | rdown | click X Y | dbl X Y
//! drag X0 Y0 X1 Y1 [alt] [shift] | rdrag X0 Y0 X1 Y1 | dbldrag X0 Y0 X1 Y1
//! type TEXT | key esc|enter|backspace|delete|sdelete|undo|redo|save|group
//! choose N (answer to the next popup menu) | wheel N | fit | cam X Y S | screen
//! timeline open|seek N|restore|close | shot out.png | state | tree | quit
//! (# starts a comment)

use crate::State;
use std::path::PathBuf;
use winit::dpi::PhysicalPosition;
use winit::keyboard::ModifiersState;

pub struct Script {
    lines: Vec<String>,
    pos: usize,
    log: PathBuf,
    out: String,
    /// Expanded steps of a compound command (click, dbl, drag).
    queue: Vec<String>,
    /// `prof`: log how long each pointer move takes.
    prof: bool,
}

impl Script {
    pub fn load(path: &str) -> Script {
        let text = std::fs::read_to_string(path).unwrap_or_default();
        let lines = text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).map(String::from).collect();
        Script { lines, pos: 0, log: PathBuf::from(format!("{path}.log")), out: String::new(), queue: Vec::new(), prof: false }
    }
}

impl State {
    /// Runs the next scripted step. Returns false when the script is over.
    pub fn script_step(&mut self) -> bool {
        let Some(sc) = &mut self.script else { return false };
        let line = if !sc.queue.is_empty() {
            sc.queue.remove(0)
        } else if sc.pos < sc.lines.len() {
            sc.pos += 1;
            sc.lines[sc.pos - 1].clone()
        } else {
            let _ = std::fs::write(&sc.log, &sc.out);
            return false;
        };
        let started = std::time::Instant::now();
        let w: Vec<&str> = line.split_whitespace().collect();
        let n = |i: usize| w.get(i).and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0);
        let has = |k: &str| w.iter().skip(1).any(|x| *x == k);
        let mut note = String::new();
        let mut queue: Vec<String> = Vec::new();
        match w.first().copied().unwrap_or("") {
            "move" => self.move_to(PhysicalPosition::new(n(1), n(2))),
            "down" => {
                if has("shift") {
                    self.mods = ModifiersState::SHIFT;
                }
                self.press(has("alt"), has("shift"))
            }
            "rdown" => self.press_right(false),
            "up" => {
                self.release();
                self.mods = ModifiersState::empty();
            }
            "click" | "dbl" => {
                // Scripted clicks come faster than a double-click; only `dbl` is one.
                self.last_press = None;
                let (x, y) = (n(1), n(2));
                queue = vec![format!("move {x} {y}"), "down".into(), "up".into()];
                if w[0] == "dbl" {
                    queue.extend(["down".to_string(), "up".to_string()]);
                }
            }
            "drag" | "rdrag" | "dbldrag" => {
                self.last_press = None;
                let (x0, y0, x1, y1) = (n(1), n(2), n(3), n(4));
                let mut m = String::new();
                if has("alt") {
                    m.push_str(" alt");
                }
                if has("shift") {
                    m.push_str(" shift");
                }
                queue.push(format!("move {x0} {y0}"));
                match w[0] {
                    "rdrag" => queue.push("rdown".into()),
                    "dbldrag" => queue.extend(["down".to_string(), "up".to_string(), "down".to_string()]),
                    _ => queue.push(format!("down{m}")),
                }
                for k in 1..=8 {
                    let t = k as f64 / 8.0;
                    queue.push(format!("move {} {}", x0 + (x1 - x0) * t, y0 + (y1 - y0) * t));
                }
                queue.push("up".into());
            }
            "type" => {
                let t = line.splitn(2, ' ').nth(1).unwrap_or("").to_string();
                self.type_text(&t);
            }
            "key" => match w.get(1).copied().unwrap_or("") {
                "esc" => {
                    if self.editing_text() {
                        self.commit_text()
                    } else {
                        self.ed.sel.clear()
                    }
                }
                "enter" => self.type_text("\n"),
                "backspace" => self.type_text("\u{8}"),
                "delete" => self.delete_selection(false),
                "sdelete" => self.delete_selection(true),
                "undo" => self.undo(false),
                "redo" => self.undo(true),
                "save" => self.flush(),
                "group" => self.wrap_selection(),
                other => note = format!("unknown key {other}"),
            },
            "choose" => self.script_choice = Some(n(1) as i32),
            "wheel" => {
                let (x, y) = (self.ed.cursor.x, self.ed.cursor.y);
                self.ed.engine.zoom_at(1.18f64.powf(n(1)), x, y);
            }
            "fit" => self.ed.engine.fit(),
            "import" => self.add_board(std::path::Path::new(line.splitn(2, ' ').nth(1).unwrap_or(""))),
            "screen" => {
                // World coordinates = window pixels, for writing tests.
                let (w, h) = self.ed.engine.size();
                self.ed.engine.cam.cx = w as f64 / 2.0;
                self.ed.engine.cam.cy = h as f64 / 2.0;
                self.ed.engine.cam.s = 1.0;
                note = format!("{w}x{h} scale {}", self.window.scale_factor());
            }
            "cam" => {
                self.ed.engine.cam.cx = n(1);
                self.ed.engine.cam.cy = n(2);
                self.ed.engine.cam.s = n(3);
            }
            "timeline" => match w.get(1).copied().unwrap_or("") {
                "open" => self.open_timeline(),
                "seek" => self.timeline_seek(n(2) as usize),
                "restore" => self.timeline_restore(),
                _ => self.close_timeline(),
            },
            "shot" => {
                self.refresh_overlay();
                crate::write_png(&mut self.ed.engine, w.get(1).copied().unwrap_or("shot.png"));
            }
            "state" => note = self.describe(),
            "tree" => note = format!("\n{}", self.tree()),
            "quit" => {
                let sc = self.script.as_mut().unwrap();
                sc.pos = sc.lines.len();
                sc.queue.clear();
            }
            other => note = format!("unknown command {other}"),
        }
        let sc = self.script.as_mut().unwrap();
        for (k, q) in queue.into_iter().enumerate() {
            sc.queue.insert(k, q);
        }
        if sc.prof && w.first() == Some(&"move") {
            sc.out.push_str(&format!("{line}  ({:.1} ms)\n", started.elapsed().as_secs_f64() * 1000.0));
        }
        if w.first() == Some(&"prof") {
            sc.prof = true;
        }
        if !w.is_empty() && !matches!(w[0], "move") {
            sc.out.push_str(&format!("{line}{}{note}\n", if note.is_empty() { "" } else { "  -> " }));
        }
        true
    }

    pub fn harness_shot(&mut self, pre: &str) {
        let k = (1..).find(|k| !std::path::Path::new(&format!("{pre}-{k}.png")).exists()).unwrap();
        self.refresh_overlay();
        crate::write_png(&mut self.ed.engine, &format!("{pre}-{k}.png"));
        let c = &self.ed.engine.cam;
        let info = format!("{}
cam {:.1} {:.1} {:.4} size {:?}
{}", self.describe(), c.cx, c.cy, c.s, self.ed.engine.size(), self.tree());
        let _ = std::fs::write(format!("{pre}-{k}.txt"), info);
    }

    fn label_of(&self, id: &str) -> String {
        self.shown().get(id).map(|n| if n.text.is_empty() { format!("#{id}") } else { n.text.replace('\n', " ") }).unwrap_or_else(|| format!("?{id}"))
    }

    fn describe(&self) -> String {
        let sel: Vec<String> = self.ed.sel.iter().map(|id| self.label_of(id)).collect();
        let edge = self.ed.sel_edge.as_ref().and_then(|e| self.ed.board.edge(e)).map(|e| format!(" edge {}--{}", self.label_of(&e.from), self.label_of(&e.to))).unwrap_or_default();
        format!(
            "nodes {} links {} sel [{}]{edge} editing {} undo {} unsaved {}",
            self.ed.board.nodes.len(),
            self.ed.board.edges.iter().filter(|e| e.kind == hs_core::board::Kind::Link).count(),
            sel.join(", "),
            self.editing_text(),
            self.ed.undo_stack.len(),
            self.unsaved.len()
        )
    }

    /// The nesting tree with boxes, then the visible connections.
    fn tree(&self) -> String {
        let mut s = String::new();
        let b = self.shown();
        for id in b.draw_order() {
            let n = b.node(&id);
            let d = b.depth(&id);
            s.push_str(&format!("{}{} [{:.0},{:.0} {:.0}x{:.0} f{:.1}]\n", "  ".repeat(d + 1), self.label_of(&id), n.x, n.y, n.w, n.h, n.font));
        }
        for e in b.edges.iter().filter(|e| e.kind == hs_core::board::Kind::Link) {
            let arrow = match (e.arrow_from, e.arrow_to) {
                (false, false) => "--",
                (false, true) => "->",
                (true, false) => "<-",
                _ => "<->",
            };
            s.push_str(&format!("  {} {arrow} {}{}\n", self.label_of(&e.from), self.label_of(&e.to), if e.label.is_empty() { String::new() } else { format!(" «{}»", e.label) }));
        }
        s
    }
}
