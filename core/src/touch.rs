//! Finger gestures for any touch screen (Android, a Windows touch screen),
//! driving the shared editor. Modelled on the circular calendar:
//!
//! - one finger on empty canvas pans, with a short glide after a throw
//!   (speed from the last ~90 ms, so a finger that stopped throws nothing);
//! - one finger on a concept moves it, on a handle resizes, on a dot drags
//!   out a connection — only once it moved past the touch slop, so a tap
//!   never nudges anything;
//! - two fingers: the points under them stay pinned (pan + zoom);
//! - tap selects (tap on empty deselects); tap on the selected concept
//!   edits its text;
//! - double tap zooms in ×2 (animated); double tap, hold and drag zooms with
//!   one finger (180 dp = ×2, down = closer); two-finger tap zooms out ×2;
//! - long press and drag draws a selection box; a long press released in
//!   place makes a new concept on empty canvas or edits the one under it.

use crate::editor::{Editor, Hit, Mode, Pos, Request};

#[derive(Clone, Copy, PartialEq, Debug)]
enum TMode {
    Idle,
    /// One finger down, not yet moved past the slop.
    Pending { grab: bool },
    /// Moving something through the editor (concept, handle, dot, box).
    Editor,
    Pan,
    /// Second touch of a double tap, not yet moved: a zoom tap or a drag-zoom.
    QsPending,
    Qs,
    /// Long press fired, finger still in place.
    Held,
    Multi,
}

pub struct Touch {
    density: f64,
    slop: f64,
    mode: TMode,
    fingers: Vec<(u64, f64, f64)>,
    down_t: u64,
    down: (f64, f64),
    last: (f64, f64),
    max_move: f64,
    multi_seen: bool,
    last_tap: Option<(u64, f64, f64)>,
    /// Two-finger tap candidate: (time, midpoint, span).
    two_tap: Option<(u64, (f64, f64), f64)>,
    samples: Vec<(u64, f64, f64)>,
    cum: (f64, f64),
    vel: (f64, f64),
    /// Remaining zoom of an animated zoom (ln), and its focus.
    zoom_left: f64,
    zoom_at: (f64, f64),
    last_tick: Option<u64>,
}

const DOUBLE_TAP_MS: u64 = 300;
const TAP_MAX_MS: u64 = 280;
const LONG_MS: u64 = 450;
const FRICTION: f64 = 7.0;
const ZOOM_RATE: f64 = 18.0;

impl Touch {
    pub fn new(density: f64) -> Touch {
        Touch {
            density,
            slop: 8.0 * density,
            mode: TMode::Idle,
            fingers: Vec::new(),
            down_t: 0,
            down: (0.0, 0.0),
            last: (0.0, 0.0),
            max_move: 0.0,
            multi_seen: false,
            last_tap: None,
            two_tap: None,
            samples: Vec::new(),
            cum: (0.0, 0.0),
            vel: (0.0, 0.0),
            zoom_left: 0.0,
            zoom_at: (0.0, 0.0),
            last_tick: None,
        }
    }

    fn mid_span(&self) -> ((f64, f64), f64) {
        let n = self.fingers.len().max(1) as f64;
        let cx = self.fingers.iter().map(|f| f.1).sum::<f64>() / n;
        let cy = self.fingers.iter().map(|f| f.2).sum::<f64>() / n;
        let span = self.fingers.iter().map(|f| (f.1 - cx).hypot(f.2 - cy)).sum::<f64>() / n;
        ((cx, cy), span)
    }

    fn sample(&mut self, t: u64) {
        self.samples.push((t, self.cum.0, self.cum.1));
        if self.samples.len() > 32 {
            self.samples.remove(0);
        }
    }

    fn reset_samples(&mut self, t: u64) {
        self.samples.clear();
        self.cum = (0.0, 0.0);
        self.sample(t);
    }

    /// Throw speed at `t` over the last ~90 ms (px/s).
    fn velocity(&self, t: u64) -> (f64, f64) {
        let Some(&(tn, xn, yn)) = self.samples.last() else { return (0.0, 0.0) };
        let Some(&(to, xo, yo)) = self.samples.iter().find(|s| t.saturating_sub(s.0) <= 90) else { return (0.0, 0.0) };
        if to == tn {
            return (0.0, 0.0);
        }
        let dt = ((t - to) as f64 / 1000.0).max(0.016);
        ((xn - xo) / dt, (yn - yo) / dt)
    }

    fn at(ed: &mut Editor, x: f64, y: f64) {
        ed.cursor = Pos { x, y };
    }

    /// Stops gliding and animated zoom (a new touch catches the canvas).
    fn stop(&mut self) {
        self.vel = (0.0, 0.0);
        self.zoom_left = 0.0;
    }

    pub fn down(&mut self, ed: &mut Editor, id: u64, x: f64, y: f64, t: u64) {
        ed.touch = true;
        if self.fingers.is_empty() {
            self.stop();
            self.fingers.push((id, x, y));
            self.down_t = t;
            self.down = (x, y);
            self.last = (x, y);
            self.max_move = 0.0;
            self.multi_seen = false;
            self.two_tap = None;
            self.reset_samples(t);
            let second = self.last_tap.is_some_and(|(lt, lx, ly)| t - lt <= DOUBLE_TAP_MS && (x - lx).hypot(y - ly) < 40.0 * self.density);
            if second {
                self.mode = TMode::QsPending;
                return;
            }
            self.last_tap = None;
            Self::at(ed, x, y);
            ed.update_hover();
            let grab = !matches!(ed.hit(), Hit::Empty | Hit::Inside(_)) && !ed.frozen && matches!(ed.mode, Mode::Idle | Mode::Text { .. });
            self.mode = TMode::Pending { grab };
            return;
        }
        self.fingers.push((id, x, y));
        self.multi_seen = true;
        if self.mode == TMode::Editor {
            // A second finger ends what the first was doing and takes the canvas.
            ed.release();
        }
        let (m, span) = self.mid_span();
        self.two_tap = (self.fingers.len() == 2 && t - self.down_t < 250 && self.max_move < self.slop).then_some((t, m, span));
        self.mode = TMode::Multi;
        self.reset_samples(t);
    }

    pub fn moved(&mut self, ed: &mut Editor, id: u64, x: f64, y: f64, t: u64) {
        let Some(k) = self.fingers.iter().position(|f| f.0 == id) else { return };
        if self.mode == TMode::Multi && self.fingers.len() >= 2 {
            let (m0, s0) = self.mid_span();
            self.fingers[k] = (id, x, y);
            let (m1, s1) = self.mid_span();
            if let Some((_, tm, ts)) = self.two_tap {
                if (m1.0 - tm.0).hypot(m1.1 - tm.1) > self.slop || (s1 - ts).abs() > self.slop {
                    self.two_tap = None;
                }
            }
            ed.engine.pan(m1.0 - m0.0, m1.1 - m0.1);
            if s0 > 1.0 && s1 > 1.0 {
                ed.engine.zoom_at(s1 / s0, m1.0, m1.1);
            }
            return;
        }
        self.fingers[k] = (id, x, y);
        self.max_move = self.max_move.max((x - self.down.0).hypot(y - self.down.1));
        let prev = Pos { x: self.last.0, y: self.last.1 };
        let (dx, dy) = (x - self.last.0, y - self.last.1);
        self.last = (x, y);
        match self.mode {
            TMode::QsPending => {
                if self.max_move >= self.slop {
                    self.mode = TMode::Qs;
                }
            }
            TMode::Qs => {
                let k = (2f64).ln() / (180.0 * self.density);
                ed.engine.zoom_at((dy * k).exp(), self.down.0, self.down.1);
            }
            TMode::Pending { grab } => {
                if self.max_move < self.slop {
                    return;
                }
                if grab {
                    Self::at(ed, self.down.0, self.down.1);
                    ed.press(false, false, false);
                    Self::at(ed, x, y);
                    ed.drag_to(Pos { x: self.down.0, y: self.down.1 });
                    self.mode = TMode::Editor;
                } else {
                    self.mode = TMode::Pan;
                    ed.engine.pan(x - self.down.0, y - self.down.1);
                    self.cum.0 += x - self.down.0;
                    self.cum.1 += y - self.down.1;
                    self.sample(t);
                }
            }
            TMode::Held => {
                if self.max_move < self.slop {
                    return;
                }
                // Long press, then drag: a selection box from where it started.
                Self::at(ed, self.down.0, self.down.1);
                ed.press_right(false);
                Self::at(ed, x, y);
                ed.drag_to(prev);
                self.mode = TMode::Editor;
            }
            TMode::Editor => {
                Self::at(ed, x, y);
                ed.drag_to(prev);
            }
            TMode::Pan => {
                ed.engine.pan(dx, dy);
                self.cum.0 += dx;
                self.cum.1 += dy;
                self.sample(t);
            }
            TMode::Multi | TMode::Idle => {}
        }
    }

    pub fn up(&mut self, ed: &mut Editor, id: u64, x: f64, y: f64, t: u64) {
        let Some(k) = self.fingers.iter().position(|f| f.0 == id) else { return };
        self.fingers.remove(k);
        if !self.fingers.is_empty() {
            if let Some((tt, m, _)) = self.two_tap.take() {
                if self.fingers.len() == 1 && t - tt < 350 {
                    // Two-finger tap: zoom out.
                    self.zoom_left = -(2f64).ln();
                    self.zoom_at = m;
                    self.mode = TMode::Idle;
                    self.fingers.clear();
                    self.last_tap = None;
                    return;
                }
            }
            if self.mode == TMode::Multi && self.fingers.len() == 1 {
                // Back to one finger: it pans, without a jump.
                self.mode = TMode::Pan;
                self.last = (self.fingers[0].1, self.fingers[0].2);
                self.reset_samples(t);
            }
            return;
        }
        let tap = !self.multi_seen && self.max_move < self.slop && t - self.down_t < TAP_MAX_MS;
        match self.mode {
            TMode::QsPending if tap => {
                self.zoom_left = (2f64).ln();
                self.zoom_at = self.down;
                self.last_tap = None;
            }
            TMode::Pending { .. } if tap => {
                Self::at(ed, x, y);
                let before: Vec<String> = ed.sel.clone();
                let hit = ed.hit();
                ed.press(false, false, false);
                ed.release();
                match hit {
                    Hit::Node(id) if before == [id.clone()] && ed.sel == [id.clone()] => ed.edit_selected(),
                    // A finger can't aim at the middle handle: a tapped
                    // connection offers its choices straight away.
                    Hit::Edge(e) if ed.request.is_none() => ed.request = Some(Request::EdgeMenu { edge: e, x, y }),
                    _ => {}
                }
                self.last_tap = Some((t, x, y));
            }
            TMode::Held => {
                // Long press released in place: new concept, or edit this one.
                Self::at(ed, self.down.0, self.down.1);
                match ed.hit() {
                    Hit::Empty | Hit::Inside(_) => {
                        ed.press(false, false, true);
                        ed.release();
                    }
                    Hit::Node(_) => {
                        ed.press(false, false, false);
                        ed.release();
                        ed.edit_selected();
                    }
                    _ => {}
                }
            }
            TMode::Editor => {
                Self::at(ed, x, y);
                ed.release();
            }
            TMode::Pan => {
                let (vx, vy) = self.velocity(t);
                let s = vx.hypot(vy);
                let cap = 30000.0;
                self.vel = if s < 60.0 * self.density { (0.0, 0.0) } else if s > cap { (vx * cap / s, vy * cap / s) } else { (vx, vy) };
                self.last_tick = Some(t);
            }
            _ => {}
        }
        self.mode = TMode::Idle;
    }

    pub fn cancel(&mut self, ed: &mut Editor) {
        if self.mode == TMode::Editor {
            ed.release();
        }
        self.fingers.clear();
        self.mode = TMode::Idle;
        self.stop();
    }

    /// Advances long press, glide and animated zoom. Returns true while it
    /// needs more frames.
    pub fn tick(&mut self, ed: &mut Editor, t: u64) -> bool {
        if let TMode::Pending { .. } = self.mode {
            if t >= self.down_t + LONG_MS && self.max_move < self.slop {
                self.mode = TMode::Held;
            }
        }
        let dt = self.last_tick.map_or(0.016, |l| ((t.saturating_sub(l)) as f64 / 1000.0).clamp(0.0, 0.05));
        self.last_tick = Some(t);
        let mut busy = matches!(self.mode, TMode::Pending { .. });
        if self.vel != (0.0, 0.0) {
            ed.engine.pan(self.vel.0 * dt, self.vel.1 * dt);
            let k = (-FRICTION * dt).exp();
            self.vel = (self.vel.0 * k, self.vel.1 * k);
            if self.vel.0.hypot(self.vel.1) < 20.0 * self.density {
                self.vel = (0.0, 0.0);
            }
            busy = true;
        }
        if self.zoom_left != 0.0 {
            let step = if self.zoom_left.abs() < 0.002 { self.zoom_left } else { self.zoom_left * (1.0 - (-ZOOM_RATE * dt).exp()) };
            ed.engine.zoom_at(step.exp(), self.zoom_at.0, self.zoom_at.1);
            self.zoom_left -= step;
            busy = true;
        }
        busy
    }

    /// A long press just fired (the shell may give a haptic tick).
    pub fn held(&self) -> bool {
        self.mode == TMode::Held
    }

    pub fn active(&self) -> bool {
        !self.fingers.is_empty()
    }
}
