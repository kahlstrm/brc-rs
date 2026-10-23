"""The One Billion Row Challenge in safe, standard-library-only Mojo: a port of the Rust solution in `src/main.rs`."""

from std.bit import count_trailing_zeros
from std.memory import OwnedPointer, bitcast, pack_bits
from std.io import RawFileDescriptor
from std.os import SEEK_END, Pipe, Process, abort, setenv
from std.sys import size_of
from std.sys.intrinsics import likely, unlikely

comptime Bytes16 = SIMD[DType.uint8, 16]
comptime INLINE_KEY_LEN = 16
"""Bytes of a station name kept in its slot's key."""
comptime MAX_LINE_LEN = 128
"""Longer than any valid line: 100 bytes of station name, ';', "-99.9" and '\\n'."""
comptime PADDING = MAX_LINE_LEN
"""A row's loads end within a line of its start, so read buffers keep a line of padding after the data: then
every row starting within it can be loaded without checking where the data ends."""
comptime LONG_TAIL_LEN = 6 * INLINE_KEY_LEN
"""Bytes of a station name past its first 16, which the challenge caps at 100 bytes, rounded up to 16."""
comptime TABLE_SIZE = 1 << 16
"""A power of two, leaving a low load factor for the 10,000 stations the challenge allows."""
comptime CHUNK_SIZE = 1 << 19
"""Bytes per `read`."""
comptime LANES = 4
"""Parts of the read buffer parsed side by side, overlapping the dependent steps of their rows."""


@fieldwise_init
struct Stats(ImplicitlyCopyable, Writable):
    """A station's measurements, in tenths of a degree, as the output shows them."""

    var min: Int64
    var max: Int64
    var sum: Int64
    var count: Int64

    def write_to(self, mut writer: Some[Writer]):
        write_tenths(writer, self.min)
        writer.write("/")
        write_rounded_tenth(writer, Float64(self.sum) / 10.0 / Float64(self.count))
        writer.write("/")
        write_tenths(writer, self.max)


def write_tenths(mut writer: Some[Writer], tenths: Int64):
    """Writes `tenths / 10` with one fractional digit."""
    var magnitude = abs(tenths)
    writer.write("-" if tenths < 0 else "", magnitude // 10, ".", magnitude % 10)


def write_rounded_tenth(mut writer: Some[Writer], value: Float64):
    """Writes `value` rounded to one fractional digit, half to even on its exact binary value, as Rust's `{:.1}`.
    """
    var bits = bitcast[DType.uint64](value)
    var exponent = Int((bits >> 52) & 0x7FF) - 1075
    var mantissa = (bits & ((1 << 52) - 1)) | (UInt64(1) << 52 if exponent > -1075 else 0)
    var scaled = mantissa * 10
    var tenths: UInt64
    if exponent >= 0:
        tenths = scaled << UInt64(exponent)
    elif exponent <= -64:
        tenths = 0
    else:
        var shift = UInt64(-exponent)
        var rest = scaled & ((UInt64(1) << shift) - 1)
        var half = UInt64(1) << (shift - 1)
        tenths = scaled >> shift
        if rest > half or (rest == half and tenths & 1 == 1):
            tenths += 1
    writer.write("-" if bits >> 63 == 1 else "", tenths // 10, ".", tenths % 10)


@inline(.always)
def subspan(buf: Span[UInt8, _], start: Int, length: Int) -> Span[UInt8, buf.origin]:
    """The `length` bytes at `start`. Checked as slicing checks, but by aborting, which the compiler knows doesn't
    return, so that it drops the slice's own checks and those of loads from the slice that it can prove in bounds.
    """
    var end = start + length
    if UInt(start) > UInt(len(buf)) or UInt(end) > UInt(len(buf)) or start > end:
        abort("out of bounds")
    return buf[start:end]


@inline(.always)
def load[width: Int](buf: Span[UInt8, _], at: Int) -> SIMD[DType.uint8, width]:
    """The `width` bytes at `at`, loaded at once: copying the bounds-checked slice compiles to a single load.
    """
    var bytes = Array[UInt8, width](fill=0)
    Span(bytes).copy_from(subspan(buf, at, width))
    return SIMD[DType.uint8, width].from_bytes(bytes)


@inline(.always)
def load_u64(buf: Span[UInt8, _], at: Int) -> UInt64:
    return bitcast[DType.uint64, 1](load[8](buf, at))


@inline(.always)
def semicolons(chunk: Bytes16) -> UInt64:
    return pack_bits[DType.uint16](chunk.eq(UInt8(ord(";")))).cast[DType.uint64]()


comptime TAIL_MASKS_AT = 48
"""Where the masks of `tail_mask` start in those of `masks`, after those of `name_mask`."""
comptime MASKS_LEN = TAIL_MASKS_AT + 128 + LONG_TAIL_LEN


def masks() -> List[UInt8]:
    """The masks `name_mask` and `tail_mask` load. Their loads stay in bounds for any length below 32 and 128,
    respectively: all that the compiler knows of lengths from `count_trailing_zeros`, which lets it drop their
    checks."""
    var masks = List[UInt8](length=MASKS_LEN, fill=0)
    for i in range(33):
        masks[i] = 0xFF
    for i in range(128):
        masks[TAIL_MASKS_AT + i] = 0xFF
    return masks^


@inline(.always)
def name_mask(masks: Span[UInt8, _], len: Int) -> Bytes16:
    """Keeps the bytes up to and including byte `len` of a name's first 16."""
    return load[INLINE_KEY_LEN](masks, 32 - len)


@fieldwise_init
struct Name(TrivialRegisterPassable):
    """The station name at a row's start: its length, capped at 16, and its key words."""

    var len: Int
    var key: SIMD[DType.uint64, 2]


@inline(.always)
def parse_name(row: Span[UInt8, _], masks: Span[UInt8, _]) -> Name:
    """Names shorter than 16 bytes, nearly all of them, are found with a single 16-byte comparison, and their key
    words hold the ';' after them, which no longer name has within its first 16 bytes, so that the key alone tells
    short names apart."""
    var bytes = load[INLINE_KEY_LEN](row, 0)
    var len = Int(count_trailing_zeros(semicolons(bytes) | (1 << INLINE_KEY_LEN)))
    return Name(len, bitcast[DType.uint64, 2](bytes & name_mask(masks, len)))


@inline(.always)
def parse_temperature(word: UInt64) -> Tuple[Int64, Int]:
    """Branchless parse of a temperature (`-99.9`..`99.9`, one fractional digit) from the 8 bytes starting at it.
    Returns the value in tenths and the length of the temperature including the trailing '\\n'.
    Based on merykitty's solution to the original Java challenge."""
    # ASCII digits have bit 4 set while '.' and '-' do not: locate the '.', which is at byte 1, 2 or 3
    var dot_bit = count_trailing_zeros(~word & 0x1010_1000)
    # all ones if the first byte is '-', zero otherwise
    var sign = bitcast[DType.int64](~word << 59) >> 63
    # clears the '-'
    var unsigned = word ^ (bitcast[DType.uint64](sign) & UInt64(ord("-")))
    # align the '.' to byte 2, putting tens, ones and tenths digits to bytes 0, 1 and 3. Masked shift: lanes parse
    # a temperature speculatively after a long name's first 16 bytes and discard it
    var digits = ((unsigned << 20) >> (dot_bit & 63)) & 0x0F00_0F0F
    # a single multiplication sums tens * 100 + ones * 10 + tenths into bits 24..34
    var magnitude = bitcast[DType.int64]((digits * 0x640A_0001) >> 24) & 0x3FF
    return ((magnitude ^ sign) - sign, Int(dot_bit // 8) + 3)


@inline(.always)
def slot_index(key: SIMD[DType.uint64, 2]) -> Int:
    comptime K: UInt64 = 0x517C_C1B7_2722_0A95
    var hash = (key[0] * K ^ key[1]) * K
    return Int(hash >> UInt64(64 - count_trailing_zeros(TABLE_SIZE)))


comptime LongTail[N: Int] = Array[Bytes16, N]
"""The `N` 16-byte chunks of a long station name after its first 16 bytes, zero past the name."""


@inline(.always)
def tail_mask(masks: Span[UInt8, _], tail_len: Int, chunk: Int) -> Bytes16:
    """Keeps the bytes of chunk `chunk` of a long name's tail that are among its first `tail_len`.
    """
    return load[INLINE_KEY_LEN](masks, TAIL_MASKS_AT + 128 - tail_len + chunk * INLINE_KEY_LEN)


@inline(.always)
def load_chunks[N: Int](buf: Span[UInt8, _], at: Int) -> LongTail[N]:
    """The `N` 16-byte chunks at `at`, bounds-checked once."""
    var bytes = subspan(buf, at, N * INLINE_KEY_LEN)
    return LongTail[N](
        fill_with_unrolled=lambda [i: Int]() -> Bytes16: load[16](bytes, i * INLINE_KEY_LEN)
    )


@fieldwise_init
struct LongName[N: Int](Movable):
    """A station name of at least 16 bytes: its length, `16 * (N + 1)` if it is not shorter, and its tail.
    """

    var len: Int
    var tail: LongTail[Self.N]


@inline(.always)
def parse_long_name[N: Int](row: Span[UInt8, _], masks: Span[UInt8, _]) -> LongName[N]:
    """The station name of at least 16 bytes at the start of `row`. The same work for any length: long names' lengths are
    spread evenly, so branches on them mispredict."""
    var chunks = load_chunks[N](row, INLINE_KEY_LEN)
    var bits = UInt128(1) << UInt128(N * INLINE_KEY_LEN)
    comptime for i in range(N):
        bits |= semicolons(chunks[i]).cast[DType.uint128]() << UInt128(i * INLINE_KEY_LEN)
    var tail_len = Int(count_trailing_zeros(bits))
    comptime for i in range(N):
        chunks[i] &= tail_mask(masks, tail_len, i)
    return LongName[N](INLINE_KEY_LEN + tail_len, chunks^)


def tail_keys() -> Array[SIMD[DType.uint64, 2], LONG_TAIL_LEN // INLINE_KEY_LEN]:
    """Keys of `long_name_slot_index`, one per 16 bytes of a long name's tail, so that their position counts.
    Each 32-bit half has a 0xFF byte, which UTF-8 never has, so that no half of a name's word cancels its key's
    to zero, which would leave the other half out of the hash."""
    var keys = Array[SIMD[DType.uint64, 2], LONG_TAIL_LEN // INLINE_KEY_LEN](fill=0)
    var state: UInt64 = 0x2545_F491_4F6C_DD1D
    for i in range(len(keys)):
        comptime for j in range(2):
            state = state * 0x5851_F42D_4C95_7F2D + 0x1405_7B7E_F767_814F
            keys[i][j] = state ^ (state >> 29) | 0xFF_0000_00FF
    return keys^


comptime TAIL_KEYS = tail_keys()


@inline(.always)
def long_name_slot_index[N: Int](key: SIMD[DType.uint64, 2], tail: LongTail[N]) -> Int:
    """Slot of the name of at least 16 bytes whose first 16 bytes are the key words `key`, with `tail` after them.
    The tail's words, each xored with its key, are mixed by multiplying their two 32-bit halves, as in the NH hash:
    the products don't depend on one another, and a single SSE2 instruction multiplies those of two words at once.
    """
    var mixed = SIMD[DType.uint64, 2](0)
    comptime for i in range(N):
        var words = bitcast[DType.uint64, 2](tail[i]) ^ materialize[TAIL_KEYS]()[i]
        mixed += (words & 0xFFFF_FFFF) * (words >> 32)
    return slot_index(key ^ mixed)


@inline(.always)
def long_tails_match[
    N: Int
](stored: Span[UInt8, _], tail: LongTail[N], tail_len: Int, masks: Span[UInt8, _]) -> Bool:
    """Whether the first `tail_len` bytes of `stored`, the tail of a name kept in `StationTable.long_names`
    followed by whatever is stored after it, are those of `tail`, compared at once."""
    var stored_chunks = load_chunks[N](stored, 0)
    var differing = Bytes16(0)
    comptime for i in range(N):
        differing |= tail[i] ^ (stored_chunks[i] & tail_mask(masks, tail_len, i))
    return differing == Bytes16(0)


@fieldwise_init
@align(32)
struct Slot(ImplicitlyCopyable):
    """Half a cache line per station, so that a lookup and its update touch a single line, and the slots of the
    10,000 stations the challenge allows fit in a core's L2 cache."""

    var key: SIMD[DType.uint64, 2]
    """A short name's bytes and the ';' after it, or a long name's first 8 bytes and `LONG_NAME_TAG`."""
    var sum: Int64
    var count: UInt32
    """Zero marks an empty slot: a station is recorded as soon as it claims one. 32 bits are enough for a billion
    rows."""
    var min: Int16
    var max: Int16

    @staticmethod
    def empty() -> Self:
        """All zeros, so that the compiler allocates the table zeroed instead of filling it: the pages of slots no
        station claims are never touched."""
        return Self(0, 0, 0, 0, 0)

    @staticmethod
    def claimed(key: SIMD[DType.uint64, 2]) -> Self:
        """No measurements yet: any measurement replaces its min and max."""
        return Self(key, 0, 0, Int16.MAX, Int16.MIN)

    @inline(.always)
    def record(mut self, measurement: Int64):
        var value = Int16(measurement)
        # new extremes get rare after a station's first rows, so branches beat loading and storing both always
        if unlikely(value < self.min):
            self.min = value
        if unlikely(value > self.max):
            self.max = value
        self.sum += measurement
        self.count += 1

    def merge(mut self, other: Self):
        self.min = min(self.min, other.min)
        self.max = max(self.max, other.max)
        self.sum += other.sum
        self.count += other.count

    def stats(self) -> Stats:
        return Stats(Int64(self.min), Int64(self.max), self.sum, Int64(self.count))


comptime LONG_NAME_TAG = UInt64(0xFF) << 56
"""In the second key word of the slot of a name of at least 16 bytes, above its length and where it is kept in
`StationTable.long_names`: UTF-8 has no 0xFF byte, which tells such slots apart from those of shorter names."""


struct StationTable(Movable):
    """Open-addressing table with linear probing, keyed by station name. Megabytes large, it is kept behind an
    `OwnedPointer` and passed by reference, whose address is then that of its slots too."""

    var slots: Array[Slot, TABLE_SIZE]
    var long_names: List[UInt8]
    """Names of at least 16 bytes one after another, and 96 zeros, so that 96 bytes can be read past any name's
    first 16."""

    def __init__(out self):
        self.slots = Array[Slot, TABLE_SIZE](fill=Slot.empty())
        self.long_names = List[UInt8](length=LONG_TAIL_LEN, fill=0)

    @inline(.always)
    def find_short_slot(mut self, name: Name) -> Int:
        """Finds the slot of the name shorter than the inline key, claiming an empty one for a new name. The key
        alone tells such names apart, as it holds the ';' after the name, which longer names' keys don't.
        """
        var i = slot_index(name.key)
        while True:
            ref slot = self.slots[i]
            if likely(((slot.key[0] ^ name.key[0]) | (slot.key[1] ^ name.key[1])) == 0):
                return i
            if unlikely(slot.count == 0):
                self.claim(i, name.key)
                return i
            i = (i + 1) & (TABLE_SIZE - 1)

    @inline(.never)
    def claim(mut self, i: Int, key: SIMD[DType.uint64, 2]):
        """Claims the empty slot `i` for a new station, out of line: inlined, the compiler forwards the empty stats
        to the row's update, and keeps the field addresses of both paths in registers."""
        self.slots[i] = Slot.claimed(key)

    @inline(.always)
    def find_long_name_slot[
        N: Int
    ](mut self, key: SIMD[DType.uint64, 2], name: LongName[N], masks: Span[UInt8, _]) -> Int:
        """Finds the slot of the name of at least 16 bytes whose first 16 bytes are `key`, claiming an empty one for
        a new name. Hashed by all its bytes, see `long_name_slot_index`. Its slot keeps its first 8 bytes and, in
        place of the next 8, where it is kept, which saves a lookup elsewhere. Of a stored name of the same length,
        bytes from its 8th to the end of `name` are compared."""
        var tag = LONG_NAME_TAG | UInt64(name.len) << 32
        var i = long_name_slot_index(key, name.tail)
        while True:
            ref slot = self.slots[i]
            if ((slot.key[0] ^ key[0]) | ((slot.key[1] ^ tag) >> 32)) == 0:
                var stored = subspan(
                    self.long_names, Int(slot.key[1] & 0xFFFF_FFFF) + 8, 8 + LONG_TAIL_LEN
                )
                if load_u64(stored, 0) == key[1] and long_tails_match(
                    stored[8:], name.tail, name.len - INLINE_KEY_LEN, masks
                ):
                    return i
            if slot.count == 0:
                var offset = len(self.long_names) - LONG_TAIL_LEN
                self.claim(i, SIMD[DType.uint64, 2](key[0], tag | UInt64(offset)))
                # the chunks whole, cut at the name's end after
                self.long_names.shrink(offset)
                self.long_names.extend(Span(key.as_bytes()))
                comptime for c in range(N):
                    self.long_names.extend(Span(name.tail[c].as_bytes()))
                self.long_names.shrink(offset + name.len)
                self.long_names.resize(offset + name.len + LONG_TAIL_LEN, 0)
                return i
            i = (i + 1) & (TABLE_SIZE - 1)

    def write_name(self, i: Int, mut bytes: List[UInt8]):
        """Appends the name of the station in the slot `i` to `bytes`."""
        var key = self.slots[i].key
        if key[1] & LONG_NAME_TAG == LONG_NAME_TAG:
            bytes.extend(
                subspan(self.long_names, Int(key[1] & 0xFFFF_FFFF), Int((key[1] >> 32) & 0xFF))
            )
        else:
            var key_bytes = key.as_bytes()
            bytes.extend(
                Span(key_bytes)[
                    : Int(count_trailing_zeros(semicolons(Bytes16.from_bytes(key_bytes))))
                ]
            )

    def stations(self) -> List[Tuple[String, Stats]]:
        var stations = List[Tuple[String, Stats]]()
        var name = List[UInt8]()
        for i in range(TABLE_SIZE):
            if self.slots[i].count != 0:
                name.clear()
                self.write_name(i, name)
                stations.append((String(from_utf8_lossy=name), self.slots[i].stats()))
        return stations^

    def encode(self) -> List[UInt8]:
        """The stations as bytes after their length: each name followed by ';' and its slot's other fields, so that
        `merge_encoded` finds them as it does a row's."""
        var bytes = List[UInt8](length=8, fill=0)
        for i in range(TABLE_SIZE):
            ref slot = self.slots[i]
            if slot.count != 0:
                self.write_name(i, bytes)
                bytes.append(UInt8(ord(";")))
                bytes.extend(Span(slot.sum.as_bytes()))
                bytes.extend(Span(slot.count.as_bytes()))
                bytes.extend(Span(slot.min.as_bytes()))
                bytes.extend(Span(slot.max.as_bytes()))
        Span(bytes)[:8].copy_from(Span(UInt64(len(bytes) - 8).as_bytes()))
        return bytes^

    def merge_encoded(mut self, encoded: Span[UInt8, _]):
        """Merges the stations `encode` encoded, after their length, which `encoded` holds with `PADDING` bytes after
        them."""
        var masks = masks()
        var at = 0
        while at < len(encoded) - PADDING:
            var slot, name_len = find_row_slot(line(encoded, at), masks, self)
            var fields = subspan(encoded, at + name_len + 1, 16)
            self.slots[slot].merge(
                Slot(
                    0,
                    bitcast[DType.int64](load_u64(fields, 0)),
                    bitcast[DType.uint32, 1](load[4](fields, 8)),
                    bitcast[DType.int16, 1](load[2](fields, 12)),
                    bitcast[DType.int16, 1](load[2](fields, 14)),
                )
            )
            at += name_len + 1 + 16


@inline(.always)
def line(buf: Span[UInt8, _], start: Int) -> Span[UInt8, buf.origin]:
    """The bytes a row starting at `start` may load, checked at once so that the row's loads need no checks.
    """
    return subspan(buf, start, MAX_LINE_LEN)


def record_row(row: Span[UInt8, _], masks: Span[UInt8, _], mut stations: StationTable) -> Int:
    """Records the line at the start of `row`, returning its length."""
    return record_named_row(row, masks, stations, parse_name(row, masks))


@inline(.always)
def record_named_row(
    row: Span[UInt8, _], masks: Span[UInt8, _], mut stations: StationTable, name: Name
) -> Int:
    """Records the line at the start of `row`, given what `parse_name` returned for it. Returns its length.
    """
    # only right for a name shorter than 16 bytes
    var measurement, temperature_len = parse_temperature(load_u64(row, name.len + 1))
    if likely(name.len < INLINE_KEY_LEN):
        var slot = stations.find_short_slot(name)
        stations.slots[slot].record(measurement)
        return name.len + 1 + temperature_len
    return record_long_row(row, masks, stations)


@inline(.never)
def record_long_row(row: Span[UInt8, _], masks: Span[UInt8, _], mut stations: StationTable) -> Int:
    """`record_row` for a line whose name is at least 16 bytes long, kept out of the side-by-side loop.
    """
    var slot, name_len = find_long_row_slot(row, masks, stations)
    var measurement, temperature_len = parse_temperature(load_u64(row, name_len + 1))
    stations.slots[slot].record(measurement)
    return name_len + 1 + temperature_len


@inline(.always)
def find_long_row_slot(
    row: Span[UInt8, _], masks: Span[UInt8, _], mut stations: StationTable
) -> Tuple[Int, Int]:
    """The slot and the length of the name of at least 16 bytes at the start of `row`. Names of up to 112 bytes are
    handled. Those shorter than 32 bytes, all the long names of the challenge's 413 stations, take a path that only
    reads their first 32 bytes."""
    var key = bitcast[DType.uint64, 2](load[INLINE_KEY_LEN](row, 0))
    var name = parse_long_name[1](row, masks)
    if name.len < 2 * INLINE_KEY_LEN:
        return (stations.find_long_name_slot(key, name, masks), name.len)
    var longer_name = parse_long_name[LONG_TAIL_LEN // INLINE_KEY_LEN](row, masks)
    return (stations.find_long_name_slot(key, longer_name, masks), longer_name.len)


def find_row_slot(
    row: Span[UInt8, _], masks: Span[UInt8, _], mut stations: StationTable
) -> Tuple[Int, Int]:
    """The slot and the length of the name at the start of `row`."""
    var name = parse_name(row, masks)
    if name.len < INLINE_KEY_LEN:
        return (stations.find_short_slot(name), name.len)
    return find_long_row_slot(row, masks, stations)


def line_len(row: Span[UInt8, _]) -> Int:
    """The length of the line at the start of `row` without its '\\n', or `MAX_LINE_LEN` if it doesn't end in it.
    """
    comptime for chunk in range(MAX_LINE_LEN // INLINE_KEY_LEN):
        var newlines = pack_bits[DType.uint16](
            load[INLINE_KEY_LEN](row, chunk * INLINE_KEY_LEN).eq(UInt8(ord("\n")))
        )
        if newlines != 0:
            return chunk * INLINE_KEY_LEN + Int(count_trailing_zeros(newlines))
    return MAX_LINE_LEN


def split_lines[K: Int](buf: Span[UInt8, _], until: Int) -> Tuple[Array[Int, K], Array[Int, K]]:
    """Splits the lines starting before `until` into `K` parts, which start and end at line starts but the last,
    which ends at `until` and whose last line may run past it. `buf` must start at a line start, and `until / K`
    must exceed the longest valid line, so that each split point has a '\n' after it and before `until`.
    """
    var starts = Array[Int, K](fill=0)
    for i in range(1, K):
        var at = until * i // K
        starts[i] = at + line_len(line(buf, at)) + 1
    var ends = Array[Int, K](fill=until)
    for i in range(K - 1):
        ends[i] = starts[i + 1]
    return (starts^, ends^)


def parse_lanes[
    K: Int
](
    buf: Span[UInt8, _],
    masks: Span[UInt8, _],
    mut stations: StationTable,
    var cursors: Array[Int, K],
    ends: Array[Int, K],
) -> Array[Int, K]:
    """Records the lines of `K` parts side by side, parsing the names of all of them before recording their rows,
    so that the CPU overlaps the dependent steps of their rows. Returns where each part's parsing stopped.
    """
    # Bounds that the compiler knows are at least a line before the buffer's end, which lets it drop the checks of
    # the rows' loads. Unsigned: it then knows that cursors below them are not negative, which a signed bound
    # doesn't tell, as it can't tell either whether adding to an `Int` wraps around
    var limits = Array[UInt, K](
        fill_with_unrolled=lambda [i: Int]() -> UInt: min(UInt(ends[i]), UInt(len(buf) - PADDING))
    )
    comptime for i in range(K):
        if UInt(ends[i]) != limits[i]:
            abort("a part ends too close to the end of the buffer")
    while True:
        var running = True
        comptime for i in range(K):
            running = running and UInt(cursors[i]) < limits[i]
        if not running:
            break
        var names = Array[Name, K](
            fill_with_unrolled=lambda [i: Int]() -> Name: parse_name(line(buf, cursors[i]), masks)
        )
        comptime for i in range(K):
            cursors[i] += record_named_row(line(buf, cursors[i]), masks, stations, names[i])
    comptime for i in range(K):
        while UInt(cursors[i]) < limits[i]:
            cursors[i] += record_row(line(buf, cursors[i]), masks, stations)
    return cursors^


def aggregate_measurements(
    file: FileHandle, length: Int, mut stations: StationTable, buf: Span[mut=True, UInt8, _]
) raises:
    """Aggregates the `length` bytes from `file`'s position, which must start a line and end one or the file, into
    `stations`. Reads them into `buf`, `CHUNK_SIZE + PADDING` bytes followed by the masks of `masks`. The buffer is
    a span, whose address the compiler keeps in a register: through a reference to a `List`, it would reload it
    after every row's store, which might change it as far as it knows."""
    var masks = buf[CHUNK_SIZE + PADDING :].as_imm()
    var filled = 0
    var remaining = length
    while True:
        var bytes_read = file.read(buf[filled : filled + min(CHUNK_SIZE - filled, remaining)])
        filled += bytes_read
        remaining -= bytes_read
        var at_eof = bytes_read == 0
        # unless at the end, only parse lines that are guaranteed to end within the buffer
        var parse_until = filled if at_eof else max(filled - MAX_LINE_LEN, 0)
        var consumed = 0
        # the precondition of `split_lines`
        if parse_until >= LANES * MAX_LINE_LEN:
            var parts = split_lines[LANES](buf, parse_until)
            consumed = parse_lanes(buf.as_imm(), masks, stations, parts[0].copy(), parts[1])[
                LANES - 1
            ]
        else:
            while consumed < parse_until:
                consumed += record_row(line(buf.as_imm(), consumed), masks, stations)
        if at_eof:
            return
        # through a copy: the borrow checker doesn't let a span be copied into its own start
        var rest = Array[UInt8, 2 * MAX_LINE_LEN](fill=0)
        Span(rest)[: filled - consumed].copy_from(buf[consumed:filled])
        buf[: filled - consumed].copy_from(Span(rest)[: filled - consumed])
        filled -= consumed


# The standard library has no threads: its thread pool's async tasks are experimental. So the file is aggregated by
# this process and worker processes, copies of this program, which send their stations back through pipes. They
# share the work through a pipe too, the chunk queue, which holds the bounds of the chunks of the file not yet
# taken: pipe reads don't interleave, so each read of a chunk's bytes takes one chunk whole, and once the pipe is
# empty, its reads find its end, as this process closes it after writing them. Taking chunks as they go, the
# processes finish together, while workers start several milliseconds after this one.

comptime WORKER_ENV = "BRC_WORKER"
"""Set in workers to "<chunk queue fd> <stations fd> <file length> <file name>". Not passed as arguments:
`Process.run` mangles arguments built at run time that are short enough to be kept inline in their `String`."""
comptime CHUNK_BOUNDS_LEN = 2 * size_of[UInt64]()
comptime MAX_CHUNKS = (1 << 16) // CHUNK_BOUNDS_LEN
"""The 64 KiB of a pipe must hold all the chunks' bounds, which this process writes before it takes any."""
comptime MIN_CHUNK_LEN = 1 << 20
comptime CHUNKS_PER_PROCESS_LEFT = 4
"""Each chunk is this share of what is left for each process: smaller chunks finish closer together, but there are
more of them, each of which costs reads of the lines its bounds cut."""


def thread_count(brc_threads: String, parallelism: Int) raises -> Int:
    """One process aggregates the file per thread, so this is both the process and the thread count.
    """
    if not brc_threads:
        return parallelism
    try:
        var count = Int(brc_threads)
        if count > 0:
            return count
    except:
        pass
    raise Error("BRC_THREADS must be a positive integer, got ", repr(brc_threads))


def chunk_bounds(file_len: Int, processes: Int) -> List[UInt8]:
    """The bounds of the chunks of the file, as the chunk queue holds them. Each chunk is a share of what is left
    after the chunks before it, so that their sizes shrink to the end, where processes finish no more than a small
    chunk apart, while there are few chunks before, each of which costs some reads that start mid-line.
    """
    var min_chunk_len = MIN_CHUNK_LEN
    while True:
        var bounds = List[UInt8]()
        var start = 0
        while start < file_len:
            var end = min(
                start
                + max((file_len - start) // (processes * CHUNKS_PER_PROCESS_LEFT), min_chunk_len),
                file_len,
            )
            bounds.extend(Span(UInt64(start).as_bytes()))
            bounds.extend(Span(UInt64(end).as_bytes()))
            start = end
        if len(bounds) <= MAX_CHUNKS * CHUNK_BOUNDS_LEN:
            return bounds^
        min_chunk_len *= 2


def line_start(
    file: FileHandle, file_len: Int, at: Int, mut rest_of_line: List[UInt8]
) raises -> Int:
    """Where the first line starting at or after `at` starts, or the file's length if none does."""
    if at <= 0 or at >= file_len:
        return min(max(at, 0), file_len)
    _ = file.seek(at - 1)
    var bytes_read = file.read(Span(rest_of_line))
    for i in range(bytes_read):
        if rest_of_line[i] == UInt8(ord("\n")):
            return at + i
    return min(at - 1 + bytes_read, file_len)


def aggregate_chunks(
    file_name: String, file_len: Int, mut queue: RawFileDescriptor
) raises -> OwnedPointer[StationTable]:
    """Aggregates the chunks of the file it takes from the chunk `queue` until it is empty, each from the first line
    starting in it to the first line starting after it."""
    var file = open(file_name, "r")
    var stations = OwnedPointer(StationTable())
    # the masks after the bytes it reads into: loads of them through the buffer's address, which the compiler keeps
    # in a register, need no other
    var buf = List[UInt8](length=CHUNK_SIZE + PADDING, fill=0)
    buf.extend(masks())
    var bounds = List[UInt8](length=CHUNK_BOUNDS_LEN, fill=0)
    var rest_of_line = List[UInt8](length=MAX_LINE_LEN, fill=0)
    while True:
        var bytes_read = queue.read_bytes(Span(bounds))
        if bytes_read == 0:
            return stations^
        if bytes_read != len(bounds):
            raise Error("read part of a chunk's bounds")
        var start = line_start(file, file_len, Int(load_u64(bounds, 0)), rest_of_line)
        var end = line_start(file, file_len, Int(load_u64(bounds, 8)), rest_of_line)
        _ = file.seek(start)
        aggregate_measurements(file, end - start, stations[], Span(buf))


@fieldwise_init
struct Job(Writable):
    """What a worker aggregates, and where it takes chunks from and sends its stations to."""

    var queue_fd: Int
    var stations_fd: Int
    var file_len: Int
    var file_name: String

    @staticmethod
    def parse(job: String) raises -> Self:
        var fields = job.split(" ", maxsplit=3)
        return Self(Int(fields[0]), Int(fields[1]), Int(fields[2]), String(fields[3]))

    def write_to(self, mut writer: Some[Writer]):
        writer.write(self.queue_fd, " ", self.stations_fd, " ", self.file_len, " ", self.file_name)


def run_worker(value: String) raises:
    """Aggregates chunks of the file as `value`, that of `WORKER_ENV`, says, and sends the stations.
    """
    var job = Job.parse(value)
    var queue = RawFileDescriptor(Int32(job.queue_fd))
    var stations = aggregate_chunks(job.file_name, job.file_len, queue)
    var pipe = RawFileDescriptor(Int32(job.stations_fd))
    pipe.write_bytes(stations[].encode())


def read_exactly(mut pipe: Pipe, mut bytes: List[UInt8], length: Int) raises:
    """Reads `length` bytes from `pipe` into the start of `bytes`."""
    var filled = 0
    while filled < length:
        var bytes_read = pipe.read_bytes(Span(bytes)[filled:length])
        if bytes_read == 0:
            raise Error("a worker exited before sending its stations")
        filled += bytes_read


struct Worker(Movable):
    var process: Process
    var pipe: Pipe
    """The worker's end of the pipe it sends its stations through."""

    def __init__(out self, program: String, var job: Job) raises:
        self.pipe = Pipe(in_close_on_exec=True, out_close_on_exec=False)
        job.stations_fd = Int(self.pipe.fd_out.value().value)
        if not setenv(WORKER_ENV, String(job)):
            raise Error("failed to set ", WORKER_ENV)
        self.process = Process.run(program, [])
        # closes this process's copy of the worker's end
        self.pipe.set_input_only()

    def receive_into(mut self, mut stations: StationTable) raises:
        """Merges the worker's stations into `stations`."""
        var header = List[UInt8](length=8, fill=0)
        read_exactly(self.pipe, header, 8)
        var length = Int(load_u64(header, 0))
        var encoded = List[UInt8](length=length + PADDING, fill=0)
        read_exactly(self.pipe, encoded, length)
        stations.merge_encoded(encoded)


def calc(file_name: String, program: String, processes: Int) raises -> String:
    """Aggregates the file `file_name` in this process and the other `processes - 1` worker processes of `program`,
    copies of it."""
    var file_len = Int(open(file_name, "r").seek(0, SEEK_END))
    # only workers take this end
    var queue = Pipe(in_close_on_exec=False, out_close_on_exec=True)
    queue.write_bytes(chunk_bounds(file_len, processes))
    queue.set_input_only()
    var queue_fd = queue.fd_in.value()
    var workers = List[Worker]()
    for _ in range(processes - 1):
        workers.append(Worker(program, Job(Int(queue_fd.value), 0, file_len, file_name)))
    var stations = aggregate_chunks(file_name, file_len, queue_fd)
    # closes the queue only now: it would close as soon as it is last used
    _ = queue^
    for i in range(len(workers)):
        workers[i].receive_into(stations[])
    var output = format_stations(stations[].stations())
    # waits for the workers to exit only now, while they did
    _ = workers^
    return output^


def format_stations(var stations: List[Tuple[String, Stats]]) -> String:
    """The challenge's output: the stations ordered by name, and their stats."""
    sort(
        Span(stations),
        lambda (a: Tuple[String, Stats], b: Tuple[String, Stats]) -> Bool: a[0] < b[0],
    )
    var output = String(capacity_bytes=len(stations) * 64)
    output.write("{")
    for i in range(len(stations)):
        output.write(", " if i > 0 else "", stations[i][0], "=", stations[i][1])
    output.write("}\n")
    return output^
