//! Scripted input for testing without touching the real mouse:
//! `hyperschema file --script steps.txt` plays one line per frame and
//! writes `steps.txt.log`. Coordinates are window pixels.
//!
//! move X Y | down [alt] [shift] | up | click X Y | dbl X Y | drag X0 Y0 X1 Y1 [alt]
//! type TEXT | key esc|enter|backspace|delete|undo|redo|save | wheel N | fit
//! shot out.png | state | quit        (# starts a comment)

use crate::State;
use std::path::PathBuf;
use winit::dpi::PhysicalPosition;

pub struct Script {
    lines: Vec<String>,
    pos: usize,
    log: PathBuf,
    out: String,
    /// Expanded steps of a compound command (click, dbl, drag).
    queue: Vec<String>,
}

impl Script {
    pub fn load(path: &str) -> Script {
        let text = std::fs::read_to_string(path).unwrap_or_default();
        let lines = text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).map(String::from).collect();
        Script { lines, pos: 0, log: PathBuf::from(format!("{path}.log")), out: String::new(), queue: Vec::new() }
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
        let w: Vec<&str> = line.split_whitespace().collect();
        let n = |i: usize| w.get(i).and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0);
        let has = |k: &str| w.iter().skip(1).any(|x| *x == k);
        let mut note = String::new();
        match w.first().copied().unwrap_or("") {
            "move" => self.move_to(PhysicalPosition::new(n(1), n(2))),
            "down" => self.press(has("alt"), has("shift")),
            "up" => self.release(),
            "click" | "dbl" => {
                let (x, y) = (n(1), n(2));
                let mut q = vec![format!("move {x} {y}"), "down".into(), "up".into()];
                if w[0] == "dbl" {
                    q.extend(["down".to_string(), "up".to_string()]);
                }
                self.script.as_mut().unwrap().queue.extend(q);
            }
            "drag" => {
                let (x0, y0, x1, y1) = (n(1), n(2), n(3), n(4));
                let m = if has("alt") { " alt" } else { "" };
                let mut q = vec![format!("move {x0} {y0}"), format!("down{m}")];
                for k in 1..=8 {
                    let t = k as f64 / 8.0;
                    q.push(format!("move {} {}", x0 + (x1 - x0) * t, y0 + (y1 - y0) * t));
                }
                q.push("up".into());
                self.script.as_mut().unwrap().queue.extend(q);
            }
            "type" => {
                let t = line.splitn(2, ' ').nth(1).unwrap_or("").to_string();
                self.type_text(&t);
            }
            "key" => match w.get(1).copied().unwrap_or("") {
                "esc" => self.commit_text(),
                "enter" => self.type_text("\n"),
                "backspace" => self.type_text("\u{8}"),
                "delete" => self.delete_selection(),
                "undo" => self.undo(false),
                "redo" => self.undo(true),
                "save" => note = format!("saved={}", self.save()),
                other => note = format!("unknown key {other}"),
            },
            "wheel" => {
                let (x, y) = (self.cursor.x, self.cursor.y);
                self.engine.zoom_at(1.18f64.powf(n(1)), x, y);
            }
            "fit" => self.engine.fit(),
            "shot" => {
                self.refresh_overlay();
                crate::write_png(&mut self.engine, w.get(1).copied().unwrap_or("shot.png"));
            }
            "state" => note = self.describe(),
            "quit" => {
                let sc = self.script.as_mut().unwrap();
                sc.pos = sc.lines.len();
                sc.queue.clear();
            }
            other => note = format!("unknown command {other}"),
        }
        let sc = self.script.as_mut().unwrap();
        if !w.is_empty() && !matches!(w[0], "move") {
            sc.out.push_str(&format!("{line}{}{note}\n", if note.is_empty() { "" } else { "  -> " }));
        }
        true
    }

    fn describe(&self) -> String {
        let Some(doc) = &self.doc else { return "no document".into() };
        let live = (0..doc.els.len()).filter(|&i| doc.live(i)).count();
        let sel: Vec<String> = self
            .sel
            .iter()
            .map(|&i| {
                let b = doc.bbox(i);
                let t = doc.bound_text(i).map(|t| doc.text(t)).unwrap_or_else(|| if doc.kind(i) == "text" { doc.text(i) } else { String::new() });
                format!("{}#{i}[{:.0},{:.0} {:.0}x{:.0}]{t:?}", doc.kind(i), b[0], b[1], b[2] - b[0], b[3] - b[1])
            })
            .collect();
        format!("elements {} live {live} dirty {} editing {} sel [{}]", doc.els.len(), doc.dirty, self.editing_text(), sel.join(", "))
    }
}
