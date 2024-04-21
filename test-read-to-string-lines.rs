// test-read-to-string-lines.rs
use std::io::Read;
fn main() {
    let now = std::time::Instant::now();
    let f = std::fs::File::open("measurements.txt").unwrap();
    let mut reader = std::io::BufReader::new(f);
    let mut s = String::new();
    reader.read_to_string(&mut s).unwrap();
    let mut amount_read = 0;
    for line in s.lines() {
        amount_read += line.len() + 1; // add the newline character back
    }
    println!("reading {amount_read} bytes took {:#?}", now.elapsed());
}
