//! `--record out.mp4`: renders a demo video frame by frame. Time runs on
//! the virtual clock (hs_core::clock), one 1/60 s step per frame, so the
//! video is smooth whatever the machine manages; frames are drawn off-screen
//! at a fixed size and piped to ffmpeg as raw RGBA.

use std::io::Write;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::time::Duration;

pub const W: u32 = 1920;
pub const H: u32 = 1080;
pub const FPS: u32 = 60;

pub struct Recorder {
    child: Child,
    stdin: Option<ChildStdin>,
    pub frames: u64,
}

impl Recorder {
    pub fn start(out: &str) -> std::io::Result<Recorder> {
        let mut cmd = Command::new("ffmpeg");
        cmd.args(["-y", "-loglevel", "error", "-f", "rawvideo", "-pix_fmt", "rgba", "-s", &format!("{W}x{H}"), "-r", &FPS.to_string(), "-i", "-"])
            .args(["-c:v", "libx264", "-preset", "medium", "-crf", "16", "-pix_fmt", "yuv420p", "-movflags", "+faststart", out])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        let mut child = cmd.spawn()?;
        let stdin = child.stdin.take();
        hs_core::clock::start_virtual();
        Ok(Recorder { child, stdin, frames: 0 })
    }

    /// Moves the clock to the next frame's time.
    pub fn tick(&self) {
        hs_core::clock::advance(Duration::from_nanos(1_000_000_000 / FPS as u64));
    }

    pub fn push(&mut self, rgba: &[u8]) {
        if let Some(s) = &mut self.stdin {
            if s.write_all(rgba).is_err() {
                self.stdin = None;
            }
        }
        self.frames += 1;
    }

    pub fn finish(mut self) {
        drop(self.stdin.take());
        let _ = self.child.wait();
    }
}
