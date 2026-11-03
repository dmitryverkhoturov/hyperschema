//! The server side of sync: one CouchDB database shared by all boards.
//!
//! - `board:<id>` — a board's card: `{type: "board", name}`;
//! - `chg:<id>:<time>-<actor>-<n>` — one batch of Automerge changes from
//!   one device, `{type: "chg", board, data: base64}`.
//!
//! Devices only ever add `chg:` documents, never rewrite someone else's,
//! so the database itself never conflicts; merging is Automerge's job.

use base64::Engine as _;
use serde_json::{json, Value};
use std::time::Duration;

#[derive(Clone, Debug)]
pub struct Config {
    pub url: String,
    pub db: String,
    pub user: String,
    pub password: String,
}

impl Config {
    pub fn parse(text: &str) -> Option<Config> {
        let v: Value = serde_json::from_str(text).ok()?;
        Some(Config {
            url: v["url"].as_str()?.trim_end_matches('/').to_string(),
            db: v["db"].as_str().unwrap_or("hyperschema").to_string(),
            user: v["user"].as_str()?.to_string(),
            password: v["password"].as_str()?.to_string(),
        })
    }
}

pub struct BoardCard {
    pub id: String,
    pub name: String,
}

pub struct Couch {
    cfg: Config,
    auth: String,
    agent: ureq::Agent,
    long: ureq::Agent,
    n: u64,
}

fn b64() -> base64::engine::GeneralPurpose {
    base64::engine::general_purpose::STANDARD
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Keys of `_all_docs` that start with `prefix`.
fn range(prefix: &str) -> String {
    let lo = serde_json::to_string(prefix).unwrap();
    let hi = serde_json::to_string(&format!("{prefix}\u{fff0}")).unwrap();
    format!("startkey={}&endkey={}", enc(&lo), enc(&hi))
}

fn enc(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

impl Couch {
    pub fn new(cfg: Config) -> Couch {
        let auth = format!("Basic {}", b64().encode(format!("{}:{}", cfg.user, cfg.password)));
        // Idle connections die quietly on the way (proxy, server); reusing
        // one hangs until the timeout, so only fresh ones are reused.
        let mk = |secs, idle| -> ureq::Agent {
            ureq::Agent::config_builder()
                .timeout_global(Some(Duration::from_secs(secs)))
                .timeout_connect(Some(Duration::from_secs(10)))
                .max_idle_age(Duration::from_secs(idle))
                .http_status_as_error(false)
                .build()
                .into()
        };
        Couch { cfg, auth, agent: mk(20, 4), long: mk(90, 1), n: 0 }
    }

    fn db(&self) -> String {
        format!("{}/{}", self.cfg.url, self.cfg.db)
    }

    fn get(&self, url: &str) -> Result<Value, String> {
        let mut r = self.agent.get(url).header("Authorization", &self.auth).call().map_err(|e| e.to_string())?;
        let status = r.status().as_u16();
        let v: Value = r.body_mut().with_config().limit(256 << 20).read_json().map_err(|e| e.to_string())?;
        if status >= 400 {
            return Err(format!("{status}: {}", v["reason"].as_str().unwrap_or("")));
        }
        Ok(v)
    }

    fn send(&self, agent: &ureq::Agent, method: &str, url: &str, body: &Value) -> Result<Value, String> {
        let req = match method {
            "PUT" => agent.put(url),
            _ => agent.post(url),
        };
        let mut r = req.header("Authorization", &self.auth).send_json(body).map_err(|e| e.to_string())?;
        let status = r.status().as_u16();
        let v: Value = r.body_mut().with_config().limit(256 << 20).read_json().map_err(|e| e.to_string())?;
        if status >= 400 {
            return Err(format!("{status}: {}", v["reason"].as_str().unwrap_or("")));
        }
        Ok(v)
    }

    pub fn boards(&self) -> Result<Vec<BoardCard>, String> {
        let v = self.get(&format!("{}/_all_docs?include_docs=true&{}", self.db(), range("board:")))?;
        Ok(v["rows"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|r| {
                let d = &r["doc"];
                Some(BoardCard { id: d["_id"].as_str()?.strip_prefix("board:")?.to_string(), name: d["name"].as_str().unwrap_or("").to_string() })
            })
            .collect())
    }

    /// Creates or renames a board's card.
    pub fn put_card(&self, id: &str, name: &str) -> Result<(), String> {
        let url = format!("{}/{}", self.db(), enc(&format!("board:{id}")));
        let mut body = json!({ "type": "board", "name": name });
        if let Ok(old) = self.get(&url) {
            if old["name"].as_str() == Some(name) {
                return Ok(());
            }
            body["_rev"] = old["_rev"].clone();
        }
        self.send(&self.agent, "PUT", &url, &body).map(|_| ())
    }

    /// Adds one batch of changes from this device.
    pub fn push(&mut self, board: &str, actor: &str, bytes: &[u8]) -> Result<(), String> {
        self.n += 1;
        let short = &actor[..actor.len().min(12)];
        let id = format!("chg:{board}:{:013}-{short}-{}", now_ms(), self.n);
        let body = json!({ "_id": id, "type": "chg", "board": board, "data": b64().encode(bytes) });
        self.send(&self.agent, "POST", &self.db(), &body).map(|_| ())
    }

    fn decode(doc: &Value) -> Option<Vec<u8>> {
        b64().decode(doc["data"].as_str()?).ok()
    }

    /// The database's current position in its change feed.
    pub fn seq(&self) -> Result<Value, String> {
        Ok(self.get(&self.db())?["update_seq"].clone())
    }

    /// Every batch of a board, oldest first.
    pub fn pull_all(&self, board: &str) -> Result<Vec<Vec<u8>>, String> {
        let v = self.get(&format!("{}/_all_docs?include_docs=true&{}", self.db(), range(&format!("chg:{board}:"))))?;
        Ok(v["rows"].as_array().into_iter().flatten().filter_map(|r| Self::decode(&r["doc"])).collect())
    }

    /// Waits up to `wait` for new batches of `board` after `since`.
    /// Returns them and the new position.
    pub fn wait(&self, board: &str, since: &Value, wait: Duration) -> Result<(Vec<Vec<u8>>, Value), String> {
        let since = match since {
            Value::String(s) => s.clone(),
            v => v.to_string(),
        };
        let url = format!("{}/_changes?feed=longpoll&include_docs=true&filter=_selector&timeout={}&since={}", self.db(), wait.as_millis(), enc(&since));
        let v = self.send(&self.long, "POST", &url, &json!({ "selector": { "type": "chg", "board": board } }))?;
        let got = v["results"].as_array().into_iter().flatten().filter_map(|r| Self::decode(&r["doc"])).collect();
        Ok((got, v["last_seq"].clone()))
    }
}
