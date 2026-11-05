//! Interface language: English unless the system is in Russian. The shell
//! sets it once at start (Windows: the user's UI language; Android: the
//! device locale).

use std::sync::atomic::{AtomicBool, Ordering};

static RUSSIAN: AtomicBool = AtomicBool::new(false);

pub fn set_russian(on: bool) {
    RUSSIAN.store(on, Ordering::Relaxed);
}

pub fn russian() -> bool {
    RUSSIAN.load(Ordering::Relaxed)
}

/// The text in the interface language.
pub fn tr(en: &'static str, ru: &'static str) -> &'static str {
    if russian() {
        ru
    } else {
        en
    }
}
