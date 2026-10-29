//! Durable history next to the board: `<name>.history/<device>.log`, one
//! JSON change per line. One file per device, so sync never conflicts;
//! the timeline reads them all merged by time.
//!
//! Old history thins out: every step for the last day, then one point per
//! hour for a week, per day for a month, per week for a year, per month
//! beyond. Dropping a point merges the steps around it into one, so
//! walking back still lands on real states. The first step stays.

use crate::board::Change;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};

const HOUR: u64 = 3_600_000;
const DAY: u64 = 24 * HOUR;

pub struct History {
    pub dir: PathBuf,
    pub dev: String,
}

/// FNV-1a: ties the tail of the log to the exact file it produced.
pub fn hash(text: &str) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for b in text.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    h
}

pub fn device_name() -> String {
    let raw = std::env::var("COMPUTERNAME").or_else(|_| std::env::var("HOSTNAME")).unwrap_or_else(|_| "device".into());
    raw.chars().map(|c| if c.is_alphanumeric() || c == '-' { c } else { '_' }).collect::<String>().to_lowercase()
}

impl History {
    pub fn for_file(board_file: &Path) -> History {
        let stem = board_file.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        History { dir: board_file.with_file_name(format!("{stem}.history")), dev: device_name() }
    }

    fn own(&self) -> PathBuf {
        self.dir.join(format!("{}.log", self.dev))
    }

    /// Appends one step; `file_hash` is the hash of the board file it led to.
    pub fn append(&self, c: &Change, file_hash: u64) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let mut v = c.to_json();
        v["dev"] = json!(self.dev);
        v["h"] = json!(format!("{file_hash:016x}"));
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(self.own())?;
        writeln!(f, "{v}")
    }

    fn read(path: &Path) -> Vec<(Change, Option<String>)> {
        let text = std::fs::read_to_string(path).unwrap_or_default();
        text.lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter_map(|v| Some((Change::from_json(&v)?, v["h"].as_str().map(String::from))))
            .collect()
    }

    /// Steps of this device, oldest first, with the file hash each produced.
    pub fn load_own(&self) -> Vec<(Change, Option<String>)> {
        Self::read(&self.own())
    }

    /// Steps of every device, oldest first.
    pub fn load_all(&self) -> Vec<Change> {
        let mut all = Vec::new();
        if let Ok(rd) = std::fs::read_dir(&self.dir) {
            for e in rd.flatten() {
                if e.path().extension().is_some_and(|x| x == "log") {
                    all.extend(Self::read(&e.path()).into_iter().map(|x| x.0));
                }
            }
        }
        all.sort_by_key(|c| c.t);
        all
    }

    /// The last day of this device's steps, if the log ends at exactly the
    /// file on disk — the undo stack that survives a restart.
    pub fn recent_undo(&self, file_text: &str, now: u64) -> Vec<Change> {
        let own = self.load_own();
        let want = format!("{:016x}", hash(file_text));
        if own.last().and_then(|x| x.1.as_ref()) != Some(&want) {
            return Vec::new();
        }
        let mut out: Vec<Change> = own.into_iter().rev().map(|x| x.0).take_while(|c| now.saturating_sub(c.t) < DAY).collect();
        out.reverse();
        out
    }

    /// Rewrites this device's log thinned out. Cheap; run on open.
    pub fn compact(&self, now: u64) -> std::io::Result<()> {
        let own = self.load_own();
        if own.len() < 2 {
            return Ok(());
        }
        let last_hash = own.last().and_then(|x| x.1.clone());
        let thinned = thin(own.into_iter().map(|x| x.0).collect(), now);
        let mut text = String::new();
        let n = thinned.len();
        for (i, c) in thinned.iter().enumerate() {
            let mut v = c.to_json();
            if i + 1 == n {
                if let Some(h) = &last_hash {
                    v["h"] = json!(h);
                }
            }
            text.push_str(&v.to_string());
            text.push('\n');
        }
        let tmp = self.own().with_extension("tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(tmp, self.own())
    }
}

/// Which bucket a step of age `age` falls in; steps sharing one merge.
fn bucket(t: u64, now: u64, i: usize) -> (u8, u64) {
    let age = now.saturating_sub(t);
    if age < DAY {
        (0, i as u64)
    } else if age < 7 * DAY {
        (1, t / HOUR)
    } else if age < 30 * DAY {
        (2, t / DAY)
    } else if age < 365 * DAY {
        (3, t / (7 * DAY))
    } else {
        (4, t / (30 * DAY))
    }
}

type Side = Vec<(String, Option<Value>, Option<Value>)>;

/// Folds `next` into `acc` (indexed by element id) in place.
fn absorb(acc: &mut Side, index: &mut HashMap<String, usize>, next: &Side) {
    for (id, b, a) in next {
        match index.get(id) {
            Some(&i) => acc[i].2 = a.clone(),
            None => {
                index.insert(id.clone(), acc.len());
                acc.push((id.clone(), b.clone(), a.clone()));
            }
        }
    }
}

pub fn thin(steps: Vec<Change>, now: u64) -> Vec<Change> {
    struct Acc {
        key: Option<(u8, u64)>,
        c: Change,
        ni: HashMap<String, usize>,
        ei: HashMap<String, usize>,
    }
    let mut out: Vec<Acc> = Vec::new();
    for (i, c) in steps.into_iter().enumerate() {
        // The very first step (the board's birth) is never merged.
        let key = (i > 0).then(|| bucket(c.t, now, i));
        if let Some(last) = out.last_mut() {
            if key.is_some() && last.key == key && key.is_some_and(|k| k.0 > 0) {
                absorb(&mut last.c.nodes, &mut last.ni, &c.nodes);
                absorb(&mut last.c.edges, &mut last.ei, &c.edges);
                last.c.t = c.t;
                last.c.dev = c.dev;
                continue;
            }
        }
        let ni = c.nodes.iter().enumerate().map(|(k, x)| (x.0.clone(), k)).collect();
        let ei = c.edges.iter().enumerate().map(|(k, x)| (x.0.clone(), k)).collect();
        out.push(Acc { key, c, ni, ei });
    }
    out.into_iter()
        .map(|mut a| {
            a.c.nodes.retain(|x| x.1 != x.2);
            a.c.edges.retain(|x| x.1 != x.2);
            a.c
        })
        .filter(|c| !c.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::{Board, Node, Shape};

    #[test]
    fn thinning_keeps_recent_and_merges_old() {
        let now = 400 * DAY;
        let mut steps = Vec::new();
        // A step every 3 hours for 400 days, each adding a node.
        let mut t = 0;
        let mut k = 0;
        while t < now {
            let n = Node { id: format!("n{k}"), x: k as f32, y: 0.0, w: 1.0, h: 1.0, text: String::new(), font: 20.0, shape: Shape::Box, color: None, fill: None, extra: Default::default() };
            steps.push(Change { t, dev: String::new(), nodes: vec![(n.id.clone(), None, Some(crate::board::node_to_json(&n)))], edges: vec![] });
            t += 3 * HOUR;
            k += 1;
        }
        let n = steps.len();
        let thinned = thin(steps.clone(), now);
        // 8 last day + 48 (one per 3 h for 6 days) + 23 days + ~48 weeks + ~2 months + first.
        assert!(thinned.len() < 140, "{}", thinned.len());
        assert!(thinned.len() > 120, "{}", thinned.len());
        // Replaying the merged steps still builds the same board.
        let mut replay = Board::new();
        for c in &thinned {
            replay.apply(c, true);
        }
        assert_eq!(replay.nodes.len(), n);
        // And walking back from the end reaches empty.
        for c in thinned.iter().rev() {
            replay.apply(c, false);
        }
        assert!(replay.nodes.is_empty());
    }
}
