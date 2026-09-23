//! Dev probe: print the chapter marks of an m4b.
//! `cargo run -p skadi-media-probe --example chapters -- /path/to/book.m4b`

fn main() {
    let path = std::env::args().nth(1).expect("usage: chapters <file.m4b>");
    let marks =
        skadi_media_probe::chapters::chapters(std::path::Path::new(&path)).expect("read chapters");
    println!("{} chapters", marks.len());
    for m in marks {
        println!(
            "{:>3}  {:>9.1}s – {:>9.1}s  {}",
            m.index, m.start_secs, m.end_secs, m.title
        );
    }
}
