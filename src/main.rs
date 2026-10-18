#![feature(portable_simd)]
use std::{
    collections::BTreeMap,
    fs::File,
    io::{BufRead, BufReader, Read, Seek},
    num::NonZeroUsize,
    ops::Add,
    simd::{cmp::SimdPartialEq, u8x16},
    thread,
};

fn main() {
    let file_name = std::env::args().nth(1);
    print!("{}", calc(file_name));
}
#[derive(Clone, Copy, Default, Debug, PartialEq)]
struct WeatherStationStats {
    min: i64,
    max: i64,
    sum: i64,
    count: usize,
}
impl WeatherStationStats {
    /// No measurements yet: any measurement replaces its min and max
    const EMPTY: Self = WeatherStationStats {
        min: i64::MAX,
        max: i64::MIN,
        sum: 0,
        count: 0,
    };
    fn record(&mut self, measurement: i64) {
        // new extremes get rare after a station's first rows, so branches beat loading and storing both always
        if measurement < self.min {
            std::hint::cold_path();
            self.min = measurement;
        }
        if measurement > self.max {
            std::hint::cold_path();
            self.max = measurement;
        }
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
/// Every load starts within the data, and the widest one, of a station name's first 16 bytes,
/// can reach up to 15 bytes past it, so read buffers keep 16 bytes of padding after it
const PADDING: usize = 2 * size_of::<u64>();

fn load_u64(buf: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(buf[at..at + 8].try_into().unwrap())
}

/// Finds the first ';' at or after `start`, checking 16 bytes at a time
fn find_semicolon(buf: &[u8], start: usize) -> usize {
    let mut at = start;
    loop {
        let bytes = u8x16::from_slice(&buf[at..at + INLINE_KEY_LEN]);
        let matches = bytes.simd_eq(u8x16::splat(b';')).to_bitmask();
        if matches != 0 {
            return at + matches.trailing_zeros() as usize;
        }
        at += INLINE_KEY_LEN;
    }
}

/// Masks keeping the bytes up to and including byte `len` of a name's first 16, at `16 - len..32 - len`
const NAME_MASKS: [u8; 33] = {
    let mut masks = [0; 33];
    let mut i = 0;
    while i <= INLINE_KEY_LEN {
        masks[i] = 0xFF;
        i += 1;
    }
    masks
};

/// The station name at `start`: its length, capped at 16, and its key words. Names shorter than 16 bytes,
/// nearly all of them, are found with a single 16-byte comparison, and their key words hold the ';' after them,
/// which no longer name has within its first 16 bytes, so that the key alone tells short names apart
fn parse_name(buf: &[u8], start: usize) -> (usize, [u64; 2]) {
    let bytes = u8x16::from_slice(&buf[start..start + INLINE_KEY_LEN]);
    let semicolons = bytes.simd_eq(u8x16::splat(b';')).to_bitmask() | 1 << INLINE_KEY_LEN;
    let len = semicolons.trailing_zeros() as usize;
    let mask = u8x16::from_slice(&NAME_MASKS[INLINE_KEY_LEN - len..][..INLINE_KEY_LEN]);
    let name = u128::from_le_bytes((bytes & mask).to_array());
    (len, [name as u64, (name >> 64) as u64])
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
    // wrapping: lanes parse a temperature speculatively after a long name's first 16 bytes and discard it
    let digits = unsigned.wrapping_shl(28u32.wrapping_sub(dot_bit)) & 0x0F_000F_0F00;
    // a single multiplication sums tens * 100 + ones * 10 + tenths into bits 32..42
    let abs = (digits.wrapping_mul(0x640a_0001) >> 32) & 0x3FF;
    let value = (abs as i64 ^ sign) - sign;
    (value, (dot_bit / 8) as usize + 3)
}

/// Parses the line starting at `start`, returning the station name, its key words, the measurement and the line length
fn parse_line(buf: &[u8], start: usize) -> (&[u8], [u64; 2], i64, usize) {
    let (len, key) = parse_name(buf, start);
    let semicolon = if len < INLINE_KEY_LEN {
        start + len
    } else {
        find_semicolon(buf, start + INLINE_KEY_LEN)
    };
    let (measurement, temperature_len) = parse_temperature(load_u64(buf, semicolon + 1));
    (
        &buf[start..semicolon],
        key,
        measurement,
        semicolon + 1 + temperature_len - start,
    )
}
struct Chunk {
    start_point: u64,
    len: usize,
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
fn chunk_le_file<T: BufRead + Seek>(mut f: T, file_len: usize) -> Vec<Chunk> {
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
        });
        tmp_arr.clear();
        cur_start = end_pos
    }
    res
}
fn calc(file_name: Option<String>) -> String {
    let file_name = file_name.unwrap_or("measurements.txt".into());
    let f = File::open(&file_name).unwrap();
    let file_len = f.metadata().unwrap().len() as usize;
    let chunks = chunk_le_file(BufReader::new(f), file_len);
    let stations = thread::scope(|s| {
        let handles = chunks
            .into_iter()
            .map(|c| {
                let file_name = &file_name;
                s.spawn(move || {
                    let mut f = File::open(file_name).unwrap();
                    f.seek(std::io::SeekFrom::Start(c.start_point)).unwrap();
                    aggregate_measurements(f.take(c.len as u64))
                })
            })
            .collect::<Vec<_>>();
        let mut stations = BTreeMap::new();
        for h in handles {
            let stations_välipala = h.join().unwrap();
            for (k, v) in stations_välipala {
                stations
                    .entry(k)
                    .and_modify(|jutska| *jutska = v + jutska)
                    .or_insert(v);
            }
        }
        stations
    });
    String::from("{")
        + &stations
            .iter()
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
/// Slot of the name with the key words `key`
fn slot_index(key: [u64; 2]) -> usize {
    const K: u64 = 0x517c_c1b7_2722_0a95;
    let hash = (key[0].wrapping_mul(K) ^ key[1]).wrapping_mul(K);
    (hash >> (64 - TABLE_SIZE.trailing_zeros())) as usize
}

/// Slot of `name`, of at least 16 bytes, whose first 16 bytes are the key words `key`.
/// The bytes past the key are folded in 8 at a time, ending with the last 8, so that every byte counts
fn long_name_slot_index(key: [u64; 2], name: &[u8]) -> usize {
    const K: u64 = 0x9e37_79b9_7f4a_7c15;
    let (words, _) = name[INLINE_KEY_LEN..].as_chunks::<8>();
    let last = load_u64(name, name.len() - 8);
    let rest = words
        .iter()
        .map(|word| u64::from_le_bytes(*word))
        .chain([last])
        .fold(name.len() as u64, |h, word| {
            (h ^ word).wrapping_mul(K).rotate_left(29)
        });
    slot_index([key[0], key[1] ^ rest])
}

/// Whether names of the same length of at least 16 bytes, whose first 16 bytes are known to match, are equal.
/// Compared 16 bytes at a time, and by the last 16 bytes for the rest
fn long_names_match(a: &[u8], b: &[u8]) -> bool {
    let last = |name: &[u8]| u8x16::from_slice(&name[name.len() - INLINE_KEY_LEN..]);
    let (a_chunks, _) = a[INLINE_KEY_LEN..].as_chunks::<INLINE_KEY_LEN>();
    let (b_chunks, _) = b[INLINE_KEY_LEN..].as_chunks::<INLINE_KEY_LEN>();
    let chunk = |bytes: &[u8; INLINE_KEY_LEN]| u8x16::from_array(*bytes);
    a_chunks
        .iter()
        .zip(b_chunks)
        .all(|(x, y)| chunk(x) == chunk(y))
        && last(a) == last(b)
}

/// One cache line per station, so a lookup and its update touch a single line
#[repr(align(64))]
#[derive(Clone, Copy, Default)]
struct Slot {
    key: [u64; 2],
    /// Zero marks an empty slot, as station names are at least one byte long
    len: usize,
    /// Where a name of at least 16 bytes starts in `StationTable::long_names`
    long_name_offset: usize,
    stats: WeatherStationStats,
}

/// Open-addressing table with linear probing, keyed by station name
struct StationTable {
    slots: Box<[Slot; TABLE_SIZE]>,
    long_names: Vec<u8>,
}
impl StationTable {
    fn new() -> Self {
        StationTable {
            slots: vec![Slot::default(); TABLE_SIZE]
                .into_boxed_slice()
                .try_into()
                .unwrap_or_else(|_| unreachable!()),
            long_names: Vec::new(),
        }
    }

    /// Records a measurement for `name`, whose key words are `key`
    fn record(&mut self, key: [u64; 2], name: &[u8], measurement: i64) {
        let slot = if name.len() < INLINE_KEY_LEN {
            self.find_short_slot(key, name.len())
        } else {
            self.find_long_name_slot(key, name)
        };
        slot.stats.record(measurement);
    }

    /// Finds the slot of the name shorter than the inline key with `key` and `len`, claiming an empty one for
    /// a new name. The key alone tells such names apart, as it holds the ';' after the name
    #[inline(always)]
    fn find_short_slot(&mut self, key: [u64; 2], len: usize) -> &mut Slot {
        let mut i = slot_index(key);
        loop {
            let slot = &self.slots[i];
            // compared word by word: comparing the arrays at once compiles to a 16-byte load of the key
            // just stored as two words, which stalls as the stores can't be forwarded to it
            if (slot.key[0] ^ key[0]) | (slot.key[1] ^ key[1]) == 0 {
                return &mut self.slots[i];
            }
            if slot.len == 0 {
                self.slots[i] = Slot {
                    key,
                    len,
                    long_name_offset: 0,
                    stats: WeatherStationStats::EMPTY,
                };
                return &mut self.slots[i];
            }
            i = (i + 1) % TABLE_SIZE;
        }
    }

    /// Finds the slot of `name`, of at least 16 bytes, whose first 16 bytes are `key`, claiming an empty one
    /// for a new name. Hashed by all its bytes, see `long_name_slot_index`
    #[inline(always)]
    fn find_long_name_slot(&mut self, key: [u64; 2], name: &[u8]) -> &mut Slot {
        let mut i = long_name_slot_index(key, name);
        loop {
            let slot = &self.slots[i];
            if (slot.key[0] ^ key[0]) | (slot.key[1] ^ key[1]) == 0
                && slot.len == name.len()
                && long_names_match(
                    &self.long_names[slot.long_name_offset..][..name.len()],
                    name,
                )
            {
                return &mut self.slots[i];
            }
            if slot.len == 0 {
                self.slots[i] = Slot {
                    key,
                    len: name.len(),
                    long_name_offset: self.long_names.len(),
                    stats: WeatherStationStats::EMPTY,
                };
                self.long_names.extend_from_slice(name);
                return &mut self.slots[i];
            }
            i = (i + 1) % TABLE_SIZE;
        }
    }

    fn into_stations(self) -> Vec<(Vec<u8>, WeatherStationStats)> {
        self.slots
            .iter()
            .filter(|slot| slot.len != 0)
            .map(|slot| {
                let name = if slot.len < INLINE_KEY_LEN {
                    slot.key
                        .iter()
                        .flat_map(|word| word.to_le_bytes())
                        .take(slot.len)
                        .collect()
                } else {
                    self.long_names[slot.long_name_offset..][..slot.len].to_vec()
                };
                (name, slot.stats)
            })
            .collect()
    }
}

/// Bytes per `read`. Anything from 64 KiB to 1 MiB measures the same: smaller sizes pay for more system calls,
/// and the hardware prefetcher hides cache misses on larger ones as the buffer is read front to back
const CHUNK_SIZE: usize = 1 << 19;
// The read buffer lives on its thread's stack, which `thread::spawn` makes 2 MiB
const _: () = assert!(CHUNK_SIZE + PADDING <= 1 << 20);
/// Longer than any valid line: 100 bytes of station name, ';', "-99.9" and '\n'
const MAX_LINE_LEN: usize = 128;
/// Parts of the read buffer parsed side by side. Measured: 2 overlaps the steps of two rows well,
/// 3 gains nothing more, and with 4 their values no longer fit in registers
const LANES: usize = 2;

/// Records the line at `start`, returning where the next one starts
fn record_row(buf: &[u8], stations: &mut StationTable, start: usize) -> usize {
    let (station_name, key, measurement, line_len) = parse_line(buf, start);
    stations.record(key, station_name, measurement);
    start + line_len
}

/// `record_row` for a line whose name is at least 16 bytes long, kept out of the side-by-side loop
#[cold]
#[inline(never)]
fn record_long_row(buf: &[u8], stations: &mut StationTable, start: usize) -> usize {
    let key = [load_u64(buf, start), load_u64(buf, start + 8)];
    let semicolon = find_semicolon(buf, start + INLINE_KEY_LEN);
    let (measurement, temperature_len) = parse_temperature(load_u64(buf, semicolon + 1));
    stations
        .find_long_name_slot(key, &buf[start..semicolon])
        .stats
        .record(measurement);
    semicolon + 1 + temperature_len
}

/// Splits the lines starting before `until` into `K` parts, which start and end at line starts but the last,
/// which ends at `until` and whose last line may run past it. `buf` must start at a line start, and `until / K`
/// must exceed the longest valid line, so that each split point has a '\n' after it and before `until`
fn split_lines<const K: usize>(buf: &[u8], until: usize) -> ([usize; K], [usize; K]) {
    let starts: [usize; K] = std::array::from_fn(|i| match i {
        0 => 0,
        _ => {
            let at = until * i / K;
            at + buf[at..].iter().position(|&b| b == b'\n').unwrap() + 1
        }
    });
    let ends = std::array::from_fn(|i| starts.get(i + 1).copied().unwrap_or(until));
    (starts, ends)
}

/// Records the lines of `K` parts side by side, one step at a time across all of them, so that the CPU
/// overlaps the dependent steps of their rows. Returns where each part's parsing stopped
fn parse_lanes<const K: usize>(
    buf: &[u8],
    stations: &mut StationTable,
    mut cursors: [usize; K],
    ends: [usize; K],
) -> [usize; K] {
    while (0..K).all(|i| cursors[i] < ends[i]) {
        let names = cursors.map(|at| parse_name(buf, at));
        let temperatures: [_; K] =
            std::array::from_fn(|i| parse_temperature(load_u64(buf, cursors[i] + names[i].0 + 1)));
        for i in 0..K {
            let (len, key) = names[i];
            cursors[i] = if len < INLINE_KEY_LEN {
                stations
                    .find_short_slot(key, len)
                    .stats
                    .record(temperatures[i].0);
                cursors[i] + len + 1 + temperatures[i].1
            } else {
                record_long_row(buf, stations, cursors[i])
            };
        }
    }
    for i in 0..K {
        while cursors[i] < ends[i] {
            cursors[i] = record_row(buf, stations, cursors[i]);
        }
    }
    cursors
}
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
        // the precondition of `split_lines`
        let consumed = if parse_until >= LANES * MAX_LINE_LEN {
            let (starts, ends) = split_lines::<LANES>(&buf, parse_until);
            parse_lanes(&buf, &mut stations, starts, ends)[LANES - 1]
        } else {
            let mut start = 0;
            while start < parse_until {
                start = record_row(&buf, &mut stations, start);
            }
            start
        };
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

    use crate::calc;
    use crate::long_name_slot_index;
    use crate::parse_lanes;
    use crate::parse_line;
    use crate::parse_temperature;
    use crate::record_row;
    use crate::slot_index;
    use crate::split_lines;
    use crate::thread_count;
    use crate::StationTable;
    use crate::WeatherStationStats;
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
    fn parse_first_line(line: &[u8]) -> (&[u8], [u64; 2], i64, usize) {
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
                let (station, _, measurement, _) = parse_line(&buf, 0);
                assert_eq!((station, measurement), (&b"a"[..], expected), "{line:?}");
            }
        }
    }
    macro_rules! tst_parse_line {
        ($func:ident,$line:expr,$expected:expr) => {
            #[test]
            fn $func() {
                let (station, measurement) = $expected;
                let (name, _, parsed, line_len) = parse_first_line($line);
                assert_eq!(
                    (name, parsed, line_len),
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
        ("StationName", -3)
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
            let name: Vec<u8> = (b'a'..=b'z').cycle().take(len).collect();
            let line = [&name[..], b";-12.3\n"].concat();
            let mut inline = [0; 16];
            inline[..len.min(16)].copy_from_slice(&name[..len.min(16)]);
            if len < 16 {
                inline[len] = b';';
            }
            let key = [
                u64::from_le_bytes(inline[..8].try_into().unwrap()),
                u64::from_le_bytes(inline[8..].try_into().unwrap()),
            ];
            assert_eq!(
                parse_first_line(&line),
                (&name[..], key, -123, line.len()),
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
        let index = |name: &[u8]| slot_index(parse_first_line(&[name, b";1.0\n"].concat()).1);
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

    /// The stations from parsing the lines of `buf` starting before `until` in `K` parts side by side,
    /// and where parsing stopped
    fn parse_in_lanes<const K: usize>(
        buf: &[u8],
        until: usize,
    ) -> (Vec<(Vec<u8>, WeatherStationStats)>, usize) {
        let mut stations = StationTable::new();
        let (starts, ends) = split_lines::<K>(buf, until);
        let consumed = parse_lanes(buf, &mut stations, starts, ends)[K - 1];
        let mut result = stations.into_stations();
        result.sort_by(|a, b| a.0.cmp(&b.0));
        (result, consumed)
    }

    #[test]
    fn parse_lanes_matches_parsing_one_line_at_a_time() {
        let data = std::fs::read("samples/measurements-1m.txt").unwrap();
        let buf = [&data[..], &[0; PADDING]].concat();
        // stop mid-way through a line, like the read loop does before refilling its buffer
        let until = data.len() - 1000;
        let mut stations = StationTable::new();
        let mut consumed = 0;
        while consumed < until {
            consumed = record_row(&buf, &mut stations, consumed);
        }
        let mut expected = stations.into_stations();
        expected.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(
            parse_in_lanes::<1>(&buf, until),
            (expected.clone(), consumed)
        );
        assert_eq!(
            parse_in_lanes::<2>(&buf, until),
            (expected.clone(), consumed)
        );
        assert_eq!(parse_in_lanes::<3>(&buf, until), (expected, consumed));
    }

    #[test]
    fn parse_lanes_parses_up_to_a_last_line_without_a_newline() {
        let data = std::fs::read("samples/measurements-1m.txt").unwrap();
        let data = data.strip_suffix(b"\n").unwrap();
        let buf = [data, &[0; PADDING]].concat();
        let mut stations = StationTable::new();
        let mut consumed = 0;
        while consumed < data.len() {
            consumed = record_row(&buf, &mut stations, consumed);
        }
        let mut expected = stations.into_stations();
        expected.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(
            parse_in_lanes::<2>(&buf, data.len()),
            (expected.clone(), consumed)
        );
        assert_eq!(parse_in_lanes::<3>(&buf, data.len()), (expected, consumed));
    }

    #[test]
    fn long_name_slot_index_spreads_names_alike_at_both_ends() {
        let names = (0..10_000).map(|i| format!("{}{i:05}{}", "P".repeat(16), "S".repeat(8)));
        let slots: std::collections::HashSet<_> = names
            .map(|name| {
                let key = parse_first_line(&[name.as_bytes(), b";1.0\n"].concat()).1;
                long_name_slot_index(key, name.as_bytes())
            })
            .collect();
        assert!(slots.len() > 9_000, "{} distinct slots", slots.len());
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
}
