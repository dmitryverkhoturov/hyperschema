//! Windows glue around the shared editor (`hs_core::editor`): double
//! clicks by the time Windows stamped on the message, redraws, native menus
//! for the editor's requests, and committed steps into history.
//!
//! Gestures (Freeform-like): left-drag on empty canvas pans; right-drag,
//! Shift+drag or double-click-and-drag draws a selection box (on a
//! touchpad: double-tap, hold, drag). A parent is moved by its title strip
//! or border; its empty inside behaves like empty canvas. Dots around the
//! selected or hovered concept drag out connections.

use crate::State;
use hs_core::editor::{Pos, Request};

impl State {
    /// Whether this press is the second of a double click. Uses the time
    /// Windows stamped on the message, so a busy frame can't fake one.
    fn classify_press(&mut self) -> bool {
        use windows::Win32::UI::Input::KeyboardAndMouse::GetDoubleClickTime;
        use windows::Win32::UI::WindowsAndMessaging::{GetMessageTime, GetSystemMetrics, SM_CXDOUBLECLK, SM_CYDOUBLECLK};
        let (now, limit, dx, dy) = unsafe { (GetMessageTime() as u32, GetDoubleClickTime(), GetSystemMetrics(SM_CXDOUBLECLK), GetSystemMetrics(SM_CYDOUBLECLK)) };
        let c = self.ed.cursor;
        let double = self.last_press.is_some_and(|(t, p): (u32, Pos)| now.wrapping_sub(t) <= limit && (p.x - c.x).abs() * 2.0 <= dx as f64 && (p.y - c.y).abs() * 2.0 <= dy as f64);
        self.last_press = if double { None } else { Some((now, c)) };
        double
    }

    pub fn press(&mut self, alt: bool, shift: bool) {
        let double = self.classify_press();
        self.ed.press(alt, shift, double);
        self.settle();
    }

    pub fn press_right(&mut self, shift: bool) {
        self.ed.press_right(shift);
    }

    pub fn update_hover(&mut self) {
        if self.ed.update_hover() {
            self.window.request_redraw();
        }
    }

    pub fn drag_to(&mut self, prev: Pos) -> bool {
        self.ed.shift = self.mods.shift_key();
        self.ed.drag_to(prev)
    }

    pub fn release(&mut self) {
        self.ed.shift = self.mods.shift_key();
        self.ed.release();
        self.settle();
    }

    pub fn refresh_overlay(&mut self) {
        self.ed.refresh_overlay();
        self.refresh_ui();
    }

    pub fn undo(&mut self, redo: bool) {
        self.ed.undo(redo);
        self.settle();
    }

    pub fn commit_text(&mut self) {
        self.ed.commit_text();
        self.settle();
    }

    pub fn delete_selection(&mut self, deep: bool) {
        self.ed.delete_selection(deep);
        self.settle();
    }

    pub fn wrap_selection(&mut self) {
        self.ed.wrap_selection();
        self.settle();
    }

    pub fn type_text(&mut self, s: &str) {
        self.ed.type_text(s);
    }

    /// After an editor call: save committed steps, answer requests.
    pub fn settle(&mut self) {
        for c in std::mem::take(&mut self.ed.done) {
            self.record(c);
        }
        if let Some(Request::EdgeMenu { edge, x, y }) = self.ed.request.take() {
            let Some(e) = self.ed.board.edge(&edge).cloned() else { return };
            let items = vec![
                (1, "Без направления".to_string(), !e.arrow_from && !e.arrow_to),
                (2, "→  Вперёд".to_string(), !e.arrow_from && e.arrow_to),
                (3, "←  Назад".to_string(), e.arrow_from && !e.arrow_to),
                (4, "↔  В обе стороны".to_string(), e.arrow_from && e.arrow_to),
                (0, String::new(), false),
                (5, "Подпись…".to_string(), false),
                (6, "Удалить связь".to_string(), false),
            ];
            let choice = if self.script.is_some() { self.script_choice.take().unwrap_or(0) } else { crate::chrome::popup(x, y, &items) };
            self.ed.apply_edge_choice(&edge, choice);
            for c in std::mem::take(&mut self.ed.done) {
                self.record(c);
            }
        }
    }
}
