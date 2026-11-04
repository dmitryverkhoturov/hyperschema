//! Times routing a real board: raw routes, then lanes.
use hs_core::board::Board;
use hs_core::route;
use std::collections::HashMap;
use std::time::Instant;

fn main() {
    let path = std::env::args().nth(1).expect("board path");
    let board = Board::parse(&std::fs::read_to_string(path).unwrap()).unwrap();
    for _ in 0..3 {
        let t = Instant::now();
        let raw = route::raw_routes(&board, false, None, &HashMap::new());
        let r = t.elapsed();
        let t = Instant::now();
        let fin = route::finish(&board, &raw);
        println!("raw {:?} finish {:?} ({} routes)", r, t.elapsed(), fin.len());
    }
}
