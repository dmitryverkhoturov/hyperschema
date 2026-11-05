//! Command line for agents and scripts: `hyperschema <command> ...`.
//! Every command first pulls the board from the server, then changes it
//! and sends the change right away, so it merges with edits made in the
//! app on any device. Without a server config it works on this device's
//! copy only.

use hs_core::board::{Board, Kind, Node, Shape};
use hs_core::couch::{Config, Couch};
use hs_core::library::{push_now, sync_now, Library, Live};
use serde_json::{Map, Value};
use std::path::PathBuf;

pub const COMMANDS: &[&str] = &["help", "boards", "show", "add", "edit", "move", "link", "unlink", "delete", "import", "export", "rename", "history"];

const HELP: &str = "\
hyperschema — boards from the command line. Changes sync to every device.

  boards                         list boards
  show <board> [--json]          concepts as an outline: id, text, links
  add <board> <text> [--in ID] [--near ID] [--at X,Y] [--size W,H] [--link ID]
                                 new concept; prints its id
  edit <board> <ID> [--text T] [--color C] [--fill C] [--shape box|ellipse|diamond|text]
  move <board> <ID> [--to X,Y] [--by DX,DY] [--in ID] [--out]
  link <board> <FROM> <TO> [--label T] [--arrow]
  unlink <board> <FROM> <TO>     or: unlink <board> <EDGE-ID>
  delete <board> <ID>            concept with everything inside it
  rename <board> <new name>
  import <file> [--name N]       .hyperschema or Excalidraw file -> new board
  export <board> [file]          board as .hyperschema JSON (stdout without file)
  history <board> [N]            last N changes with time and device

<board> is a board name (or a unique part of it) or its id.
Never edit the app's files directly: use these commands.";

fn data_dir() -> PathBuf {
    crate::data_dir()
}

/// The server connection the user entered in the menu ("Sync server…").
pub fn config_path() -> PathBuf {
    data_dir().join("server.json")
}

pub fn config() -> Option<Config> {
    std::fs::read_to_string(config_path()).ok().and_then(|s| Config::parse(&s))
}

/// Windows GUI apps get no console; borrow the caller's when there is one.
fn attach_console() {
    use windows::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
    unsafe {
        let _ = AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

pub fn run(args: Vec<String>) -> i32 {
    attach_console();
    match go(args) {
        Ok(out) => {
            if !out.is_empty() {
                println!("{out}");
            }
            0
        }
        Err(e) => {
            eprintln!("error: {e}");
            1
        }
    }
}

struct Opts {
    pos: Vec<String>,
    flags: Map<String, Value>,
}

fn opts(args: &[String]) -> Opts {
    let mut pos = Vec::new();
    let mut flags = Map::new();
    let mut it = args.iter().peekable();
    while let Some(a) = it.next() {
        if let Some(k) = a.strip_prefix("--") {
            let takes = !matches!(k, "json" | "arrow" | "out");
            let v = if takes { it.next().cloned().map(Value::String).unwrap_or(Value::Null) } else { Value::Bool(true) };
            flags.insert(k.to_string(), v);
        } else {
            pos.push(a.clone());
        }
    }
    Opts { pos, flags }
}

impl Opts {
    fn s(&self, k: &str) -> Option<&str> {
        self.flags.get(k).and_then(|v| v.as_str())
    }
    fn b(&self, k: &str) -> bool {
        self.flags.contains_key(k)
    }
    fn pair(&self, k: &str) -> Result<Option<(f32, f32)>, String> {
        let Some(s) = self.s(k) else { return Ok(None) };
        let v: Vec<f32> = s.split(',').filter_map(|x| x.trim().parse().ok()).collect();
        if v.len() != 2 {
            return Err(format!("--{k} wants X,Y"));
        }
        Ok(Some((v[0], v[1])))
    }
    fn arg(&self, i: usize, what: &str) -> Result<&str, String> {
        self.pos.get(i).map(String::as_str).ok_or_else(|| format!("missing {what}"))
    }
}

/// The app's library; the CLI keeps its own copy so it never writes a
/// file the running app holds.
fn lib() -> Library {
    Library::open(data_dir().join("cli"), true)
}

/// Finds a board by id or name, on the server first.
fn find(lib: &Library, cfg: Option<&Config>, q: &str) -> Result<(String, String), String> {
    let mut all: Vec<(String, String)> = match cfg {
        Some(c) => Couch::new(c.clone()).boards()?.into_iter().map(|b| (b.id, b.name)).collect(),
        None => Library::open(data_dir().join("boards"), true).list(),
    };
    for (id, name) in lib.list() {
        if !all.iter().any(|x| x.0 == id) {
            all.push((id, name));
        }
    }
    if let Some(x) = all.iter().find(|x| x.0 == q) {
        return Ok(x.clone());
    }
    let ql = q.to_lowercase();
    if let Some(x) = all.iter().find(|x| x.1.to_lowercase() == ql) {
        return Ok(x.clone());
    }
    let hits: Vec<_> = all.iter().filter(|x| x.1.to_lowercase().contains(&ql)).collect();
    match hits.len() {
        1 => Ok(hits[0].clone()),
        0 => Err(format!("no board matches \"{q}\"; see `hyperschema boards`")),
        _ => Err(format!("\"{q}\" matches several boards: {}", hits.iter().map(|x| x.1.as_str()).collect::<Vec<_>>().join(", "))),
    }
}

/// Opens a board fresh from the server (or this device without one).
fn open(q: &str) -> Result<(Library, Option<Config>, Live), String> {
    let cfg = config();
    let lib = lib();
    let (id, name) = find(&lib, cfg.as_ref(), q)?;
    if !lib.exists(&id) {
        match &cfg {
            Some(_) => lib.adopt(&id, &name).map_err(|e| e.to_string())?,
            None => {
                // No server: work on the app's own copy.
                let app = Library::open(data_dir().join("boards"), true);
                return app.load(&id).map(|l| (app, None, l));
            }
        }
    }
    let live = match &cfg {
        Some(c) => sync_now(&lib, &id, c)?,
        None => lib.load(&id)?,
    };
    Ok((lib, cfg, live))
}

/// Writes one step and sends it.
fn commit(cfg: &Option<Config>, live: &mut Live, before: &Board, after: &Board) -> Result<(), String> {
    let Some(c) = after.diff(&before.snapshot()) else { return Ok(()) };
    live.record(&[c]);
    live.save().map_err(|e| e.to_string())?;
    if let Some(cfg) = cfg {
        push_now(live, cfg)?;
    }
    Ok(())
}

fn node_ref<'a>(b: &'a Board, id: &str) -> Result<&'a Node, String> {
    b.get(id).ok_or_else(|| format!("no concept with id {id}"))
}

/// A size that fits `text` at `font`, roughly the way the app wraps.
fn fit(text: &str, font: f32) -> (f32, f32) {
    let longest = text.lines().map(|l| l.chars().count()).max().unwrap_or(0) as f32;
    let w = (longest * font * 0.55 + font).clamp(font * 8.0, font * 16.0);
    let per_line = ((w - font) / (font * 0.55)).max(1.0);
    let lines: f32 = text.lines().map(|l| (l.chars().count() as f32 / per_line).ceil().max(1.0)).sum::<f32>().max(1.0);
    (w, (lines * font * 1.3 + font).max(font * 4.0))
}

fn title_lines(_: &Node) -> usize {
    1
}

fn go(args: Vec<String>) -> Result<String, String> {
    let cmd = args.first().cloned().unwrap_or_else(|| "help".into());
    let o = opts(&args[1..]);
    match cmd.as_str() {
        "help" => Ok(HELP.into()),
        "boards" => {
            let cfg = config();
            let mut out = String::new();
            match &cfg {
                Some(c) => {
                    for b in Couch::new(c.clone()).boards()? {
                        out.push_str(&format!("{}  {}\n", b.id, b.name));
                    }
                }
                None => {
                    for (id, name) in Library::open(data_dir().join("boards"), true).list() {
                        out.push_str(&format!("{id}  {name}\n"));
                    }
                    out.push_str("(no server configured: boards of this device only)\n");
                }
            }
            Ok(out.trim_end().into())
        }
        "show" => {
            let (_, _, live) = open(o.arg(0, "board")?)?;
            let b = live.board();
            if o.b("json") {
                return Ok(b.serialize());
            }
            Ok(outline(&b, &live.name))
        }
        "export" => {
            let (_, _, live) = open(o.arg(0, "board")?)?;
            let text = live.board().serialize();
            match o.pos.get(1) {
                Some(f) => std::fs::write(f, text).map(|_| format!("written {f}")).map_err(|e| e.to_string()),
                None => Ok(text),
            }
        }
        "history" => {
            let (_, _, mut live) = open(o.arg(0, "board")?)?;
            let n: usize = o.pos.get(1).and_then(|s| s.parse().ok()).unwrap_or(20);
            let ch = live.shared.changes();
            let mut out = String::new();
            for (t, h, actor) in ch.iter().rev().take(n).rev() {
                let secs = (*t / 1000) as u64;
                out.push_str(&format!("{}  {}  {}\n", crate::store::when(secs), &actor[..8.min(actor.len())], &h.to_string()[..12]));
            }
            out.push_str(&format!("{} changes in all", ch.len()));
            Ok(out)
        }
        "rename" => {
            let (lib, cfg, mut live) = open(o.arg(0, "board")?)?;
            let name = o.pos[1..].join(" ");
            if name.trim().is_empty() {
                return Err("missing new name".into());
            }
            lib.rename(&live.id, &name);
            live.name = name.clone();
            if let Some(c) = &cfg {
                Couch::new(c.clone()).put_card(&live.id, &name)?;
            }
            Ok(format!("renamed to {name}"))
        }
        "import" => {
            let f = o.arg(0, "file")?;
            let text = std::fs::read_to_string(f).map_err(|e| e.to_string())?;
            let board = if f.to_lowercase().ends_with(".hyperschema") { Board::parse(&text)? } else { hs_core::import::from_excalidraw(&text)?.0 };
            let name = o.s("name").map(String::from).unwrap_or_else(|| crate::store::stem(std::path::Path::new(f)));
            let lib = lib();
            let id = lib.create(&name, &board).map_err(|e| e.to_string())?;
            let mut live = lib.load(&id)?;
            if let Some(c) = config() {
                push_now(&mut live, &c)?;
            }
            Ok(format!("{id}  {name}  ({} concepts)", board.nodes.len()))
        }
        "add" => {
            let (_, cfg, mut live) = open(o.arg(0, "board")?)?;
            let text = o.arg(1, "text")?.replace("\\n", "\n");
            let before = live.board();
            let mut b = before.clone();
            let parent = o.s("in").map(String::from);
            let near = o.s("near").map(String::from);
            let font = match (&parent, &near) {
                (_, Some(n)) => node_ref(&b, n)?.font,
                (Some(p), None) => {
                    let kids = b.children(p);
                    kids.first().map(|k| b.node(k).font).unwrap_or(node_ref(&b, p)?.font * 0.6)
                }
                _ => b.nodes.iter().map(|n| n.font).fold(None, |m: Option<f32>, f| Some(m.map_or(f, |m| m.min(f)))).unwrap_or(20.0).max(20.0),
            };
            let (mut w, mut h) = fit(&text, font);
            if let Some((sw, sh)) = o.pair("size")? {
                (w, h) = (sw, sh);
            }
            let (x, y) = if let Some(p) = o.pair("at")? {
                p
            } else if let Some(n) = &near {
                let a = node_ref(&b, n)?;
                (a.x + a.w + a.font * 2.0, a.y)
            } else if let Some(p) = &parent {
                let pn = node_ref(&b, p)?.clone();
                let area = b.content_area(&pn, 1);
                let bottom = b.children(p).iter().map(|k| b.node(k).bbox()[3]).fold(area[1], f32::max);
                (area[0], if b.children(p).is_empty() { area[1] } else { bottom + font })
            } else {
                let bb = b.bounds();
                (bb[2] + font * 4.0, bb[1])
            };
            let id = b.new_id();
            b.add_node(Node { id: id.clone(), x, y, w, h, text, font, shape: Shape::Box, color: None, fill: None, extra: Map::new() });
            let parent = parent.or_else(|| near.as_ref().and_then(|n| b.parent(n).map(String::from)));
            if let Some(p) = &parent {
                node_ref(&b, p)?;
                b.set_parent(&id, Some(p));
                b.fit_ancestors(&id, &title_lines);
            }
            if let Some(l) = o.s("link").or(near.as_deref()) {
                node_ref(&b, l)?;
                let e = b.add_edge(l, &id, Kind::Link);
                if let Some(e) = b.edge_mut(&e) {
                    e.arrow_to = true;
                }
            }
            commit(&cfg, &mut live, &before, &b)?;
            Ok(id)
        }
        "edit" => {
            let (_, cfg, mut live) = open(o.arg(0, "board")?)?;
            let id = o.arg(1, "concept id")?;
            let before = live.board();
            let mut b = before.clone();
            node_ref(&b, id)?;
            let n = b.get_mut(id).unwrap();
            if let Some(t) = o.s("text") {
                n.text = t.replace("\\n", "\n");
            }
            if let Some(c) = o.s("color") {
                n.color = (!c.is_empty()).then(|| c.to_string());
            }
            if let Some(c) = o.s("fill") {
                n.fill = (!c.is_empty()).then(|| c.to_string());
            }
            if let Some(s) = o.s("shape") {
                n.shape = match s {
                    "ellipse" => Shape::Ellipse,
                    "diamond" => Shape::Diamond,
                    "text" => Shape::Text,
                    _ => Shape::Box,
                };
            }
            commit(&cfg, &mut live, &before, &b)?;
            Ok(format!("edited {id}"))
        }
        "move" => {
            let (_, cfg, mut live) = open(o.arg(0, "board")?)?;
            let id = o.arg(1, "concept id")?.to_string();
            let before = live.board();
            let mut b = before.clone();
            let n = node_ref(&b, &id)?.clone();
            if o.b("out") {
                b.set_parent(&id, None);
            }
            if let Some(p) = o.s("in") {
                node_ref(&b, p)?;
                b.set_parent(&id, Some(p));
                if o.pair("to")?.is_none() && o.pair("by")?.is_none() {
                    let pn = b.node(p).clone();
                    let area = b.content_area(&pn, 1);
                    let bottom = b.children(p).iter().filter(|k| **k != id).map(|k| b.node(k).bbox()[3]).fold(area[1], f32::max);
                    b.move_tree(&id, area[0] - n.x, bottom + n.font - n.y);
                }
            }
            if let Some((x, y)) = o.pair("to")? {
                b.move_tree(&id, x - n.x, y - n.y);
            }
            if let Some((dx, dy)) = o.pair("by")? {
                b.move_tree(&id, dx, dy);
            }
            b.fit_ancestors(&id, &title_lines);
            commit(&cfg, &mut live, &before, &b)?;
            Ok(format!("moved {id}"))
        }
        "link" => {
            let (_, cfg, mut live) = open(o.arg(0, "board")?)?;
            let (f, t) = (o.arg(1, "from id")?, o.arg(2, "to id")?);
            let before = live.board();
            let mut b = before.clone();
            node_ref(&b, f)?;
            node_ref(&b, t)?;
            let e = b.add_edge(f, t, Kind::Link);
            if let Some(ed) = b.edge_mut(&e) {
                ed.arrow_to = o.b("arrow");
                ed.label = o.s("label").unwrap_or_default().to_string();
            }
            commit(&cfg, &mut live, &before, &b)?;
            Ok(e)
        }
        "unlink" => {
            let (_, cfg, mut live) = open(o.arg(0, "board")?)?;
            let before = live.board();
            let mut b = before.clone();
            let gone: Vec<String> = match (o.pos.get(1), o.pos.get(2)) {
                (Some(f), Some(t)) => b.edges.iter().filter(|e| e.kind == Kind::Link && ((&e.from == f && &e.to == t) || (&e.from == t && &e.to == f))).map(|e| e.id.clone()).collect(),
                (Some(id), None) => b.edges.iter().filter(|e| &e.id == id).map(|e| e.id.clone()).collect(),
                _ => return Err("missing ids".into()),
            };
            if gone.is_empty() {
                return Err("no such connection".into());
            }
            b.edges.retain(|e| !gone.contains(&e.id));
            b.reindex();
            commit(&cfg, &mut live, &before, &b)?;
            Ok(format!("removed {}", gone.len()))
        }
        "delete" => {
            let (_, cfg, mut live) = open(o.arg(0, "board")?)?;
            let id = o.arg(1, "concept id")?;
            let before = live.board();
            let mut b = before.clone();
            node_ref(&b, id)?;
            let gone = b.subtree(id);
            b.nodes.retain(|n| !gone.contains(&n.id));
            b.reindex();
            commit(&cfg, &mut live, &before, &b)?;
            Ok(format!("deleted {} concept(s)", gone.len()))
        }
        other => Err(format!("unknown command {other}; see `hyperschema help`")),
    }
}

/// The board as nested text: `[id] text  -> links`.
fn outline(b: &Board, name: &str) -> String {
    let mut out = format!("# {name} — {} concepts, {} connections\n", b.nodes.len(), b.edges.iter().filter(|e| e.kind == Kind::Link).count());
    let one = |s: &str| -> String {
        let t = s.replace('\n', " / ");
        if t.chars().count() > 120 {
            format!("{}…", t.chars().take(120).collect::<String>())
        } else {
            t
        }
    };
    let mut stack: Vec<(String, usize)> = b.roots().into_iter().rev().map(|r| (r, 0)).collect();
    while let Some((id, d)) = stack.pop() {
        let n = b.node(&id);
        let links: Vec<String> = b
            .edges
            .iter()
            .filter(|e| e.kind == Kind::Link && e.from == id)
            .map(|e| if e.label.is_empty() { e.to.clone() } else { format!("{} ({})", e.to, one(&e.label)) })
            .collect();
        let text = if n.text.is_empty() { "(empty)".to_string() } else { one(&n.text) };
        out.push_str(&format!("{}[{}] {}", "  ".repeat(d), id, text));
        if !links.is_empty() {
            out.push_str(&format!("  -> {}", links.join(", ")));
        }
        out.push('\n');
        for c in b.children(&id).iter().rev() {
            stack.push((c.clone(), d + 1));
        }
    }
    out.trim_end().to_string()
}
