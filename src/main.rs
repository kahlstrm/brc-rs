use std::{
    collections::HashMap,
    fs::File,
    hash::{BuildHasherDefault, Hasher},
    io::{BufRead, BufReader, Read, Seek},
    num::NonZeroUsize,
    ops::{Add, BitXor},
    sync::{Arc, Mutex},
    thread,
};

fn main() {
    let file_name = std::env::args().nth(1);
    let res = calc(file_name);
    println!("{res}");
    return;
}
struct WeatherStationStats {
    min: i64,
    max: i64,
    sum: i64,
    count: usize,
}
impl WeatherStationStats {
    fn mean(&self) -> f64 {
        self.sum as f64 / 10.0 / self.count as f64
    }
}
impl Add<&mut Self> for WeatherStationStats {
    type Output = Self;

    fn add(self, rhs: &mut Self) -> Self::Output {
        WeatherStationStats {
            min: self.min.min(rhs.min),
            max: self.max.max(rhs.max),
            sum: self.sum + rhs.sum,
            count: self.count + rhs.count,
        }
    }
}
fn parse_line(line: &[u8]) -> (&[u8], i64) {
    // we know that the measurement is pure ASCII and is at max 5 characters long
    // based on this we can find the semicolon faster by doing at most 6 byte comparisons by iterating the reversed bytes
    // At the same time, we _are_ iterating through the measurement from the least significant character to the biggest
    let mut semicolon_idx = 0;
    let mut is_negative = false;
    let mut measurement = 0;
    for (idx, b) in line.into_iter().rev().take(6).enumerate() {
        match (b, idx) {
            (b';', _) => {
                // idx is 0-based starting from the end, meaning it is 1-based from the beginning, hence the -1
                semicolon_idx = line.len() - idx - 1;
                break;
            }
            (b'-', _) => is_negative = true,
            (b'.', _) => (),
            // reversed index 0, this is the fractional digit, add to measurement as is
            (b, 0) => measurement += (b - b'0') as i64,
            // reversed index 2, is the first whole number, "shift" it once to the left with * 10
            (b, 2) => measurement += (b - b'0') as i64 * 10,
            // reversed index 2, is the first whole number, "shift" it twice to the left with * 100
            (b, 3) => measurement += (b - b'0') as i64 * 100,
            // Data is of incorrect format, as in indices 1, 4 or 5 always must be one of the other characters
            (b, _) => panic!(
                "{} , {:#?}",
                String::from_utf8(vec![*b]).unwrap(),
                String::from_utf8(line.to_vec())
            ),
        }
    }
    (
        &line[..semicolon_idx],
        if is_negative {
            -measurement
        } else {
            measurement
        },
    )
}
struct Chunk {
    start_point: u64,
    len: usize,
    outer_map: Arc<Mutex<HashMap<Vec<u8>, WeatherStationStats>>>,
}
fn chunk_le_file<T: BufRead + Seek>(
    mut f: T,
    file_len: usize,
    arccimuuteksi: Arc<Mutex<HashMap<Vec<u8>, WeatherStationStats>>>,
) -> Vec<Chunk> {
    let chunk_count = std::thread::available_parallelism()
        .map(NonZeroUsize::get)
        .unwrap_or(1)
    // do a sneaky 4x chunks vs available threads to allow OS scheduler to switch between threads,
    // potentially enabling I/O blocked threads being swapped to threads where I/O is not blocked.
    // 4 was tested to provide best perf with both M1 Macbook Max and Ryzen 5950x
    * 4;
    let chunk_size = file_len / chunk_count + 1;
    // max length of line is 100 bytes station name, ';', '-99.9', '\n'
    let mut tmp_arr = Vec::with_capacity(107);
    let mut res = vec![];
    let mut cur_start = 0;
    for _ in 0..chunk_count {
        f.seek(std::io::SeekFrom::Current(chunk_size as i64))
            .unwrap();
        f.read_until(b'\n', &mut tmp_arr).unwrap();
        let end_pos = f.stream_position().unwrap();
        res.push(Chunk {
            start_point: cur_start,
            len: (end_pos - cur_start) as usize,
            outer_map: arccimuuteksi.clone(),
        });
        tmp_arr.clear();
        cur_start = end_pos
    }
    res
}
fn calc(file_name: Option<String>) -> String {
    let file_name: Arc<str> = file_name.unwrap_or("measurements.txt".into()).into();
    let f = File::open(file_name.to_string()).unwrap();
    let file_len = f.metadata().unwrap().len() as usize;
    let stations = Arc::new(Mutex::new(HashMap::<Vec<u8>, WeatherStationStats>::new()));
    let chunks = chunk_le_file(BufReader::new(f), file_len, stations.clone());
    let handles = chunks
        .into_iter()
        .map(|c| {
            let file_name = file_name.clone();
            thread::spawn(move || {
                let mut f = File::open(file_name.to_string()).unwrap();
                f.seek(std::io::SeekFrom::Start(c.start_point)).unwrap();
                let f = f.take(c.len as u64);
                let stations_välipala = aggregate_measurements(f);
                let mut stations = c.outer_map.lock().unwrap();
                for (k, v) in stations_välipala {
                    match stations.get_mut(&k) {
                        Some(jutska) => *jutska = v + jutska,
                        None => {
                            stations.insert(k, v);
                        }
                    }
                }
            })
        })
        .collect::<Vec<_>>();
    for h in handles {
        h.join().unwrap()
    }
    let lock = stations.lock().unwrap();
    let mut res = lock.iter().collect::<Vec<_>>();

    res.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    String::from("{")
        + &res
            .into_iter()
            .map(|(station, stats)| {
                format!(
                    "{}={:.1}/{:.1}/{:.1}",
                    String::from_utf8_lossy(station),
                    stats.min as f64 / 10.0,
                    stats.mean(),
                    stats.max as f64 / 10.0
                )
            })
            .collect::<Vec<_>>()
            .join(", ")
        + &String::from("}\n")
}

type BuildCustomHasher = BuildHasherDefault<CustomHasher>;

#[derive(Default, Clone)]
struct CustomHasher {
    hash: u64,
}
// yoinked from https://docs.rs/rustc-hash/1.1.0/src/rustc_hash/lib.rs.html#76-109
impl CustomHasher {
    fn add_to_hash(&mut self, i: u64) {
        self.hash = self
            .hash
            .rotate_left(5)
            .bitxor(i)
            .wrapping_mul(0x517cc1b727220a95);
    }
}
impl Hasher for CustomHasher {
    fn finish(&self) -> u64 {
        self.hash
    }

    fn write(&mut self, mut bytes: &[u8]) {
        // This clone tries to ensure that the compiler keeps the state in a register instead of memory
        // https://github.com/rust-lang/rustc-hash/pull/34
        let mut state = self.clone();
        while bytes.len() >= 8 {
            state.add_to_hash(u64::from_ne_bytes(bytes[..8].try_into().unwrap()));
            bytes = &bytes[8..]
        }

        if bytes.len() >= 4 {
            state.add_to_hash(u32::from_ne_bytes(bytes[..4].try_into().unwrap()) as u64);
            bytes = &bytes[4..];
        }
        if bytes.len() >= 2 {
            state.add_to_hash(u16::from_ne_bytes(bytes[..2].try_into().unwrap()) as u64);
            bytes = &bytes[2..];
        }
        if bytes.len() >= 1 {
            state.add_to_hash(u8::from_ne_bytes(bytes[..1].try_into().unwrap()) as u64);
        }
        *self = state;
    }
}
// yoink end

const CHUNK_SIZE: usize = 500_000;
fn aggregate_measurements(
    mut kontsa: impl Read,
) -> HashMap<Vec<u8>, WeatherStationStats, BuildCustomHasher> {
    let mut stations = HashMap::with_hasher(BuildCustomHasher::default());
    let mut buf = [0; CHUNK_SIZE];
    let mut bytes_read = kontsa.read(&mut buf).unwrap();
    let mut consumed = 0;
    loop {
        let Some(line_end_idx) = buf[consumed..bytes_read].iter().position(|b| *b == b'\n') else {
            buf.copy_within(consumed..bytes_read, 0);
            let remainder = bytes_read - consumed;
            bytes_read = kontsa.read(&mut buf[remainder..]).unwrap();
            // here if we get bytes_read == 0, which means we did not add anything to remaining characters
            // and as we are here already, we know that there is no valid line
            if bytes_read == 0 {
                break;
            }
            bytes_read += remainder;
            consumed = 0;
            continue;
        };
        let (station_name, measurement) = parse_line(&buf[consumed..consumed + line_end_idx]);

        match stations.get_mut(station_name) {
            None => {
                stations.insert(
                    station_name.to_vec(),
                    WeatherStationStats {
                        min: measurement,
                        max: measurement,
                        sum: measurement,
                        count: 1,
                    },
                );
            }
            Some(s) => {
                s.max = s.max.max(measurement);
                s.min = s.min.min(measurement);
                s.count += 1;
                s.sum += measurement;
            }
        };
        // We have "consumed" one line of input
        consumed += line_end_idx + 1;
    }
    stations
}

#[cfg(test)]
mod tests {
    use std::fs::read_to_string;

    use crate::calc;
    use crate::parse_line;
    macro_rules! tst_parse_line {
        ($func:ident,$line:expr,$expected:expr) => {
            #[test]
            fn $func() {
                let (station, measurement) = $expected;
                assert_eq!(parse_line($line), (station.as_bytes(), measurement))
            }
        };
    }
    tst_parse_line!(
        parse_line_works_negative_double_digit,
        b"StationName;-12.3",
        ("StationName", -123)
    );
    tst_parse_line!(
        parse_line_works_negative_only_decimal,
        b"StationName;-0.3",
        ("StationName", -03)
    );
    tst_parse_line!(
        parse_line_works_positive_single_digit,
        b"StationName;3.0",
        ("StationName", 30)
    );
    tst_parse_line!(
        parse_line_works_positive_only_decimal,
        b"StationName;0.6",
        ("StationName", 6)
    );
    tst_parse_line!(
        parse_line_works_positive_double_digit,
        b"StationName;99.9",
        ("StationName", 999)
    );

    macro_rules! tst {
        ($func:ident,$file_name:expr) => {
            #[test]
            fn $func() {
                println!($file_name);
                let res = read_to_string(format!("{}.out", $file_name)).unwrap();
                for (expected, val) in res
                    .split(",")
                    .zip(calc(Some(format!("{}.txt", $file_name))).split(","))
                {
                    assert_eq!(val, expected);
                }
            }
        };
    }
    tst!(measurements_1, "samples/measurements-1");
    tst!(measurements_10, "samples/measurements-10");
    tst!(
        measurements_10000_unique_keys,
        "samples/measurements-10000-unique-keys"
    );
    tst!(measurements_2, "samples/measurements-2");
    tst!(measurements_20, "samples/measurements-20");
    tst!(measurements_3, "samples/measurements-3");
    tst!(measurements_boundaries, "samples/measurements-boundaries");
    tst!(
        measurements_complex_utf8,
        "samples/measurements-complex-utf8"
    );
    tst!(measurements_dot, "samples/measurements-dot");
    tst!(measurements_rounding, "samples/measurements-rounding");
    tst!(measurements_short, "samples/measurements-short");
    tst!(measurements_shortest, "samples/measurements-shortest");
    tst!(measurements_1m, "samples/measurements-1m");
}
