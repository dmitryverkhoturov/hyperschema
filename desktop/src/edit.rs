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
use hs_core::board::Shape as NShape;
use hs_core::editor::{Pos, Request, CANVAS_NEW, NODE_COLOR, NODE_DELETE, NODE_DELETE_DEEP, NODE_DUPLICATE, NODE_CLOSE, NODE_EDIT, NODE_PATH, NODE_SHAPE, NODE_WRAP, PALETTE};

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

    /// Ctrl+C / Ctrl+X on the canvas: the selected concepts, with their
    /// text as plain text for other programs.
    pub fn copy(&mut self, cut: bool) {
        let Some(b) = self.ed.copy_selection() else { return };
        crate::clip::set_with(&self.ed.selection_text().unwrap_or_default(), Some(&b.serialize()));
        if cut {
            self.delete_selection(true);
        }
    }

    /// Ctrl+V on the canvas: copied concepts, or a concept from plain text.
    pub fn paste(&mut self) {
        if let Some(b) = crate::clip::get_board().and_then(|j| hs_core::board::Board::parse(&j).ok()) {
            self.ed.paste_board(b);
        } else if let Some(t) = crate::clip::get().filter(|t| !t.trim().is_empty()) {
            self.ed.paste_concept(t.trim());
        }
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
        match self.ed.request.take() {
            Some(Request::NodeMenu { x, y }) => self.node_menu(x, y),
            Some(Request::CanvasMenu { x, y }) => {
                let items = vec![(CANVAS_NEW, "Новый блок здесь".to_string(), false), (1, "Вставить\tCtrl+V".to_string(), false)];
                match self.menu_choice(x, y, &items) {
                    1 => self.paste(),
                    c => self.ed.apply_node_choice(c),
                }
            }
            Some(Request::EdgeMenu { edge, x, y }) => self.edge_menu(edge, x, y),
            None => {}
        }
        for c in std::mem::take(&mut self.ed.done) {
            self.record(c);
        }
        self.save_walks();
        // Test runs keep the user's remembered arrows and layout.
        let real = !crate::dev_run();
        if real && self.ed.layout != self.layout_saved {
            self.layout_saved = self.ed.layout;
            let _ = std::fs::write(crate::data_dir().join("path-layout.txt"), self.ed.layout.key());
        }
        if self.ed.next_arrows != self.arrows_saved && real {
            self.arrows_saved = self.ed.next_arrows;
            let (f, t) = self.arrows_saved;
            let _ = std::fs::write(crate::data_dir().join("arrows.txt"), format!("{}{}", f as u8, t as u8));
        }
    }

    fn menu_choice(&mut self, x: f64, y: f64, items: &[(i32, String, bool)]) -> i32 {
        if self.script.is_some() {
            self.script_choice.take().unwrap_or(0)
        } else {
            crate::chrome::popup(x, y, items)
        }
    }

    fn node_menu(&mut self, x: f64, y: f64) {
        let sel: Vec<_> = self.ed.sel.iter().filter_map(|id| self.ed.board.get(id).cloned()).collect();
        let Some(first) = sel.first() else { return };
        let mut items = vec![(-2, "Цвет".to_string(), false)];
        for (k, (name, c)) in PALETTE.iter().enumerate() {
            items.push((NODE_COLOR + k as i32, name.to_string(), first.fill.as_deref() == *c));
        }
        items.push((-1, String::new(), false));
        items.push((-2, "Форма".to_string(), false));
        let parent = sel.iter().any(|n| self.ed.board.has_children(&n.id));
        for (k, (name, shape)) in [("Прямоугольник", NShape::Box), ("Овал", NShape::Ellipse), ("Ромб", NShape::Diamond), ("Текст без рамки", NShape::Text)].into_iter().enumerate() {
            if shape == NShape::Text && parent {
                continue;
            }
            items.push((NODE_SHAPE + k as i32, name.to_string(), first.shape == shape));
        }
        items.push((-1, String::new(), false));
        items.push((0, String::new(), false));
        if sel.len() == 1 {
            items.push((NODE_EDIT, "Изменить текст\tEnter".to_string(), false));
            if self.ed.path_active() {
                if self.ed.path_closable(&first.id) {
                    items.push((NODE_CLOSE, "Убрать из пути".to_string(), false));
                }
            } else {
                items.push((NODE_PATH, "Путь по связям\tP".to_string(), false));
            }
        }
        items.push((NODE_DUPLICATE, "Дублировать\tCtrl+D".to_string(), false));
        items.push((1, "Копировать\tCtrl+C".to_string(), false));
        items.push((NODE_WRAP, "Обернуть в блок\tCtrl+G".to_string(), false));
        items.push((0, String::new(), false));
        items.push((NODE_DELETE, "Удалить\tDel".to_string(), false));
        if parent {
            items.push((NODE_DELETE_DEEP, "Удалить с содержимым\tShift+Del".to_string(), false));
        }
        match self.menu_choice(x, y, &items) {
            1 => self.copy(false),
            c => self.ed.apply_node_choice(c),
        }
    }

    fn edge_menu(&mut self, edge: String, x: f64, y: f64) {
        {
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
            let choice = self.menu_choice(x, y, &items);
            self.ed.apply_edge_choice(&edge, choice);
        }
    }
}
