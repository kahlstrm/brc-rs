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
/// Every 8-byte load starts within the data, so read buffers keep one load width of padding after it
const PADDING: usize = size_of::<u64>();

fn load_u64(buf: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(buf[at..at + 8].try_into().unwrap())
}

/// Branchless parse of a temperature (`-99.9`..`99.9`, one fractional digit) from the 8 bytes starting at it,
/// returning the value in tenths. Based on merykitty's solution to the original Java challenge.
fn parse_temperature(word: u64) -> i64 {
    // ASCII digits have bit 4 set while '.' and '-' do not: locate the '.', which is at byte 1, 2 or 3
    let dot_bit = (!word & 0x1010_1000).trailing_zeros();
    // all ones if the first byte is '-', zero otherwise
    let sign = ((!word << 59) as i64) >> 63;
    let unsigned = word & !(sign as u64 & 0xFF);
    // align the '.' to byte 3, putting tens, ones and tenths digits to bytes 1, 2 and 4
    let digits = (unsigned << (28 - dot_bit)) & 0x0F_000F_0F00;
    // a single multiplication sums tens * 100 + ones * 10 + tenths into bits 32..42
    let abs = (digits.wrapping_mul(0x640a_0001) >> 32) & 0x3FF;
    (abs as i64 ^ sign) - sign
}

/// Parses the line from `start` to the '\n' at `end`, returning the station name and the measurement
fn parse_line(buf: &[u8], start: usize, end: usize) -> (&[u8], i64) {
    // the temperature is at most 5 bytes long, so the ';' is within the last 6 bytes
    let semicolon = end
        - 1
        - buf[start..end]
            .iter()
            .rev()
            .take(6)
            .position(|&b| b == b';')
            .unwrap();
    (
        &buf[start..semicolon],
        parse_temperature(load_u64(buf, semicolon + 1)),
    )
}
struct Chunk {
    start_point: u64,
    len: usize,
    outer_map: Arc<Mutex<HashMap<Vec<u8>, WeatherStationStats>>>,
}
/// One thread is spawned per chunk, so this is both the chunk and the thread count.
fn thread_count(brc_threads: Option<String>, parallelism: usize) -> usize {
    match brc_threads {
        Some(s) => s
            .parse::<NonZeroUsize>()
            .unwrap_or_else(|_| panic!("BRC_THREADS must be a positive integer, got {s:?}"))
            .get(),
        None => parallelism,
    }
}
fn chunk_le_file<T: BufRead + Seek>(
    mut f: T,
    file_len: usize,
    arccimuuteksi: Arc<Mutex<HashMap<Vec<u8>, WeatherStationStats>>>,
) -> Vec<Chunk> {
    let chunk_count = thread_count(
        std::env::var("BRC_THREADS").ok(),
        std::thread::available_parallelism()
            .map(NonZeroUsize::get)
            .unwrap_or(1),
    );
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
    let mut buf = [0; CHUNK_SIZE + PADDING];
    let mut bytes_read = kontsa.read(&mut buf[..CHUNK_SIZE]).unwrap();
    let mut consumed = 0;
    loop {
        let Some(line_end_idx) = buf[consumed..bytes_read].iter().position(|b| *b == b'\n') else {
            buf.copy_within(consumed..bytes_read, 0);
            let remainder = bytes_read - consumed;
            bytes_read = kontsa.read(&mut buf[remainder..CHUNK_SIZE]).unwrap();
            // here if we get bytes_read == 0, which means we did not add anything to remaining characters
            // and as we are here already, we know that there is no valid line
            if bytes_read == 0 {
                break;
            }
            bytes_read += remainder;
            consumed = 0;
            continue;
        };
        let (station_name, measurement) = parse_line(&buf, consumed, consumed + line_end_idx);

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
    use crate::parse_temperature;
    use crate::thread_count;
    use crate::PADDING;

    #[test]
    fn thread_count_defaults_to_parallelism() {
        assert_eq!(thread_count(None, 8), 8);
    }
    #[test]
    fn thread_count_uses_override() {
        assert_eq!(thread_count(Some("1".into()), 8), 1);
    }
    #[test]
    #[should_panic(expected = "BRC_THREADS must be a positive integer")]
    fn thread_count_rejects_zero() {
        thread_count(Some("0".into()), 8);
    }
    /// Parses the first line of `line` followed by another line and padding, as in the read buffer
    fn parse_first_line(line: &[u8]) -> (&[u8], i64) {
        let buf = [line, b"Next;-1.0\n", &[0; PADDING]].concat().leak();
        let end = buf.iter().position(|&b| b == b'\n').unwrap();
        parse_line(buf, 0, end)
    }

    #[test]
    fn parse_line_stays_in_bounds_at_the_end_of_the_buffer() {
        for (temperature, expected) in [("1.2", 12), ("-1.2", -12), ("12.3", 123), ("-12.3", -123)]
        {
            let buf = [format!("a;{temperature}\n").as_bytes(), &[0; PADDING]].concat();
            let end = buf.len() - PADDING - 1;
            assert_eq!(
                parse_line(&buf, 0, end),
                (&b"a"[..], expected),
                "{temperature}"
            );
        }
    }
    macro_rules! tst_parse_line {
        ($func:ident,$line:expr,$expected:expr) => {
            #[test]
            fn $func() {
                let (station, measurement) = $expected;
                assert_eq!(parse_first_line($line), (station.as_bytes(), measurement))
            }
        };
    }
    tst_parse_line!(
        parse_line_works_negative_double_digit,
        b"StationName;-12.3\n",
        ("StationName", -123)
    );
    tst_parse_line!(
        parse_line_works_negative_only_decimal,
        b"StationName;-0.3\n",
        ("StationName", -03)
    );
    tst_parse_line!(
        parse_line_works_positive_single_digit,
        b"StationName;3.0\n",
        ("StationName", 30)
    );
    tst_parse_line!(
        parse_line_works_positive_only_decimal,
        b"StationName;0.6\n",
        ("StationName", 6)
    );
    tst_parse_line!(
        parse_line_works_positive_double_digit,
        b"StationName;99.9\n",
        ("StationName", 999)
    );
    tst_parse_line!(
        parse_line_works_multibyte_utf8_name,
        "Søul;12.0\n".as_bytes(),
        ("Søul", 120)
    );

    #[test]
    fn parse_line_works_for_all_name_lengths() {
        for len in 1..=100 {
            let name = "x".repeat(len);
            let line = format!("{name};-12.3\n");
            assert_eq!(
                parse_first_line(line.as_bytes()),
                (name.as_bytes(), -123),
                "name length {len}"
            );
        }
    }

    #[test]
    fn parse_temperature_works_for_all_values() {
        for tenths in -999i64..=999 {
            let sign = if tenths < 0 { "-" } else { "" };
            let text = format!("{sign}{}.{}\n", tenths.abs() / 10, tenths.abs() % 10);
            let bytes = [text.as_bytes(), b"Next;"].concat();
            let word = u64::from_le_bytes(bytes[..8].try_into().unwrap());
            assert_eq!(parse_temperature(word), tenths, "{text:?}");
        }
    }

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
                    assert_eq!(val.trim(), expected.trim());
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
