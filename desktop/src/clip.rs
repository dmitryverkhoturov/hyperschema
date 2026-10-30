//! The Windows clipboard, plain text only.

use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL};
use windows::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, RegisterClipboardFormatW, SetClipboardData};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::System::Ole::CF_UNICODETEXT;

/// Our own format: copied concepts as board JSON, next to the plain text.
fn board_format() -> u32 {
    unsafe { RegisterClipboardFormatW(windows::core::w!("Hyperschema.Board")) }
}

pub fn get() -> Option<String> {
    read(CF_UNICODETEXT.0 as u32)
}

/// Copied concepts, if the clipboard holds ours.
pub fn get_board() -> Option<String> {
    read(board_format())
}

fn read(format: u32) -> Option<String> {
    unsafe {
        OpenClipboard(None).ok()?;
        let out = (|| {
            let h = GetClipboardData(format).ok()?;
            let g = HGLOBAL(h.0);
            let p = GlobalLock(g) as *const u16;
            if p.is_null() {
                return None;
            }
            let max = GlobalSize(g) / 2;
            let mut n = 0;
            while n < max && *p.add(n) != 0 {
                n += 1;
            }
            let s = String::from_utf16_lossy(std::slice::from_raw_parts(p, n));
            let _ = GlobalUnlock(g);
            Some(s.replace("\r\n", "\n"))
        })();
        let _ = CloseClipboard();
        out
    }
}

pub fn set(text: &str) {
    set_with(text, None);
}

/// Plain text, plus copied concepts in our own format.
pub fn set_with(text: &str, board: Option<&str>) {
    unsafe {
        if OpenClipboard(None).is_err() {
            return;
        }
        let _ = EmptyClipboard();
        put(CF_UNICODETEXT.0 as u32, &text.replace('\n', "\r\n"));
        if let Some(b) = board {
            put(board_format(), b);
        }
        let _ = CloseClipboard();
    }
}

unsafe fn put(format: u32, text: &str) {
    let wide: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
    unsafe {
        if let Ok(g) = GlobalAlloc(GMEM_MOVEABLE, wide.len() * 2) {
            let p = GlobalLock(g) as *mut u16;
            if !p.is_null() {
                std::ptr::copy_nonoverlapping(wide.as_ptr(), p, wide.len());
                let _ = GlobalUnlock(g);
                if SetClipboardData(format, Some(HANDLE(g.0))).is_err() {
                    let _ = GlobalFree(Some(g));
                }
            }
        }
    }
}
