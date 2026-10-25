#![feature(portable_simd)]
use std::{
    fs::File,
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    num::NonZeroUsize,
    ops::Range,
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
    fn mean(&self) -> f64 {
        self.sum as f64 / 10.0 / self.count as f64
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

/// Bytes per chunk claimed from a `ChunkQueue`. Measured: 2 to 16 MiB are equally fast. 1 MiB was 1% slower, as
/// each chunk costs reads of the lines its bounds cut and a last, partly filled buffer, and 64 MiB was 6% slower on
/// the 10,000-station set, where a thread claiming the last chunk late kept the others waiting
const CHUNK_LEN: u64 = 4 << 20;

/// Byte ranges of a file, `chunk_len` bytes each but the last, claimed by threads one at a time. With one equal
/// chunk per thread, threads running at different speeds finished tens of milliseconds apart: taking chunks as
/// they go, they finish within one chunk's time of each other
struct ChunkQueue {
    next: AtomicU64,
    file_len: u64,
    chunk_len: u64,
}
impl ChunkQueue {
    fn new(file_len: u64, chunk_len: u64) -> Self {
        ChunkQueue {
            next: AtomicU64::new(0),
            file_len,
            chunk_len: chunk_len.max(1),
        }
    }
    /// The next unclaimed range, which may start and end within lines, `None` when the file is all claimed
    fn claim(&self) -> Option<Range<u64>> {
        let start = self.next.fetch_add(self.chunk_len, Ordering::Relaxed);
        (start < self.file_len).then(|| start..(start + self.chunk_len).min(self.file_len))
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
    let queue = ChunkQueue::new(file_len, CHUNK_LEN);
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
                    stations
                })
            })
            .collect::<Vec<_>>();
        handles
            .into_iter()
            .map(|h| h.join().unwrap())
            .reduce(|mut stations, stations_välipala| {
                stations.merge(&stations_välipala);
                stations
            })
            .unwrap_or_else(StationTable::new)
    });
    let mut output = vec![b'{'];
    for (i, station) in stations.sorted_stations().iter().enumerate() {
        if i > 0 {
            output.extend_from_slice(b", ");
        }
        station.write_name(&mut output);
        output.push(b'=');
        write_tenths(
            &mut output,
            station.stats.min < 0,
            station.stats.min.unsigned_abs(),
        );
        output.push(b'/');
        let (negative, mean) = round_to_tenths(station.stats.mean());
        write_tenths(&mut output, negative, mean);
        output.push(b'/');
        write_tenths(
            &mut output,
            station.stats.max < 0,
            station.stats.max.unsigned_abs(),
        );
    }
    output.extend_from_slice(b"}\n");
    String::from_utf8_lossy(&output).into_owned()
}

/// Writes `tenths` as a decimal with one fractional digit, as `{:.1}` writes the number it is the tenths of
fn write_tenths(output: &mut Vec<u8>, negative: bool, tenths: u64) {
    if negative {
        output.push(b'-');
    }
    let whole = tenths / 10;
    let digits = whole.checked_ilog10().unwrap_or(0) + 1;
    output.extend(
        (0..digits)
            .rev()
            .map(|i| b'0' + (whole / 10u64.pow(i) % 10) as u8),
    );
    output.extend_from_slice(&[b'.', b'0' + (tenths % 10) as u8]);
}

/// Whether `value` is negative and its magnitude in tenths, rounded as `{:.1}` rounds it: its exact binary value
/// to the nearest tenth, ties to even. Exact rather than through `value * 10.0`, whose own rounding may tip it over
/// a half. `value` must be below 2^52 in magnitude, which a mean of temperatures always is
fn round_to_tenths(value: f64) -> (bool, u64) {
    let bits = value.to_bits();
    let biased_exponent = (bits >> 52 & 0x7FF) as i64;
    let mantissa = bits & ((1 << 52) - 1) | ((biased_exponent != 0) as u64) << 52;
    // value = mantissa * 2^-shift; past 64, ten times the mantissa, below 2^57, is under half of 2^-shift
    let shift = 1075 - biased_exponent.max(1);
    assert!(shift > 0, "{value} is not below 2^52 in magnitude");
    let shift = shift.min(64) as u32;
    let scaled = 10 * mantissa as u128;
    let truncated = scaled >> shift;
    let remainder = scaled - (truncated << shift);
    let half = 1 << (shift - 1);
    let rounds_up = remainder > half || remainder == half && truncated & 1 == 1;
    (
        value.is_sign_negative(),
        (truncated + rounds_up as u128) as u64,
    )
}

/// Bytes of a station name stored inline in its slot and compared as two words
const INLINE_KEY_LEN: usize = 2 * size_of::<u64>();
/// A power of two, leaving a low load factor for the 10,000 stations the challenge allows. 4 MiB, which measured fastest:
/// smaller tables collide more, and bigger ones were slower too
const TABLE_SIZE: usize = 1 << 17;
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

/// Half a cache line per station, so that a lookup and its update touch a single line, and twice as many slots fit
/// in the memory of a table
#[repr(C, align(32))]
#[derive(Clone, Copy, Default)]
struct Slot {
    /// A short name's bytes and the ';' after it, or a long name's first 8 bytes and its `LONG_NAME_TAG` word
    key: [u64; 2],
    /// Zero marks an empty slot: a station is recorded as soon as it claims one. 32 bits are enough for a billion
    /// rows
    count: u32,
    min: i16,
    max: i16,
    sum: i64,
}
const _: () = assert!(size_of::<Slot>() == 32);
impl Slot {
    /// No measurements yet: any measurement replaces its min and max
    fn claimed(key: [u64; 2]) -> Self {
        Slot {
            key,
            sum: 0,
            count: 0,
            min: i16::MAX,
            max: i16::MIN,
        }
    }
    fn record(&mut self, measurement: i64) {
        let value = measurement as i16;
        // new extremes get rare after a station's first rows, so branches beat loading and storing both always
        if value < self.min {
            std::hint::cold_path();
            self.min = value;
        }
        if value > self.max {
            std::hint::cold_path();
            self.max = value;
        }
        self.sum += measurement;
        self.count += 1;
    }
    fn stats(&self) -> WeatherStationStats {
        WeatherStationStats {
            min: self.min.into(),
            max: self.max.into(),
            sum: self.sum,
            count: self.count as usize,
        }
    }
    fn add(&mut self, other: &Slot) {
        self.min = self.min.min(other.min);
        self.max = self.max.max(other.max);
        self.sum += other.sum;
        self.count += other.count;
    }
    fn is_long(&self) -> bool {
        self.key[1] & LONG_NAME_TAG == LONG_NAME_TAG
    }
    fn name_len(&self) -> usize {
        if self.is_long() {
            (self.key[1] >> 32 & 0xFF) as usize
        } else {
            let bytes = self.key.iter().flat_map(|word| word.to_le_bytes());
            bytes.take_while(|&byte| byte != b';').count()
        }
    }
}

/// In the second key word of the slot of a name of at least 16 bytes, above its length and where it starts in
/// `StationTable::long_names`: UTF-8 has no 0xFF byte, so no short name's key has it
const LONG_NAME_TAG: u64 = 0xFF << 56;

/// Where the name of at least 16 bytes with the second key word `tagged` starts in `StationTable::long_names`
fn long_name_offset(tagged: u64) -> usize {
    (tagged & 0xFFFF_FFFF) as usize
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

    /// Finds the slot of the name shorter than the inline key with `key`, claiming an empty one for a new name.
    /// The key alone tells such names apart, as it holds the ';' after the name
    #[inline(always)]
    fn find_short_slot(&mut self, key: [u64; 2]) -> &mut Slot {
        let mut i = slot_index(key);
        loop {
            let slot = &self.slots[i];
            // compared at once, with a single 16-byte load: in 64-byte slots, whose key followed the stats, that load
            // stalled behind the stores to them, while word by word it did not
            if slot.key == key {
                return &mut self.slots[i];
            }
            if slot.count == 0 {
                self.slots[i] = Slot::claimed(key);
                return &mut self.slots[i];
            }
            i = (i + 1) % TABLE_SIZE;
        }
    }

    /// Finds the slot of the name of `len` bytes, at least 16, whose first 16 bytes are `key`, with `tail` after
    /// them up to its end, claiming an empty one for a new name. Hashed by all its bytes, see
    /// `long_name_slot_index`. Its slot keeps its first 8 bytes and, in place of the next 8, its length and where
    /// it is stored. Of a stored name of the same length, its bytes from the 8th up to the end `tail` keeps are
    /// compared
    #[inline(always)]
    fn find_long_name_slot<const N: usize>(
        &mut self,
        key: [u64; 2],
        len: usize,
        tail: &LongTail<N>,
    ) -> &mut Slot {
        let tag = LONG_NAME_TAG | (len as u64) << 32;
        let mut i = long_name_slot_index(key, tail);
        loop {
            let slot = &self.slots[i];
            if (slot.key[0] ^ key[0]) | (slot.key[1] ^ tag) >> 32 == 0 {
                let stored = &self.long_names[long_name_offset(slot.key[1]) + 8..];
                if load_u64(stored, 0) == key[1]
                    && long_tails_match(
                        &stored[8..][..N * INLINE_KEY_LEN],
                        tail,
                        len - INLINE_KEY_LEN,
                    )
                {
                    return &mut self.slots[i];
                }
            }
            if slot.count == 0 {
                let offset = self.long_names.len() - LONG_TAIL_LEN;
                self.slots[i] = Slot::claimed([key[0], tag | offset as u64]);
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

    fn occupied_slots(&self) -> impl Iterator<Item = &Slot> {
        self.slots.iter().filter(|slot| slot.count != 0)
    }

    /// The first 16 bytes of the name in `slot`, which a long name's slot keeps only the first 8 of
    fn name_key(&self, slot: &Slot) -> [u64; 2] {
        if slot.is_long() {
            let stored = &self.long_names[long_name_offset(slot.key[1])..];
            [slot.key[0], load_u64(stored, 8)]
        } else {
            slot.key
        }
    }

    /// The tail of the long name in `slot` as `parse_long_name::<N>` returns it, zero past the name
    fn stored_tail<const N: usize>(&self, slot: &Slot) -> LongTail<N> {
        let stored = &self.long_names[long_name_offset(slot.key[1]) + INLINE_KEY_LEN..];
        let masks = tail_masks::<N>(slot.name_len() - INLINE_KEY_LEN);
        std::array::from_fn(|i| u8x16::from_slice(&stored[i * INLINE_KEY_LEN..]) & masks[i])
    }

    /// Adds the stations of `other` to this table's, finding each as a row of its name would.
    /// Long names take the same number of chunks as in `record_long_row`, which their slot depends on
    fn merge(&mut self, other: &StationTable) {
        for slot in other.occupied_slots() {
            let (key, len) = (other.name_key(slot), slot.name_len());
            let merged = if len < INLINE_KEY_LEN {
                self.find_short_slot(key)
            } else if len < 2 * INLINE_KEY_LEN {
                self.find_long_name_slot(key, len, &other.stored_tail::<1>(slot))
            } else {
                let tail = other.stored_tail::<{ LONG_TAIL_LEN / INLINE_KEY_LEN }>(slot);
                self.find_long_name_slot(key, len, &tail)
            };
            merged.add(slot);
        }
    }

    /// The stations sorted by name. Sorted by their first 16 bytes as a number, which tells nearly all names
    /// apart without reaching for the bytes of long names
    fn sorted_stations(&self) -> Vec<Station<'_>> {
        let mut stations: Vec<_> = self
            .occupied_slots()
            .map(|slot| {
                let [low, high] = self.name_key(slot);
                let key = u128::from(low) | u128::from(high) << 64;
                let len = slot.name_len();
                let (prefix, long_name) = if len < INLINE_KEY_LEN {
                    // without the ';' after the name, so that a name sorts before those it is the start of
                    (key & !(u128::MAX << (8 * len)), &[][..])
                } else {
                    (
                        key,
                        &self.long_names[long_name_offset(slot.key[1])..][..len],
                    )
                };
                Station {
                    prefix: prefix.swap_bytes(),
                    long_name,
                    len,
                    stats: slot.stats(),
                }
            })
            .collect();
        stations.sort_unstable_by(|a, b| {
            (a.prefix, a.long_name, a.len).cmp(&(b.prefix, b.long_name, b.len))
        });
        stations
    }
}

/// A station of `StationTable::sorted_stations`
struct Station<'a> {
    /// The name's first 16 bytes, zero past a shorter name, as a big-endian number
    prefix: u128,
    /// The whole name if it is at least 16 bytes long, empty otherwise
    long_name: &'a [u8],
    len: usize,
    stats: WeatherStationStats,
}
impl Station<'_> {
    fn write_name(&self, output: &mut Vec<u8>) {
        if self.long_name.is_empty() {
            output.extend_from_slice(&self.prefix.to_be_bytes()[..self.len]);
        } else {
            output.extend_from_slice(self.long_name);
        }
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
        stations.find_short_slot(key).record(measurement);
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
    use crate::round_to_tenths;
    use crate::slot_index;
    use crate::split_lines;
    use crate::thread_count;
    use crate::write_tenths;
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
    /// The stations of `stations` sorted by name, each with its name
    fn named_stations(stations: &StationTable) -> Vec<(Vec<u8>, WeatherStationStats)> {
        let sorted = stations.sorted_stations();
        let named = sorted.iter().map(|station| {
            let mut name = Vec::new();
            station.write_name(&mut name);
            (name, station.stats)
        });
        named.collect()
    }

    /// Records the first line of `buf`, returning its station name, measurement and length
    fn record_first_row(buf: &[u8]) -> (Vec<u8>, i64, usize) {
        let mut stations = StationTable::new();
        let line_len = record_row(buf, &mut stations, 0);
        let [(name, stats)] = named_stations(&stations).try_into().unwrap();
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

    /// `value` as `write_tenths` writes it after `round_to_tenths`
    fn tenths_text(value: f64) -> String {
        let (negative, tenths) = round_to_tenths(value);
        let mut output = Vec::new();
        write_tenths(&mut output, negative, tenths);
        String::from_utf8(output).unwrap()
    }

    #[test]
    fn round_to_tenths_writes_means_as_formatting_does() {
        // exact halves, such as 5 / 10 / 2, and values just off them
        for count in 1..=200 {
            for sum in -2_000i64..=2_000 {
                let mean = WeatherStationStats {
                    min: 0,
                    max: 0,
                    sum,
                    count,
                }
                .mean();
                assert_eq!(tenths_text(mean), format!("{mean:.1}"), "{sum} / {count}");
            }
        }
        for value in [
            0.0, -0.0, 0.05, -0.05, 0.25, -0.25, 99.95, -99.95, 1e-300, -1e-300, 5e-324,
        ] {
            assert_eq!(tenths_text(value), format!("{value:.1}"), "{value:e}");
        }
    }

    #[test]
    fn merge_matches_recording_every_row_into_one_table() {
        let data = std::fs::read("samples/measurements-10000-unique-keys.txt").unwrap();
        let buf = [&data[..], &[0; PADDING]].concat();
        let record = |range: std::ops::Range<usize>| {
            let mut stations = StationTable::new();
            let mut start = range.start;
            while start < range.end {
                start = record_row(&buf, &mut stations, start);
            }
            stations
        };
        let half = data.len() / 2;
        let middle = half + data[half..].iter().position(|&b| b == b'\n').unwrap() + 1;
        let mut merged = record(0..middle);
        merged.merge(&record(middle..data.len()));
        merged.merge(&record(0..middle));
        let mut expected = record(0..data.len());
        expected.merge(&record(0..middle));
        assert_eq!(named_stations(&merged), named_stations(&expected));
    }

    #[test]
    fn sorted_stations_sorts_names_by_their_bytes() {
        let names = [
            "b",
            "a",
            "ab",
            "a!",
            "aaaaaaaaaaaaaaaa",
            "aaaaaaaaaaaaaaa",
            "aaaaaaaaaaaaaaaab",
        ];
        let data: Vec<u8> = names
            .iter()
            .flat_map(|name| format!("{name};1.0\n").into_bytes())
            .collect();
        let buf = [&data[..], &[0; PADDING]].concat();
        let mut stations = StationTable::new();
        let mut start = 0;
        while start < data.len() {
            start = record_row(&buf, &mut stations, start);
        }
        let mut expected: Vec<_> = names.map(|name| name.as_bytes().to_vec()).into();
        expected.sort();
        let sorted: Vec<_> = named_stations(&stations)
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        assert_eq!(sorted, expected);
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
        (named_stations(&stations), consumed)
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
        let expected = named_stations(&stations);
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
        let expected = named_stations(&stations);
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
        for (count, chunk_len) in [
            (1, 1),
            (2, 1),
            (3, 7),
            (8, 1),
            (8, 64),
            (50, 1),
            (3, 1 << 20),
        ] {
            let queue = ChunkQueue::new(len, chunk_len);
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
            let context = format!("{count} threads, {chunk_len}: {ranges:?}");
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
        let recorded: Vec<_> = named_stations(&stations)
            .into_iter()
            .map(|(name, stats)| (name, stats.min, stats.max, stats.count))
            .collect();
        let mut expected: Vec<_> = names.into_iter().map(|name| (name, -20, 10, 2)).collect();
        expected.sort();
        assert_eq!(recorded, expected);
    }

    #[test]
    fn long_names_differing_only_in_their_second_8_bytes_get_a_slot_each() {
        // enough names that their probes pass over one another's slots, whose keys keep only the first 8 bytes
        let names: Vec<Vec<u8>> = (0..10_000)
            .map(|i| format!("{}{i:08}{}", "p".repeat(8), "s".repeat(4)).into_bytes())
            .collect();
        let data: Vec<u8> = names
            .iter()
            .flat_map(|name| [name, &b";1.0\n"[..]].concat())
            .collect();
        let buf = [&data[..], &[0; PADDING]].concat();
        let mut stations = StationTable::new();
        let mut start = 0;
        while start < data.len() {
            start = record_row(&buf, &mut stations, start);
        }
        let recorded: Vec<_> = named_stations(&stations)
            .into_iter()
            .map(|(name, stats)| (name, stats.count))
            .collect();
        assert_eq!(
            recorded,
            names.into_iter().map(|name| (name, 1)).collect::<Vec<_>>()
        );
    }
}
