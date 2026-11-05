//! Boards on this device: `<dir>/<id>.am` (the Automerge document) and
//! `<dir>/<id>.json` (name, what the server already has, the device's
//! position in the change feed). The server copy is in couch.rs; `Link`
//! talks to it on background threads so the window never waits.

use crate::board::{Board, Change};
use crate::couch::{Config, Couch};
use crate::sync::Shared;
use automerge::{ActorId, ChangeHash};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::Duration;

pub struct Library {
    pub dir: PathBuf,
    pub actor: ActorId,
}

fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

/// HS_SYNC_LOG=file: what sync does, for debugging.
pub fn slog(msg: impl AsRef<str>) {
    if let Some(p) = std::env::var_os("HS_SYNC_LOG") {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(p) {
            let _ = writeln!(f, "{} {}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0), msg.as_ref());
        }
    }
}

pub fn new_id() -> String {
    const A: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let a = ActorId::random();
    a.to_bytes().iter().take(12).map(|b| A[*b as usize % A.len()] as char).collect()
}

impl Library {
    /// `actor` is this device's identity in every board: one per app
    /// install, kept in `<dir>/actor`. Pass `fresh_actor` for one-off
    /// writers (the CLI), which must not share the app's.
    pub fn open(dir: PathBuf, fresh_actor: bool) -> Library {
        let _ = std::fs::create_dir_all(&dir);
        let actor = if fresh_actor {
            ActorId::random()
        } else {
            let f = dir.join("actor");
            match std::fs::read_to_string(&f).ok().and_then(|s| hex(s.trim())) {
                Some(b) => ActorId::from(b),
                None => {
                    let a = ActorId::random();
                    let _ = std::fs::write(&f, a.to_hex_string());
                    a
                }
            }
        };
        Library { dir, actor }
    }

    fn am(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.am"))
    }
    fn meta_path(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.json"))
    }

    /// Boards on this device: (id, name), by name.
    pub fn list(&self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        if let Ok(rd) = std::fs::read_dir(&self.dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.extension().is_some_and(|x| x == "am") {
                    let id = p.file_stem().unwrap().to_string_lossy().to_string();
                    let name = self.meta(&id)["name"].as_str().unwrap_or(&id).to_string();
                    out.push((id, name));
                }
            }
        }
        out.sort_by(|a, b| a.1.to_lowercase().cmp(&b.1.to_lowercase()));
        out
    }

    pub fn meta(&self, id: &str) -> Value {
        std::fs::read_to_string(self.meta_path(id)).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(json!({}))
    }

    pub fn set_meta(&self, id: &str, m: &Value) {
        let _ = write_atomic(&self.meta_path(id), m.to_string().as_bytes());
    }

    pub fn exists(&self, id: &str) -> bool {
        self.am(id).exists()
    }

    /// A new board holding `board`; returns its id.
    pub fn create(&self, name: &str, board: &Board) -> std::io::Result<String> {
        let id = new_id();
        let mut s = Shared::from_board(board, Some(self.actor.clone()));
        write_atomic(&self.am(&id), &s.save())?;
        self.set_meta(&id, &json!({ "name": name, "pushed": [] }));
        Ok(id)
    }

    /// A board known only to the server so far: an empty document that the
    /// first pull fills.
    pub fn adopt(&self, id: &str, name: &str) -> std::io::Result<()> {
        let mut s = Shared::new(Some(self.actor.clone()));
        write_atomic(&self.am(id), &s.save())?;
        self.set_meta(id, &json!({ "name": name, "pushed": [] }));
        Ok(())
    }

    pub fn load(&self, id: &str) -> Result<Live, String> {
        let bytes = std::fs::read(self.am(id)).map_err(|e| e.to_string())?;
        let shared = Shared::load(&bytes, Some(self.actor.clone())).map_err(|e| e.to_string())?;
        let meta = self.meta(id);
        let pushed = meta["pushed"].as_array().into_iter().flatten().filter_map(|h| h.as_str()?.parse().ok()).collect();
        Ok(Live { id: id.to_string(), name: meta["name"].as_str().unwrap_or(id).to_string(), path: self.am(id), meta_path: self.meta_path(id), shared, pushed, in_flight: None, dirty: false, link: None })
    }

    pub fn rename(&self, id: &str, name: &str) {
        let mut m = self.meta(id);
        m["name"] = json!(name);
        self.set_meta(id, &m);
    }

    /// Moves a board's files into `<dir>/deleted` (never erased here).
    pub fn remove(&self, id: &str) {
        let bin = self.dir.join("deleted");
        let _ = std::fs::create_dir_all(&bin);
        let _ = std::fs::rename(self.am(id), bin.join(format!("{id}.am")));
        let _ = std::fs::rename(self.meta_path(id), bin.join(format!("{id}.json")));
    }
}

fn hex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 || s.is_empty() {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok()).collect()
}

/// What the background threads report.
pub enum Event {
    /// Changes from another device.
    Remote(Vec<u8>),
    /// The server took everything up to these heads.
    Pushed(Vec<ChangeHash>),
    /// Connection state for the title strip: None = all good.
    Status(Option<String>),
}

pub struct Link {
    push_tx: Sender<(Vec<u8>, Vec<ChangeHash>)>,
    rx: Receiver<Event>,
}

/// An open board.
pub struct Live {
    pub id: String,
    pub name: String,
    pub path: PathBuf,
    meta_path: PathBuf,
    pub shared: Shared,
    /// Heads the server is known to have.
    pushed: Vec<ChangeHash>,
    in_flight: Option<Vec<ChangeHash>>,
    /// Document changed since the last save to disk.
    dirty: bool,
    link: Option<Link>,
}

impl Live {
    pub fn board(&self) -> Board {
        self.shared.board()
    }

    /// Writes local steps into the document.
    pub fn record(&mut self, steps: &[Change]) {
        slog(format!("record {} steps, link {}", steps.len(), self.link.is_some()));
        for c in steps {
            self.shared.record(c);
        }
        if !steps.is_empty() {
            self.dirty = true;
            self.push();
        }
    }

    /// Saves the document if it changed. Cheap enough to call after every
    /// batch of steps or remote changes.
    pub fn save(&mut self) -> std::io::Result<()> {
        if !self.dirty {
            return Ok(());
        }
        write_atomic(&self.path, &self.shared.save())?;
        self.dirty = false;
        Ok(())
    }

    fn save_meta(&self) {
        let mut m: Value = std::fs::read_to_string(&self.meta_path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(json!({}));
        m["name"] = json!(self.name);
        m["pushed"] = json!(self.pushed.iter().map(|h| h.to_string()).collect::<Vec<_>>());
        let _ = write_atomic(&self.meta_path, m.to_string().as_bytes());
    }

    /// Starts talking to the server: first everything it has, then the live
    /// feed. `wake` is called from a background thread when an event waits
    /// in `poll`.
    pub fn connect(&mut self, cfg: Config, wake: impl Fn() + Send + Sync + 'static) {
        let wake = std::sync::Arc::new(wake);
        let (tx, rx) = channel();
        let (push_tx, push_rx) = channel::<(Vec<u8>, Vec<ChangeHash>)>();
        let board = self.id.clone();
        let name = self.name.clone();
        let actor = self.shared.doc.get_actor().to_hex_string();

        // Pusher: sends batches in order, retrying until the server takes each.
        {
            let tx = tx.clone();
            let wake = wake.clone();
            let cfg = cfg.clone();
            let board = board.clone();
            std::thread::spawn(move || {
                let mut couch = Couch::new(cfg);
                let mut carded = false;
                while let Ok((bytes, heads)) = push_rx.recv() {
                    slog(format!("push {} bytes", bytes.len()));
                    let mut delay = 2;
                    loop {
                        let r = if carded { Ok(()) } else { couch.put_card(&board, &name) };
                        let r = r.and_then(|_| couch.push(&board, &actor, &bytes));
                        match r {
                            Ok(()) => {
                                carded = true;
                                let _ = tx.send(Event::Pushed(heads));
                                wake();
                                break;
                            }
                            Err(e) => {
                                slog(format!("push failed: {e}"));
                                let _ = tx.send(Event::Status(Some(e)));
                                wake();
                                std::thread::sleep(Duration::from_secs(delay));
                                delay = (delay * 2).min(60);
                            }
                        }
                    }
                }
            });
        }

        // Puller: everything once, then the live feed.
        std::thread::spawn(move || {
            let couch = Couch::new(cfg);
            let mut since: Option<Value> = None;
            let mut was_down = false;
            loop {
                let step = (|| -> Result<(), String> {
                    if since.is_none() {
                        let seq = couch.seq()?;
                        for b in couch.pull_all(&board)? {
                            tx.send(Event::Remote(b)).map_err(|_| "closed".to_string())?;
                        }
                        since = Some(seq);
                        wake();
                    }
                    let (got, last) = couch.wait(&board, since.as_ref().unwrap(), Duration::from_secs(50))?;
                    for b in got {
                        tx.send(Event::Remote(b)).map_err(|_| "closed".to_string())?;
                    }
                    since = Some(last);
                    Ok(())
                })();
                match step {
                    Ok(()) => {
                        if was_down {
                            was_down = false;
                            let _ = tx.send(Event::Status(None));
                        }
                        wake();
                    }
                    Err(e) if e == "closed" => return,
                    Err(e) => {
                        was_down = true;
                        let _ = tx.send(Event::Status(Some(e)));
                        wake();
                        std::thread::sleep(Duration::from_secs(5));
                    }
                }
            }
        });
        self.link = Some(Link { push_tx, rx });
        self.push();
    }

    pub fn connected(&self) -> bool {
        self.link.is_some()
    }

    /// Sends what the server lacks, one batch at a time.
    fn push(&mut self) {
        let Some(link) = &self.link else { return };
        if self.in_flight.is_some() {
            return;
        }
        let heads = self.shared.heads();
        if heads == self.pushed {
            return;
        }
        // A board nobody has drawn on yet stays on this device.
        if self.pushed.is_empty() && self.shared.board().nodes.is_empty() {
            return;
        }
        let bytes = self.shared.save_after(&self.pushed);
        slog(format!("queue {} bytes", bytes.len()));
        if bytes.is_empty() {
            self.pushed = heads;
            return;
        }
        if link.push_tx.send((bytes, heads.clone())).is_ok() {
            self.in_flight = Some(heads);
        }
    }

    /// Handles what the background threads reported. Returns (anything
    /// remote merged, connection status if it changed).
    pub fn poll(&mut self) -> (bool, Option<Option<String>>) {
        let mut merged = false;
        let mut status = None;
        let events: Vec<Event> = match &self.link {
            Some(l) => l.rx.try_iter().collect(),
            None => return (false, None),
        };
        for ev in events {
            match ev {
                Event::Remote(b) => {
                    slog(format!("remote {} bytes", b.len()));
                    // What came from the server is on the server: if nothing of
                    // ours was waiting, there is nothing to send back.
                    let clean = self.in_flight.is_none() && self.shared.heads() == self.pushed;
                    if self.shared.merge_bytes(&b).unwrap_or(false) {
                        merged = true;
                        self.dirty = true;
                        if clean {
                            self.pushed = self.shared.heads();
                        }
                    }
                }
                Event::Pushed(h) => {
                    slog("pushed");
                    self.pushed = h;
                    self.in_flight = None;
                    self.save_meta();
                    status = Some(None);
                }
                Event::Status(s) => status = Some(s),
            }
        }
        if merged || self.in_flight.is_none() {
            self.push();
        }
        (merged, status)
    }

    /// True while local changes have not reached the server.
    pub fn unsent(&mut self) -> bool {
        self.shared.heads() != self.pushed
    }
}

/// Merges everything the server has for `id` into the local copy and sends
/// what it lacks, synchronously (for the CLI).
pub fn sync_now(lib: &Library, id: &str, cfg: &Config) -> Result<Live, String> {
    let couch = Couch::new(cfg.clone());
    let mut live = lib.load(id)?;
    for b in couch.pull_all(id)? {
        let clean = live.shared.heads() == live.pushed;
        if live.shared.merge_bytes(&b).map_err(|e| e.to_string())? {
            live.dirty = true;
            if clean {
                live.pushed = live.shared.heads();
            }
        }
    }
    live.save().map_err(|e| e.to_string())?;
    live.save_meta();
    Ok(live)
}

/// Sends local changes of `live` that the server lacks (for the CLI).
pub fn push_now(live: &mut Live, cfg: &Config) -> Result<(), String> {
    let heads = live.shared.heads();
    if heads == live.pushed {
        return Ok(());
    }
    let bytes = live.shared.save_after(&live.pushed);
    let mut couch = Couch::new(cfg.clone());
    couch.put_card(&live.id, &live.name)?;
    couch.push(&live.id, &live.shared.doc.get_actor().to_hex_string(), &bytes)?;
    live.pushed = heads;
    live.save_meta();
    Ok(())
}
