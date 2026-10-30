//! Times the CPU work of one resize step on a real board.
use hs_core::board::Board;
use hs_core::{route, view};
use std::time::Instant;

fn main() {
    let path = std::env::args().nth(1).expect("board path");
    let text = std::fs::read_to_string(path).unwrap();
    let board = Board::parse(&text).unwrap();
    let m = |t: &str, _s: f32, _w: f32| t.len() / 20 + 1;
    for _ in 0..3 {
        let t = Instant::now();
        let b2 = board.clone();
        let clone = t.elapsed();
        let t = Instant::now();
        let routes = route::route_all(&b2, false);
        let r = t.elapsed();
        let t = Instant::now();
        let mut n = 0;
        for e in b2.edges.iter().filter(|e| e.kind == hs_core::board::Kind::Link) {
            if let (Some(a), Some(b)) = (b2.get(&e.from), b2.get(&e.to)) {
                n += route::route_between(&b2, a, b).len();
            }
        }
        println!("  route_between only {:?} ({n} pts)", t.elapsed());
        let t = Instant::now();
        let skip = view::Skip { nodes: &Default::default(), edges: &Default::default() };
        let s = view::build(&b2, &routes, &m, &skip, view::bg_color());
        let v = t.elapsed();
        println!("clone {:?}  route_all {:?}  view {:?}  rects {} labels {}", clone, r, v, s.rects.len(), s.labels.len());
    }
}
