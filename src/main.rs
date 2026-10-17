use std::{
    collections::HashMap,
    fs::File,
    io::{BufRead, BufReader, Read, Seek},
    num::NonZeroUsize,
    ops::Add,
    sync::{Arc, Mutex},
    thread,
};

fn main() {
    let file_name = std::env::args().nth(1);
    let res = calc(file_name);
    println!("{res}");
    return;
}
#[derive(Clone, Copy, Default)]
struct WeatherStationStats {
    min: i64,
    max: i64,
    sum: i64,
    count: usize,
}
impl WeatherStationStats {
    fn new(measurement: i64) -> Self {
        WeatherStationStats {
            min: measurement,
            max: measurement,
            sum: measurement,
            count: 1,
        }
    }
    fn record(&mut self, measurement: i64) {
        self.min = self.min.min(measurement);
        self.max = self.max.max(measurement);
        self.sum += measurement;
        self.count += 1;
    }
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
/// Every 8-byte load starts within the data, except a short station name's second key word,
/// which starts less than 8 bytes past it, so read buffers keep two load widths of padding after it
const PADDING: usize = 2 * size_of::<u64>();

fn load_u64(buf: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(buf[at..at + 8].try_into().unwrap())
}

/// Finds the first ';' at or after `start`, checking 8 bytes at a time (SWAR)
fn find_semicolon(buf: &[u8], start: usize) -> usize {
    const ONES: u64 = 0x0101_0101_0101_0101;
    const HIGH_BITS: u64 = 0x8080_8080_8080_8080;
    const SEMICOLONS: u64 = ONES * b';' as u64;
    let mut at = start;
    loop {
        let x = load_u64(buf, at) ^ SEMICOLONS;
        // sets the high bit of each zero byte of x, i.e. each ';'.
        // Only the lowest match is guaranteed to be exact, which is the one we need
        let matches = x.wrapping_sub(ONES) & !x & HIGH_BITS;
        if matches != 0 {
            return at + (matches.trailing_zeros() / 8) as usize;
        }
        at += 8;
    }
}

/// Branchless parse of a temperature (`-99.9`..`99.9`, one fractional digit) from the 8 bytes starting at it.
/// Returns the value in tenths and the length of the temperature including the trailing '\n'.
/// Based on merykitty's solution to the original Java challenge.
fn parse_temperature(word: u64) -> (i64, usize) {
    // ASCII digits have bit 4 set while '.' and '-' do not: locate the '.', which is at byte 1, 2 or 3
    let dot_bit = (!word & 0x1010_1000).trailing_zeros();
    // all ones if the first byte is '-', zero otherwise
    let sign = ((!word << 59) as i64) >> 63;
    let unsigned = word & !(sign as u64 & 0xFF);
    // align the '.' to byte 3, putting tens, ones and tenths digits to bytes 1, 2 and 4
    let digits = (unsigned << (28 - dot_bit)) & 0x0F_000F_0F00;
    // a single multiplication sums tens * 100 + ones * 10 + tenths into bits 32..42
    let abs = (digits.wrapping_mul(0x640a_0001) >> 32) & 0x3FF;
    let value = (abs as i64 ^ sign) - sign;
    (value, (dot_bit / 8) as usize + 3)
}

/// Parses the line starting at `start`, returning the station name, the measurement and the line length
fn parse_line(buf: &[u8], start: usize) -> (&[u8], i64, usize) {
    let semicolon = find_semicolon(buf, start);
    let (measurement, temperature_len) = parse_temperature(load_u64(buf, semicolon + 1));
    (
        &buf[start..semicolon],
        measurement,
        semicolon + 1 + temperature_len - start,
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

/// Bytes of a station name stored inline in its slot and compared as two words
const INLINE_KEY_LEN: usize = 2 * size_of::<u64>();
/// A power of two, leaving a low load factor for the 10,000 stations the challenge allows
const TABLE_SIZE: usize = 1 << 16;
/// `KEY_MASKS[n]` keeps the lowest `n` bytes of a little-endian word
const KEY_MASKS: [u64; 9] = {
    let mut masks = [0; 9];
    let mut n = 1;
    while n <= 8 {
        masks[n] = u64::MAX >> (64 - 8 * n);
        n += 1;
    }
    masks
};

/// The first `INLINE_KEY_LEN` bytes of the name at `start`, zero-padded
fn key_words(buf: &[u8], start: usize, len: usize) -> [u64; 2] {
    [
        load_u64(buf, start) & KEY_MASKS[len.min(8)],
        load_u64(buf, start + 8) & KEY_MASKS[len.clamp(8, 16) - 8],
    ]
}

/// Bytes past the inline key only exist for long names, which are rare enough to hash bytewise
fn slot_index(key: [u64; 2], tail: &[u8]) -> usize {
    const K: u64 = 0x517c_c1b7_2722_0a95;
    let hash = tail
        .iter()
        .fold(key[0].wrapping_mul(K) ^ key[1], |h, &b| {
            (h ^ b as u64).wrapping_mul(K).rotate_left(5)
        })
        .wrapping_mul(K);
    (hash >> (64 - TABLE_SIZE.trailing_zeros())) as usize
}

/// One cache line per station, so a lookup and its update touch a single line
#[repr(align(64))]
#[derive(Clone, Copy, Default)]
struct Slot {
    key: [u64; 2],
    /// Zero marks an empty slot, as station names are at least one byte long
    len: usize,
    /// Where the bytes past the inline key start in `StationTable::tails`
    tail_offset: usize,
    stats: WeatherStationStats,
}

/// Open-addressing table with linear probing, keyed by station name
struct StationTable {
    slots: Box<[Slot; TABLE_SIZE]>,
    tails: Vec<u8>,
}
impl StationTable {
    fn new() -> Self {
        StationTable {
            slots: vec![Slot::default(); TABLE_SIZE]
                .into_boxed_slice()
                .try_into()
                .unwrap_or_else(|_| unreachable!()),
            tails: Vec::new(),
        }
    }

    /// Records a measurement for `name`, which starts at `start` in `buf`
    fn record(&mut self, buf: &[u8], start: usize, name: &[u8], measurement: i64) {
        let len = name.len();
        let key = key_words(buf, start, len);
        let tail = name.get(INLINE_KEY_LEN..).unwrap_or_default();
        let mut i = slot_index(key, tail);
        loop {
            let slot = &mut self.slots[i];
            if slot.key == key
                && slot.len == len
                && (tail.is_empty() || self.tails[slot.tail_offset..][..tail.len()] == *tail)
            {
                slot.stats.record(measurement);
                return;
            }
            if slot.len == 0 {
                *slot = Slot {
                    key,
                    len,
                    tail_offset: self.tails.len(),
                    stats: WeatherStationStats::new(measurement),
                };
                self.tails.extend_from_slice(tail);
                return;
            }
            i = (i + 1) % TABLE_SIZE;
        }
    }

    fn into_stations(self) -> Vec<(Vec<u8>, WeatherStationStats)> {
        self.slots
            .iter()
            .filter(|slot| slot.len != 0)
            .map(|slot| {
                let inline = slot.key.iter().flat_map(|word| word.to_le_bytes());
                let tail_len = slot.len.saturating_sub(INLINE_KEY_LEN);
                let tail = &self.tails[slot.tail_offset..][..tail_len];
                let name = inline.take(slot.len - tail_len).chain(tail.iter().copied());
                (name.collect(), slot.stats)
            })
            .collect()
    }
}

const CHUNK_SIZE: usize = 500_000;
/// Longer than any valid line: 100 bytes of station name, ';', "-99.9" and '\n'
const MAX_LINE_LEN: usize = 128;
fn aggregate_measurements(mut kontsa: impl Read) -> Vec<(Vec<u8>, WeatherStationStats)> {
    let mut stations = StationTable::new();
    let mut buf = [0; CHUNK_SIZE + PADDING];
    let mut filled = 0;
    loop {
        let bytes_read = kontsa.read(&mut buf[filled..CHUNK_SIZE]).unwrap();
        filled += bytes_read;
        let at_eof = bytes_read == 0;
        // Unless at EOF, only parse lines that are guaranteed to end within the buffer
        let parse_until = if at_eof {
            filled
        } else {
            filled.saturating_sub(MAX_LINE_LEN)
        };
        let mut consumed = 0;
        while consumed < parse_until {
            let (station_name, measurement, line_len) = parse_line(&buf, consumed);
            stations.record(&buf, consumed, station_name, measurement);
            consumed += line_len;
        }
        if at_eof {
            break;
        }
        buf.copy_within(consumed..filled, 0);
        filled -= consumed;
    }
    stations.into_stations()
}

#[cfg(test)]
mod tests {
    use std::fs::read_to_string;

    use crate::calc;
    use crate::key_words;
    use crate::parse_line;
    use crate::parse_temperature;
    use crate::slot_index;
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
    fn parse_first_line(line: &[u8]) -> (&[u8], i64, usize) {
        let buf = [line, b"Next;-1.0\n", &[0; PADDING]].concat().leak();
        parse_line(buf, 0)
    }

    #[test]
    fn parse_line_stays_in_bounds_at_the_end_of_the_buffer() {
        for (temperature, expected) in [("1.2", 12), ("-1.2", -12), ("12.3", 123), ("-12.3", -123)]
        {
            // the last line of a file may lack its '\n', which is the furthest a load reaches
            for line in [format!("a;{temperature}\n"), format!("a;{temperature}")] {
                let buf = [line.as_bytes(), &[0; PADDING]].concat();
                let (station, measurement, _) = parse_line(&buf, 0);
                assert_eq!((station, measurement), (&b"a"[..], expected), "{line:?}");
            }
        }
    }
    macro_rules! tst_parse_line {
        ($func:ident,$line:expr,$expected:expr) => {
            #[test]
            fn $func() {
                let (station, measurement) = $expected;
                assert_eq!(
                    parse_first_line($line),
                    (station.as_bytes(), measurement, $line.len())
                )
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
        parse_line_works_negative_single_digit,
        b"StationName;-7.5\n",
        ("StationName", -75)
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
        parse_line_works_negative_minimum,
        b"StationName;-99.9\n",
        ("StationName", -999)
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
                (name.as_bytes(), -123, line.len()),
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
            assert_eq!(parse_temperature(word), (tenths, text.len()), "{text:?}");
        }
    }

    #[test]
    fn slot_index_separates_equal_changes_to_both_key_words() {
        let index = |name: &[u8]| {
            let buf = [name, &[0; PADDING]].concat();
            slot_index(key_words(&buf, 0, name.len()), &[])
        };
        let name = *b"abcdefghijklmnop";
        for i in 0..8 {
            for j in 8..16 {
                let mut changed = name;
                changed[i] ^= 3;
                changed[j] ^= 3;
                assert_ne!(index(&name), index(&changed), "bytes {i} and {j}");
            }
        }
    }

    #[test]
    fn calc_separates_long_names_sharing_a_prefix() {
        let prefix = "p".repeat(16);
        let long = "q".repeat(100);
        let input = format!(
            "{prefix}A;1.0\n{prefix}B;2.0\n{prefix};3.0\n{long};4.0\n{prefix}A;5.0\n{long};-6.0\n"
        );
        let path = std::env::temp_dir().join("brc-long-names.txt");
        std::fs::write(&path, input).unwrap();
        assert_eq!(
            calc(Some(path.to_str().unwrap().into())),
            format!("{{{prefix}=3.0/3.0/3.0, {prefix}A=1.0/3.0/5.0, {prefix}B=2.0/2.0/2.0, {long}=-6.0/-1.0/4.0}}\n")
        );
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
