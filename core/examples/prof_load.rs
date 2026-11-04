//! Times opening a board, step by step, without a window.
use hs_core::board::Board;
use hs_core::{route, view};
use std::collections::HashMap;
use std::time::Instant;

fn main() {
    let path = std::env::args().nth(1).expect("board path");
    let t = Instant::now();
    let text = std::fs::read_to_string(path).unwrap();
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    println!("json {:?}", t.elapsed());
    drop(v);
    let t = Instant::now();
    let board = Board::parse(&text).unwrap();
    println!("parse {:?}", t.elapsed());
    let mut board = board;
    let t = Instant::now();
    board.reindex();
    println!("reindex {:?}", t.elapsed());
    let t = Instant::now();
    let b2 = board.clone();
    println!("clone {:?}", t.elapsed());
    let t = Instant::now();
    let order = b2.draw_order();
    println!("draw_order {:?} ({})", t.elapsed(), order.len());
    let t = Instant::now();
    let raw = route::raw_routes(&b2, false, None, &HashMap::new());
    println!("raw {:?}", t.elapsed());
    let t = Instant::now();
    let routes = route::finish(&b2, &raw);
    println!("finish {:?}", t.elapsed());
    let m = |t: &str, _s: f32, _w: f32| t.len() / 20 + 1;
    let t = Instant::now();
    let skip = view::Skip { nodes: &Default::default(), edges: &Default::default(), bright: None, moving: None };
    let s = view::build(&b2, &routes, &m, &skip, view::bg_color());
    println!("view {:?}  rects {} labels {} lines {}", t.elapsed(), s.rects.len(), s.labels.len(), s.lines.len());
}
