//! Window chrome: no system title bar. The canvas runs to the top edge; a
//! transparent strip there holds the menu button, the file name and the
//! three window buttons, which we draw ourselves. Windows still does the
//! work: the strip is a caption (drag, double-click, snap), the buttons
//! answer hit-tests as real min/max/close (so Windows 11 shows snap
//! layouts on the maximise button), and the side borders stay resizable.

use hs_core::scene::{rgba, Align, Label, Line, Rect, Scene, Shape};
use std::sync::atomic::{AtomicI32, AtomicIsize, AtomicU32, Ordering};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_CLOAK};
use windows::Win32::Graphics::Gdi::{InvalidateRect, ScreenToClient};
use windows::Win32::UI::HiDpi::{GetDpiForWindow, GetSystemMetricsForDpi};
use windows::Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TME_NONCLIENT, TRACKMOUSEEVENT};
use windows::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::*;

/// Logical sizes; multiplied by the DPI scale.
pub const STRIP: f64 = 36.0;
pub const BUTTON: f64 = 46.0;

static SCALE_X100: AtomicI32 = AtomicI32::new(100);
static WIDTH: AtomicI32 = AtomicI32::new(0);
/// Hit-test code under the mouse in the strip (HTMINBUTTON...), 0 if none.
static HOVER: AtomicU32 = AtomicU32::new(0);
static PRESSED: AtomicU32 = AtomicU32::new(0);
static HWND_RAW: AtomicIsize = AtomicIsize::new(0);
/// When the last popup menu closed.
static MENU_CLOSED: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);

/// True right after a popup closed: the click that closed it by landing on
/// the menu button must not open it again.
pub fn menu_just_closed() -> bool {
    MENU_CLOSED.lock().ok().and_then(|g| *g).is_some_and(|t| t.elapsed().as_millis() < 250)
}

fn scale() -> f64 {
    SCALE_X100.load(Ordering::Relaxed) as f64 / 100.0
}

pub fn set_metrics(scale: f64, width: u32) {
    SCALE_X100.store((scale * 100.0).round() as i32, Ordering::Relaxed);
    WIDTH.store(width as i32, Ordering::Relaxed);
}

pub fn strip_h() -> f64 {
    STRIP * scale()
}

/// Which of our strip controls is at client point (x, y).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Part {
    Menu,
    Min,
    Max,
    Close,
    Caption,
}

pub fn part_at(x: f64, y: f64) -> Option<Part> {
    let (s, w) = (scale(), WIDTH.load(Ordering::Relaxed) as f64);
    if y < 0.0 || y >= STRIP * s {
        return None;
    }
    let b = BUTTON * s;
    Some(if x < b {
        Part::Menu
    } else if x >= w - b {
        Part::Close
    } else if x >= w - 2.0 * b {
        Part::Max
    } else if x >= w - 3.0 * b {
        Part::Min
    } else {
        Part::Caption
    })
}

fn hwnd() -> HWND {
    HWND(HWND_RAW.load(Ordering::Relaxed) as *mut _)
}

fn frame_y(h: HWND) -> i32 {
    unsafe {
        let dpi = GetDpiForWindow(h);
        GetSystemMetricsForDpi(SM_CYFRAME, dpi) + GetSystemMetricsForDpi(SM_CXPADDEDBORDER, dpi)
    }
}

unsafe extern "system" fn subclass(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM, _id: usize, _data: usize) -> LRESULT {
    match msg {
        WM_NCCALCSIZE if wp.0 != 0 => {
            // Let Windows size the side and bottom borders, then take the
            // caption and top border into the client area.
            let params = &mut *(lp.0 as *mut NCCALCSIZE_PARAMS);
            let top = params.rgrc[0].top;
            let _ = DefSubclassProc(h, msg, wp, lp);
            params.rgrc[0].top = top;
            if IsZoomed(h).as_bool() {
                // A maximised window hangs its frame off-screen.
                params.rgrc[0].top += frame_y(h);
            }
            LRESULT(0)
        }
        WM_NCHITTEST => {
            let r = DefSubclassProc(h, msg, wp, lp);
            if r.0 != HTCLIENT as isize {
                return r;
            }
            let mut p = POINT { x: (lp.0 & 0xffff) as i16 as i32, y: ((lp.0 >> 16) & 0xffff) as i16 as i32 };
            let _ = ScreenToClient(h, &mut p);
            let fy = frame_y(h);
            if !IsZoomed(h).as_bool() && p.y < fy {
                let mut rc = RECT::default();
                let _ = GetClientRect(h, &mut rc);
                let corner = fy * 2;
                return LRESULT(if p.x < corner {
                    HTTOPLEFT
                } else if p.x > rc.right - corner {
                    HTTOPRIGHT
                } else {
                    HTTOP
                } as isize);
            }
            match part_at(p.x as f64, p.y as f64) {
                Some(Part::Min) => LRESULT(HTMINBUTTON as isize),
                Some(Part::Max) => LRESULT(HTMAXBUTTON as isize),
                Some(Part::Close) => LRESULT(HTCLOSE as isize),
                Some(Part::Caption) => LRESULT(HTCAPTION as isize),
                _ => r,
            }
        }
        WM_NCMOUSEMOVE => {
            let code = wp.0 as u32;
            let code = if matches!(code, HTMINBUTTON | HTMAXBUTTON | HTCLOSE) { code } else { 0 };
            if HOVER.swap(code, Ordering::Relaxed) != code {
                let _ = InvalidateRect(Some(h), None, false);
            }
            let mut t = TRACKMOUSEEVENT { cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE | TME_NONCLIENT, hwndTrack: h, dwHoverTime: 0 };
            let _ = TrackMouseEvent(&mut t);
            DefSubclassProc(h, msg, wp, lp)
        }
        WM_NCMOUSELEAVE | WM_MOUSEMOVE => {
            if HOVER.swap(0, Ordering::Relaxed) != 0 {
                PRESSED.store(0, Ordering::Relaxed);
                let _ = InvalidateRect(Some(h), None, false);
            }
            DefSubclassProc(h, msg, wp, lp)
        }
        WM_NCLBUTTONDOWN | WM_NCLBUTTONDBLCLK if matches!(wp.0 as u32, HTMINBUTTON | HTMAXBUTTON | HTCLOSE) => {
            PRESSED.store(wp.0 as u32, Ordering::Relaxed);
            let _ = InvalidateRect(Some(h), None, false);
            LRESULT(0)
        }
        WM_NCLBUTTONUP if matches!(wp.0 as u32, HTMINBUTTON | HTMAXBUTTON | HTCLOSE) => {
            let code = wp.0 as u32;
            if PRESSED.swap(0, Ordering::Relaxed) == code {
                let cmd = match code {
                    HTMINBUTTON => SC_MINIMIZE,
                    HTMAXBUTTON if IsZoomed(h).as_bool() => SC_RESTORE,
                    HTMAXBUTTON => SC_MAXIMIZE,
                    _ => SC_CLOSE,
                };
                let _ = PostMessageW(Some(h), WM_SYSCOMMAND, WPARAM(cmd as usize), LPARAM(0));
            }
            LRESULT(0)
        }
        _ => DefSubclassProc(h, msg, wp, lp),
    }
}

pub fn install(h: HWND) {
    HWND_RAW.store(h.0 as isize, Ordering::Relaxed);
    unsafe {
        let _ = SetWindowSubclass(h, Some(subclass), 1, 0);
        let _ = SetWindowPos(h, None, 0, 0, 0, 0, SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
    }
}

/// Hides the window from the compositor without hiding it from Windows,
/// so it can be shown, maximised and drawn before anyone sees it.
pub fn cloak(on: bool) {
    let v: i32 = on as i32;
    unsafe {
        let _ = DwmSetWindowAttribute(hwnd(), DWMWA_CLOAK, &v as *const i32 as *const _, 4);
    }
}

/// Moves and sizes the window by its outer rectangle.
pub fn place(x: i32, y: i32, w: u32, h: u32) {
    unsafe {
        let _ = SetWindowPos(hwnd(), None, x, y, w as i32, h as i32, SWP_NOZORDER | SWP_NOACTIVATE);
    }
}

pub fn maximized() -> bool {
    unsafe { IsZoomed(hwnd()).as_bool() }
}

/// The strip: menu button, file name, window buttons. `menu_hover` comes
/// from client mouse moves (the menu button is client area).
pub fn draw(ui: &mut Scene, name: &str, menu_hover: bool, ink: u32) {
    let s = scale() as f32;
    let w = WIDTH.load(Ordering::Relaxed) as f32;
    let (h, b) = (STRIP as f32 * s, BUTTON as f32 * s);
    let hover = HOVER.load(Ordering::Relaxed);
    let pressed = PRESSED.load(Ordering::Relaxed);
    let soft = rgba(0, 0, 0, 14);
    let line = |ui: &mut Scene, x0: f32, y0: f32, x1: f32, y1: f32, c: u32| ui.lines.push(Line { x0, y0, x1, y1, color: c, width: 1.0 * s, head: false });
    let bg = |ui: &mut Scene, x: f32, c: u32| ui.rects.push(Rect { x, y: 0.0, w: b, h, fill: c, stroke: 0, stroke_w: 0.0, radius: 0.0, shape: Shape::Rect });

    // Menu: three bars.
    if menu_hover {
        bg(ui, 0.0, soft);
    }
    let (cx, cy) = (b * 0.5, h * 0.5);
    for k in [-1.0f32, 0.0, 1.0] {
        line(ui, cx - 7.0 * s, cy + k * 5.0 * s, cx + 7.0 * s, cy + k * 5.0 * s, ink);
    }
    // File name.
    ui.labels.push(Label { x: b + 4.0 * s, y: cy - 9.0 * s, w: (w - 4.0 * b).max(0.0), size: 14.0 * s, color: rgba(0x55, 0x55, 0x55, 255), align: Align::Left, wrap: false, text: name.to_string() });

    // Min, max/restore, close.
    let codes = [(HTMINBUTTON, w - 3.0 * b), (HTMAXBUTTON, w - 2.0 * b), (HTCLOSE, w - b)];
    for (code, x) in codes {
        let on = hover == code || pressed == code;
        let mut c = ink;
        if on {
            if code == HTCLOSE {
                bg(ui, x, rgba(0xe8, 0x11, 0x23, 255));
                c = rgba(255, 255, 255, 255);
            } else {
                bg(ui, x, if pressed == code { rgba(0, 0, 0, 30) } else { soft });
            }
        }
        let (mx, my) = (x + b * 0.5, h * 0.5);
        let r = 5.0 * s;
        match code {
            HTMINBUTTON => line(ui, mx - r, my, mx + r, my, c),
            HTMAXBUTTON if maximized() => {
                let o = 2.0 * s;
                ui.rects.push(Rect { x: mx - r, y: my - r + o, w: 2.0 * r - o, h: 2.0 * r - o, fill: 0, stroke: c, stroke_w: s, radius: 0.0, shape: Shape::Rect });
                line(ui, mx - r + o, my - r, mx + r, my - r, c);
                line(ui, mx + r, my - r, mx + r, my + r - o, c);
            }
            HTMAXBUTTON => ui.rects.push(Rect { x: mx - r, y: my - r, w: 2.0 * r, h: 2.0 * r, fill: 0, stroke: c, stroke_w: s, radius: 0.0, shape: Shape::Rect }),
            _ => {
                line(ui, mx - r, my - r, mx + r, my + r, c);
                line(ui, mx - r, my + r, mx + r, my - r, c);
            }
        }
    }
}

/// A native popup menu at client point (x, y); returns the chosen id or 0.
/// Items: (id, text); id 0 = separator; a negative id starts a submenu
/// whose items follow until an item with id -1.
pub fn popup(x: f64, y: f64, items: &[(i32, String, bool)]) -> i32 {
    unsafe {
        let Ok(root) = CreatePopupMenu() else { return 0 };
        let mut stack = vec![root];
        let mut pending: Vec<(String, HMENU)> = Vec::new();
        for (id, text, checked) in items {
            let menu = *stack.last().unwrap();
            let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
            let pcw = windows::core::PCWSTR(wide.as_ptr());
            match *id {
                0 => {
                    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
                }
                -1 => {
                    if let (Some(sub), Some((t, _))) = (stack.pop(), pending.pop()) {
                        let w2: Vec<u16> = t.encode_utf16().chain(std::iter::once(0)).collect();
                        let parent = *stack.last().unwrap();
                        let _ = AppendMenuW(parent, MF_POPUP, sub.0 as usize, windows::core::PCWSTR(w2.as_ptr()));
                    }
                }
                i if i < -1 => {
                    if let Ok(sub) = CreatePopupMenu() {
                        pending.push((text.clone(), sub));
                        stack.push(sub);
                    }
                }
                i => {
                    let flags = if *checked { MF_STRING | MF_CHECKED } else { MF_STRING };
                    let _ = AppendMenuW(menu, flags, i as usize, pcw);
                }
            }
        }
        let mut p = POINT { x: x as i32, y: y as i32 };
        let _ = windows::Win32::Graphics::Gdi::ClientToScreen(hwnd(), &mut p);
        // Without the window in front a click outside does not close the
        // menu; the posted no-op lets the menu loop notice it is done.
        let _ = SetForegroundWindow(hwnd());
        let r = TrackPopupMenu(root, TPM_RETURNCMD | TPM_LEFTALIGN | TPM_TOPALIGN | TPM_RIGHTBUTTON, p.x, p.y, Some(0), hwnd(), None);
        let _ = PostMessageW(Some(hwnd()), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(root);
        if let Ok(mut g) = MENU_CLOSED.lock() {
            *g = Some(std::time::Instant::now());
        }
        r.0
    }
}
