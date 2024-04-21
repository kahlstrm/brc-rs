// test-bufread-lines.rs
use std::io::BufRead;
fn main() {
    let now = std::time::Instant::now();
    let f = std::fs::File::open("measurements.txt").unwrap();
    let reader = std::io::BufReader::new(f);
    let mut amount_read = 0;
    for line in reader.lines() {
        amount_read += line.unwrap().len() + 1; // add the newline character back
    }
    println!("reading {amount_read} bytes took {:#?}", now.elapsed());
}
