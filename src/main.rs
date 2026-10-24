#![feature(portable_simd)]
use std::{
    collections::BTreeMap,
    fs::File,
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    num::NonZeroUsize,
    ops::{Add, Range},
    simd::{cmp::SimdPartialEq, u64x2, u8x16, ToBytes},
    sync::atomic::{AtomicU64, Ordering},
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
/// A row's loads end within a line of its start, so read buffers keep a line of padding after the data:
/// then every row starting within it can be loaded without checking where the data ends
const PADDING: usize = MAX_LINE_LEN;

fn load_u64(buf: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(buf[at..at + 8].try_into().unwrap())
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

/// Bytes of a station name past its first 16, which the challenge caps at 100 bytes, rounded up to 16
const LONG_TAIL_LEN: usize = 6 * INLINE_KEY_LEN;
const _: () = assert!(INLINE_KEY_LEN + LONG_TAIL_LEN <= PADDING);
/// The `N` 16-byte chunks of a long station name after its first 16 bytes, zero past the name
type LongTail<const N: usize> = [u8x16; N];

/// Masks keeping the first `len` bytes of a long name's tail, at `LONG_TAIL_LEN - len..2 * LONG_TAIL_LEN - len`
const TAIL_MASKS: [u8; 2 * LONG_TAIL_LEN] = {
    let mut masks = [0; 2 * LONG_TAIL_LEN];
    let mut i = 0;
    while i < LONG_TAIL_LEN {
        masks[i] = 0xFF;
        i += 1;
    }
    masks
};

/// The length of the station name of at least 16 bytes at `start`, `16 * (N + 1)` if it is not shorter, and its
/// tail. The same work for any length: long names' lengths are spread evenly, so branches on them mispredict
fn parse_long_name<const N: usize>(buf: &[u8], start: usize) -> (usize, LongTail<N>) {
    let bytes = &buf[start + INLINE_KEY_LEN..][..N * INLINE_KEY_LEN];
    let chunks: LongTail<N> =
        std::array::from_fn(|i| u8x16::from_slice(&bytes[i * INLINE_KEY_LEN..]));
    let semicolons = chunks.iter().rev().fold(0, |bits: u128, chunk| {
        bits << INLINE_KEY_LEN | chunk.simd_eq(u8x16::splat(b';')).to_bitmask() as u128
    }) | 1 << (N * INLINE_KEY_LEN);
    let tail_len = semicolons.trailing_zeros() as usize;
    let masks = tail_masks::<N>(tail_len);
    (
        INLINE_KEY_LEN + tail_len,
        std::array::from_fn(|i| chunks[i] & masks[i]),
    )
}

/// Masks of the `N` chunks of a long name's tail keeping its first `tail_len` bytes
fn tail_masks<const N: usize>(tail_len: usize) -> [u8x16; N] {
    let masks = &TAIL_MASKS[LONG_TAIL_LEN - tail_len..][..LONG_TAIL_LEN];
    std::array::from_fn(|i| u8x16::from_slice(&masks[i * INLINE_KEY_LEN..]))
}

/// Branchless parse of a temperature (`-99.9`..`99.9`, one fractional digit) from the 8 bytes starting at it.
/// Returns the value in tenths and the length of the temperature including the trailing '\n'.
/// Based on merykitty's solution to the original Java challenge.
fn parse_temperature(word: u64) -> (i64, usize) {
    // ASCII digits have bit 4 set while '.' and '-' do not: locate the '.', which is at byte 1, 2 or 3
    let dot_bit = (!word & 0x1010_1000).trailing_zeros();
    // all ones if the first byte is '-', zero otherwise
    let sign = ((!word << 59) as i64) >> 63;
    // clears the '-'
    let unsigned = word ^ (sign as u64 & b'-' as u64);
    // align the '.' to byte 2, putting tens, ones and tenths digits to bytes 0, 1 and 3: shifted right by the
    // '.''s bit, which is 4 into its byte, after shifting left by 20 bits, which keeps the last digit's byte 4.
    // Wrapping: lanes parse a temperature speculatively after a long name's first 16 bytes and discard it
    let digits = (unsigned << 20).wrapping_shr(dot_bit) & 0x0F00_0F0F;
    // a single multiplication sums tens * 100 + ones * 10 + tenths into bits 24..34
    let abs = (digits.wrapping_mul(0x640a_0001) >> 24) & 0x3FF;
    let value = (abs as i64 ^ sign) - sign;
    (value, (dot_bit / 8) as usize + 3)
}

/// The number of threads spawned, each claiming chunks from a shared `ChunkQueue` until it is empty
fn thread_count(brc_threads: Option<String>, parallelism: usize) -> usize {
    match brc_threads {
        Some(s) => s
            .parse::<NonZeroUsize>()
            .unwrap_or_else(|_| panic!("BRC_THREADS must be a positive integer, got {s:?}"))
            .get(),
        None => parallelism,
    }
}

/// Smallest chunk claimed from a `ChunkQueue`, so that the last chunks still amortize their seeks and reads
const MIN_CHUNK_LEN: u64 = 1 << 20;

/// Byte ranges of a file claimed by threads one at a time, each a quarter of the remaining bytes' share per thread,
/// but at least `min_len`. Equal chunks, one per thread, finished tens of milliseconds apart as threads ran at
/// different speeds: with chunks shrinking towards the end, the threads finishing first claim the last ones
struct ChunkQueue {
    next: AtomicU64,
    file_len: u64,
    divisor: u64,
    min_len: u64,
}
impl ChunkQueue {
    fn new(file_len: u64, thread_count: usize, min_len: u64) -> Self {
        ChunkQueue {
            next: AtomicU64::new(0),
            file_len,
            divisor: 4 * thread_count as u64,
            min_len: min_len.max(1),
        }
    }
    /// The next unclaimed range, which may start and end within lines, `None` when the file is all claimed
    fn claim(&self) -> Option<Range<u64>> {
        let chunk_end = |start: u64| {
            let len = ((self.file_len - start) / self.divisor).max(self.min_len);
            (start + len).min(self.file_len)
        };
        self.next
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |start| {
                (start < self.file_len).then(|| chunk_end(start))
            })
            .ok()
            .map(|start| start..chunk_end(start))
    }
    /// The lines starting within the next unclaimed range, `None` when the file is all claimed. Empty if no line
    /// starts within the range: the line it is in belongs to the range before
    fn claim_lines(&self, mut f: impl Read + Seek) -> Option<Range<u64>> {
        let claimed = self.claim()?;
        let start = line_start_from(&mut f, claimed.start, self.file_len);
        let end = line_start_from(&mut f, claimed.end, self.file_len);
        Some(start..end)
    }
}

/// The first line start at or after `at`, or `file_len` if there is none
fn line_start_from(mut f: impl Read + Seek, at: u64, file_len: u64) -> u64 {
    if at == 0 || at >= file_len {
        return at.min(file_len);
    }
    // a line starts at `at` if the byte before it is a '\n'
    f.seek(SeekFrom::Start(at - 1)).unwrap();
    let mut rest_of_line = Vec::with_capacity(MAX_LINE_LEN);
    let read = BufReader::with_capacity(MAX_LINE_LEN, f)
        .read_until(b'\n', &mut rest_of_line)
        .unwrap();
    at - 1 + read as u64
}

fn calc(file_name: Option<String>) -> String {
    let file_name = file_name.unwrap_or("measurements.txt".into());
    let file_len = File::open(&file_name).unwrap().metadata().unwrap().len();
    let thread_count = thread_count(
        std::env::var("BRC_THREADS").ok(),
        std::thread::available_parallelism()
            .map(NonZeroUsize::get)
            .unwrap_or(1),
    );
    let queue = ChunkQueue::new(file_len, thread_count, MIN_CHUNK_LEN);
    let stations = thread::scope(|s| {
        let handles = (0..thread_count)
            .map(|_| {
                let (file_name, queue) = (&file_name, &queue);
                s.spawn(move || {
                    let f = File::open(file_name).unwrap();
                    let mut stations = StationTable::new();
                    let mut buf = [0; CHUNK_SIZE + PADDING];
                    while let Some(lines) = queue.claim_lines(&f) {
                        (&f).seek(SeekFrom::Start(lines.start)).unwrap();
                        aggregate_measurements(
                            &mut stations,
                            &mut buf,
                            (&f).take(lines.end - lines.start),
                        );
                    }
                    stations.into_stations()
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

/// Keys of `long_name_slot_index`, one per 16 bytes of a long name's tail, so that their position counts.
/// Each 32-bit half has a 0xFF byte, which UTF-8 never has, so that no half of a name's word cancels its key's
/// to zero, which would leave the other half out of the hash
const TAIL_KEYS: [u64x2; LONG_TAIL_LEN / INLINE_KEY_LEN] = {
    let mut keys = [u64x2::from_array([0; 2]); LONG_TAIL_LEN / INLINE_KEY_LEN];
    let mut state: u64 = 0x2545_f491_4f6c_dd1d;
    let mut i = 0;
    while i < keys.len() {
        let mut words = [0; 2];
        let mut j = 0;
        while j < 2 {
            state = state
                .wrapping_mul(0x5851_f42d_4c95_7f2d)
                .wrapping_add(0x1405_7b7e_f767_814f);
            words[j] = state ^ (state >> 29) | 0xFF_0000_00FF;
            j += 1;
        }
        keys[i] = u64x2::from_array(words);
        i += 1;
    }
    keys
};

/// Slot of the name of at least 16 bytes whose first 16 bytes are the key words `key`, with `tail` after them.
/// The tail's words, each xored with its key, are mixed by multiplying their two 32-bit halves, as in the NH hash:
/// the products don't depend on one another, and a single SSE2 instruction multiplies those of two words at once
fn long_name_slot_index<const N: usize>(key: [u64; 2], tail: &LongTail<N>) -> usize {
    let mixed = tail
        .iter()
        .zip(TAIL_KEYS)
        .fold(u64x2::splat(0), |sum, (chunk, chunk_key)| {
            let words = u64x2::from_ne_bytes(*chunk) ^ chunk_key;
            sum + (words & u64x2::splat(0xFFFF_FFFF)) * (words >> 32)
        });
    slot_index([key[0] ^ mixed[0], key[1] ^ mixed[1]])
}

/// Whether the first `tail_len` bytes of `stored`, the tail of a name kept in `StationTable::long_names` followed
/// by whatever is stored after it, are those of `tail`, compared at once
fn long_tails_match<const N: usize>(stored: &[u8], tail: &LongTail<N>, tail_len: usize) -> bool {
    let differing = tail
        .iter()
        .zip(stored.as_chunks().0)
        .zip(tail_masks::<N>(tail_len))
        .fold(u8x16::splat(0), |differing, ((chunk, stored), mask)| {
            differing | (chunk ^ (u8x16::from_array(*stored) & mask))
        });
    differing == u8x16::splat(0)
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
    /// Names of at least 16 bytes one after another, and 96 zeros, so that 96 bytes can be read past any name's
    /// first 16. Packed rather than each padded to 112 bytes, they take less than half the cache
    long_names: Vec<u8>,
}
impl StationTable {
    fn new() -> Self {
        StationTable {
            slots: vec![Slot::default(); TABLE_SIZE]
                .into_boxed_slice()
                .try_into()
                .unwrap_or_else(|_| unreachable!()),
            long_names: vec![0; LONG_TAIL_LEN],
        }
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

    /// Finds the slot of the name of `len` bytes, at least 16, whose first 16 bytes are `key`, with `tail` after
    /// them up to its end, claiming an empty one for a new name. Hashed by all its bytes, see
    /// `long_name_slot_index`. Of a stored name of the same length, only the bytes `tail` keeps are compared
    #[inline(always)]
    fn find_long_name_slot<const N: usize>(
        &mut self,
        key: [u64; 2],
        len: usize,
        tail: &LongTail<N>,
    ) -> &mut Slot {
        let mut i = long_name_slot_index(key, tail);
        loop {
            let slot = &self.slots[i];
            if (slot.key[0] ^ key[0]) | (slot.key[1] ^ key[1]) | (slot.len ^ len) as u64 == 0
                && long_tails_match(
                    &self.long_names[slot.long_name_offset + INLINE_KEY_LEN..]
                        [..N * INLINE_KEY_LEN],
                    tail,
                    len - INLINE_KEY_LEN,
                )
            {
                return &mut self.slots[i];
            }
            if slot.len == 0 {
                let offset = self.long_names.len() - LONG_TAIL_LEN;
                self.slots[i] = Slot {
                    key,
                    len,
                    long_name_offset: offset,
                    stats: WeatherStationStats::EMPTY,
                };
                let key_bytes = key.iter().flat_map(|word| word.to_le_bytes());
                let tail_bytes = tail.iter().flat_map(|chunk| chunk.to_array());
                // the chunks whole, cut at the name's end after: taking `len` bytes copies them one at a time
                self.long_names.truncate(offset);
                self.long_names.extend(key_bytes.chain(tail_bytes));
                self.long_names.truncate(offset + len);
                self.long_names.resize(offset + len + LONG_TAIL_LEN, 0);
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
/// Parts of the read buffer parsed side by side. Measured: 3 overlaps the dependent steps of rows better than 2,
/// and with 4 too many values no longer fit in registers
const LANES: usize = 3;

/// Records the line at `start`, returning where the next one starts
fn record_row(buf: &[u8], stations: &mut StationTable, start: usize) -> usize {
    record_named_row(buf, stations, start, parse_name(buf, start))
}

/// Records the line at `start`, given what `parse_name` returned for it. Returns where the next line starts
#[inline(always)]
fn record_named_row(
    buf: &[u8],
    stations: &mut StationTable,
    start: usize,
    (len, key): (usize, [u64; 2]),
) -> usize {
    // only right for a name shorter than 16 bytes
    let (measurement, temperature_len) = parse_temperature(load_u64(buf, start + len + 1));
    if len < INLINE_KEY_LEN {
        stations.find_short_slot(key, len).stats.record(measurement);
        start + len + 1 + temperature_len
    } else {
        record_long_row(buf, stations, start)
    }
}

/// `record_row` for a line whose name is at least 16 bytes long, kept out of the side-by-side loop.
/// Names of up to 112 bytes are handled. Those shorter than 32 bytes, all the long names of the challenge's
/// 413 stations, take a path that only reads their first 32 bytes
#[cold]
#[inline(never)]
fn record_long_row(buf: &[u8], stations: &mut StationTable, start: usize) -> usize {
    let (len, tail) = parse_long_name::<1>(buf, start);
    if len < 2 * INLINE_KEY_LEN {
        record_long_name_row(buf, stations, start, len, &tail)
    } else {
        let (len, tail) = parse_long_name::<{ LONG_TAIL_LEN / INLINE_KEY_LEN }>(buf, start);
        record_long_name_row(buf, stations, start, len, &tail)
    }
}

/// Records the line at `start`, whose name of `len` bytes, at least 16, ends within `tail`
#[inline(always)]
fn record_long_name_row<const N: usize>(
    buf: &[u8],
    stations: &mut StationTable,
    start: usize,
    len: usize,
    tail: &LongTail<N>,
) -> usize {
    let key = [load_u64(buf, start), load_u64(buf, start + 8)];
    let (measurement, temperature_len) = parse_temperature(load_u64(buf, start + len + 1));
    stations
        .find_long_name_slot(key, len, tail)
        .stats
        .record(measurement);
    start + len + 1 + temperature_len
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

/// Records the lines of `K` parts side by side, parsing the names of all of them before recording their rows, so
/// that the CPU overlaps the dependent steps of their rows. Each row's temperature is only parsed as it is recorded,
/// which leaves fewer values to keep in registers in between. Returns where each part's parsing stopped
fn parse_lanes<const K: usize>(
    buf: &[u8],
    stations: &mut StationTable,
    mut cursors: [usize; K],
    ends: [usize; K],
) -> [usize; K] {
    // lets the compiler drop the bounds checks of the rows' loads
    assert!(ends.iter().all(|&end| end + PADDING <= buf.len()));
    while (0..K).all(|i| cursors[i] < ends[i]) {
        let names = cursors.map(|at| parse_name(buf, at));
        for i in 0..K {
            cursors[i] = record_named_row(buf, stations, cursors[i], names[i]);
        }
    }
    for i in 0..K {
        while cursors[i] < ends[i] {
            cursors[i] = record_row(buf, stations, cursors[i]);
        }
    }
    cursors
}
/// Records the lines `kontsa` reads in `stations`, read into `buf`, which a thread reuses for all its chunks
fn aggregate_measurements(
    stations: &mut StationTable,
    buf: &mut [u8; CHUNK_SIZE + PADDING],
    mut kontsa: impl Read,
) {
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
            let (starts, ends) = split_lines::<LANES>(buf, parse_until);
            parse_lanes(buf, stations, starts, ends)[LANES - 1]
        } else {
            let mut start = 0;
            while start < parse_until {
                start = record_row(buf, stations, start);
            }
            start
        };
        if at_eof {
            break;
        }
        buf.copy_within(consumed..filled, 0);
        filled -= consumed;
    }
}

#[cfg(test)]
mod tests {

    use crate::calc;
    use crate::long_name_slot_index;
    use crate::parse_lanes;
    use crate::parse_long_name;
    use crate::parse_name;
    use crate::parse_temperature;
    use crate::record_row;
    use crate::slot_index;
    use crate::split_lines;
    use crate::thread_count;
    use crate::ChunkQueue;
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
    /// Records the first line of `buf`, returning its station name, measurement and length
    fn record_first_row(buf: &[u8]) -> (Vec<u8>, i64, usize) {
        let mut stations = StationTable::new();
        let line_len = record_row(buf, &mut stations, 0);
        let [(name, stats)] = stations.into_stations().try_into().unwrap();
        (name, stats.sum, line_len)
    }

    /// Records the first line of `line` followed by another line and padding, as in the read buffer
    fn record_first_line(line: &[u8]) -> (Vec<u8>, i64, usize) {
        record_first_row(&[line, b"Next;-1.0\n", &[0; PADDING]].concat())
    }

    /// The key words of the station `name`
    fn key_of(name: &[u8]) -> [u64; 2] {
        parse_name(&[name, b";1.0\n", &[0; PADDING]].concat(), 0).1
    }

    #[test]
    fn parse_line_stays_in_bounds_at_the_end_of_the_buffer() {
        for (temperature, expected) in [("1.2", 12), ("-1.2", -12), ("12.3", 123), ("-12.3", -123)]
        {
            // the last line of a file may lack its '\n', which is the furthest a load reaches
            for line in [format!("a;{temperature}\n"), format!("a;{temperature}")] {
                let buf = [line.as_bytes(), &[0; PADDING]].concat();
                let (station, measurement, _) = record_first_row(&buf);
                assert_eq!(
                    (&station[..], measurement),
                    (&b"a"[..], expected),
                    "{line:?}"
                );
            }
        }
    }
    macro_rules! tst_parse_line {
        ($func:ident,$line:expr,$expected:expr) => {
            #[test]
            fn $func() {
                let (station, measurement) = $expected;
                let (name, parsed, line_len) = record_first_line($line);
                assert_eq!(
                    (&name[..], parsed, line_len),
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
                (record_first_line(&line), key_of(&name)),
                ((name, -123, line.len()), key),
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
        let index = |name: &[u8]| slot_index(key_of(name));
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

    /// How many slots `long_name_slot_index` spreads the station `names` over
    fn long_name_slot_count(names: impl Iterator<Item = Vec<u8>>) -> usize {
        let slots: std::collections::HashSet<_> = names
            .map(|name| {
                let buf = [&name[..], b";1.0\n", &[0; PADDING]].concat();
                long_name_slot_index(key_of(&name), &parse_long_name::<6>(&buf, 0).1)
            })
            .collect();
        slots.len()
    }

    #[test]
    fn long_name_slot_index_spreads_names_alike_at_both_ends() {
        let names =
            (0..10_000).map(|i| format!("{}{i:05}{}", "P".repeat(16), "S".repeat(8)).into_bytes());
        let slots = long_name_slot_count(names);
        assert!(slots > 9_000, "{slots} distinct slots");
    }

    #[test]
    fn long_name_slot_index_spreads_names_differing_in_bits_of_adjacent_chunks() {
        for chunk in 0..4 {
            // a bit of one 16-byte chunk of the tail flipped together with the bit below it in the next one
            let names = (0..10_000).map(|i: u32| {
                let mut name = b"a".repeat(96);
                for byte in (0..14).filter(|byte| i >> byte & 1 == 1) {
                    name[16 * (chunk + 1) + byte] ^= 2;
                    name[16 * (chunk + 2) + byte] ^= 1;
                }
                name
            });
            let slots = long_name_slot_count(names);
            assert!(slots > 9_000, "chunk {chunk}: {slots} distinct slots");
        }
    }

    #[test]
    fn chunk_queue_covers_the_file_with_line_ranges_claimed_from_any_threads() {
        let input: Vec<u8> = (0..50)
            .flat_map(|i| format!("{}{i};{i}.0\n", "s".repeat(i % 7)).into_bytes())
            .collect();
        let len = input.len() as u64;
        for (count, min_len) in [
            (1, 1),
            (2, 1),
            (3, 7),
            (8, 1),
            (8, 64),
            (50, 1),
            (3, 1 << 20),
        ] {
            let queue = ChunkQueue::new(len, count, min_len);
            let mut ranges: Vec<_> = std::thread::scope(|s| {
                let handles: Vec<_> = (0..count)
                    .map(|_| {
                        s.spawn(|| {
                            let mut f = std::io::Cursor::new(&input);
                            std::iter::from_fn(|| queue.claim_lines(&mut f)).collect::<Vec<_>>()
                        })
                    })
                    .collect();
                handles
                    .into_iter()
                    .flat_map(|h| h.join().unwrap())
                    .collect()
            });
            ranges.retain(|range| !range.is_empty());
            ranges.sort_by_key(|range| range.start);
            let context = format!("{count} threads, {min_len}: {ranges:?}");
            assert_eq!(ranges.first().map(|r| r.start), Some(0), "{context}");
            assert_eq!(ranges.last().map(|r| r.end), Some(len), "{context}");
            assert!(
                ranges.windows(2).all(|w| w[0].end == w[1].start),
                "{context}"
            );
            assert!(
                ranges.iter().all(|r| input[r.end as usize - 1] == b'\n'),
                "{context}"
            );
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

    #[test]
    fn long_names_of_every_length_differing_in_their_last_byte_get_a_slot_each() {
        let names: Vec<Vec<u8>> = (16..=100)
            .flat_map(|len| b"ab".map(|last| [b"n".repeat(len - 1), vec![last]].concat()))
            .collect();
        // each name twice, so that different bytes follow it, which must not count
        let data: Vec<u8> = [&b"1.0"[..], b"-2.0"]
            .iter()
            .flat_map(|temperature| {
                names
                    .iter()
                    .map(|name| [name, &b";"[..], temperature, b"\n"].concat())
            })
            .flatten()
            .collect();
        let buf = [&data[..], &[0; PADDING]].concat();
        let mut stations = StationTable::new();
        let mut start = 0;
        while start < data.len() {
            start = record_row(&buf, &mut stations, start);
        }
        let mut recorded: Vec<_> = stations
            .into_stations()
            .into_iter()
            .map(|(name, stats)| (name, stats.min, stats.max, stats.count))
            .collect();
        recorded.sort();
        let mut expected: Vec<_> = names.into_iter().map(|name| (name, -20, 10, 2)).collect();
        expected.sort();
        assert_eq!(recorded, expected);
    }
}
