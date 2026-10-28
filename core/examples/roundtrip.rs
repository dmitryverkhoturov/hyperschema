//! Loads a scheme, saves it without edits and checks nothing changed.
use hs_core::doc::Doc;

fn main() {
    let path = std::env::args().nth(1).expect("path");
    let text = std::fs::read_to_string(&path).unwrap();
    let d = Doc::parse(&text).unwrap();
    let out = d.serialize();
    let back = Doc::parse(&out).unwrap();
    println!("elements: {} (live {})", d.els.len(), (0..d.els.len()).filter(|&i| d.live(i)).count());
    println!("elements equal after save: {}", d.els == back.els);
    let section = |s: &str| s.split("## Text Elements\n").nth(1).map(|r| r.split("%%").next().unwrap().to_string());
    println!("text section identical: {}", section(&text) == section(&out));
    if section(&text) != section(&out) {
        std::fs::write("orig-section.txt", section(&text).unwrap_or_default()).unwrap();
        std::fs::write("new-section.txt", section(&out).unwrap_or_default()).unwrap();
    }
    let head = |s: &str| s.split("## Drawing").next().unwrap().len();
    println!("prefix len {} -> {}", head(&text), head(&out));
    std::fs::write("roundtrip-out.md", &out).unwrap();
}
