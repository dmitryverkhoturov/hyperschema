//! The time animations run on: the real clock, or, while a video is
//! rendered frame by frame, a virtual one that moves a frame at a time.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

static VIRTUAL: AtomicBool = AtomicBool::new(false);
static BASE: OnceLock<Instant> = OnceLock::new();
static NANOS: AtomicU64 = AtomicU64::new(0);

pub fn now() -> Instant {
    if VIRTUAL.load(Ordering::Relaxed) {
        *BASE.get_or_init(Instant::now) + Duration::from_nanos(NANOS.load(Ordering::Relaxed))
    } else {
        Instant::now()
    }
}

/// Time since `t` on this clock.
pub fn since(t: Instant) -> Duration {
    now().saturating_duration_since(t)
}

/// From now on time moves only by `advance`.
pub fn start_virtual() {
    BASE.get_or_init(Instant::now);
    let real = Instant::now().saturating_duration_since(*BASE.get().unwrap());
    NANOS.store(real.as_nanos() as u64, Ordering::Relaxed);
    VIRTUAL.store(true, Ordering::Relaxed);
}

pub fn advance(d: Duration) {
    NANOS.fetch_add(d.as_nanos() as u64, Ordering::Relaxed);
}
