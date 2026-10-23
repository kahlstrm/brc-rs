from std.collections import Set
from std.memory import OwnedPointer
from std.os import getenv, listdir, path
from std.sys import argv
from std.tempfile import mkdtemp
from std.testing import TestSuite, assert_equal, assert_raises, assert_true

from brc import (
    INLINE_KEY_LEN,
    LONG_TAIL_LEN,
    PADDING,
    TABLE_SIZE,
    WORKER_ENV,
    StationTable,
    Stats,
    calc,
    chunk_bounds,
    format_stations,
    line,
    load_u64,
    long_name_slot_index,
    masks,
    parse_lanes,
    parse_long_name,
    parse_name,
    parse_temperature,
    record_row,
    run_worker,
    slot_index,
    split_lines,
    thread_count,
    write_rounded_tenth,
)


def padded(text: String) -> List[UInt8]:
    """The bytes of `text` followed by padding, as the read buffer holds them."""
    var bytes = List(text.as_bytes())
    bytes.resize(len(bytes) + PADDING, 0)
    return bytes^


def record_rows(buf: Span[UInt8, _], until: Int) -> Tuple[String, Int]:
    """The stations of the lines of `buf` starting before `until`, recorded one at a time, and where they end."""
    var stations = OwnedPointer(StationTable())
    var row_masks = masks()
    var at = 0
    while at < until:
        at += record_row(line(buf, at), row_masks, stations[])
    return (format_stations(stations[].stations()), at)


def record_first_line(text: String) -> Tuple[String, Int]:
    """The station of the first line of `text` and its length."""
    var buf = padded(text)
    var stations = OwnedPointer(StationTable())
    var line_len = record_row(line(buf, 0), masks(), stations[])
    return (format_stations(stations[].stations()), line_len)


def key_of(name: String) -> SIMD[DType.uint64, 2]:
    return parse_name(line(padded(name + ";1.0\n"), 0), masks()).key


def temp_file(contents: String) raises -> String:
    var file_name = path.join(mkdtemp(prefix="brc"), "measurements.txt")
    with open(file_name, "w") as file:
        file.write(contents)
    return file_name


def test_thread_count_defaults_to_parallelism() raises:
    assert_equal(thread_count("", 8), 8)


def test_thread_count_uses_override() raises:
    assert_equal(thread_count("1", 8), 1)


def test_thread_count_rejects_zero() raises:
    with assert_raises(contains="BRC_THREADS must be a positive integer"):
        _ = thread_count("0", 8)


def test_record_row_stays_in_bounds_at_the_end_of_the_buffer() raises:
    for example in [("1.2", "1.2"), ("-1.2", "-1.2"), ("12.3", "12.3"), ("-12.3", "-12.3")]:
        # the last line of a file may lack its '\n', which is the furthest a load reaches
        for text in [String("a;", example[0], "\n"), String("a;", example[0])]:
            var buf = padded(text)
            var stations = OwnedPointer(StationTable())
            _ = record_row(line(buf, 0), masks(), stations[])
            assert_equal(format_stations(stations[].stations()), String("{a=", example[1], "/", example[1], "/", example[1], "}\n"))


def test_record_row_parses_temperatures_and_names() raises:
    for example in [
        ("StationName;-12.3\n", "{StationName=-12.3/-12.3/-12.3}\n"),
        ("StationName;-0.3\n", "{StationName=-0.3/-0.3/-0.3}\n"),
        ("StationName;-7.5\n", "{StationName=-7.5/-7.5/-7.5}\n"),
        ("StationName;3.0\n", "{StationName=3.0/3.0/3.0}\n"),
        ("StationName;0.6\n", "{StationName=0.6/0.6/0.6}\n"),
        ("StationName;99.9\n", "{StationName=99.9/99.9/99.9}\n"),
        ("StationName;-99.9\n", "{StationName=-99.9/-99.9/-99.9}\n"),
        ("Søul;12.0\n", "{Søul=12.0/12.0/12.0}\n"),
    ]:
        var text = String(example[0])
        var stations, line_len = record_first_line(text + "Next;-1.0\n")
        assert_equal(stations, String(example[1]))
        assert_equal(line_len, text.byte_length())


def test_record_row_works_for_all_name_lengths() raises:
    for len in range(1, 101):
        var name = String()
        for i in range(len):
            name.write(chr(ord("a") + i % 26))
        var text = name + ";-12.3\n"
        var stations, line_len = record_first_line(text + "Next;-1.0\n")
        assert_equal(stations, String("{", name, "=-12.3/-12.3/-12.3}\n"), String("name length ", len))
        assert_equal(line_len, text.byte_length())
        if len < INLINE_KEY_LEN:
            var key_bytes = List((name + ";").as_bytes())
            key_bytes.resize(INLINE_KEY_LEN, 0)
            assert_equal(key_of(name)[0], load_u64(key_bytes, 0))
            assert_equal(key_of(name)[1], load_u64(key_bytes, 8))


def test_parse_temperature_works_for_all_values() raises:
    for tenths in range(-999, 1000):
        var text = String("-" if tenths < 0 else "", abs(tenths) // 10, ".", abs(tenths) % 10, "\n")
        var measurement, temperature_len = parse_temperature(load_u64(padded(text + "Next;"), 0))
        assert_equal(Int(measurement), tenths, text)
        assert_equal(temperature_len, text.byte_length(), text)


def test_write_rounded_tenth_rounds_half_to_even_on_the_exact_value() raises:
    # Rust's `{:.1}`: 0.05 and -0.05 are a little over a half in binary, 12.35 a little under, 0.25 exactly
    for example in [
        (0.25, "0.2"),
        (0.75, "0.8"),
        (0.05, "0.1"),
        (-0.05, "-0.1"),
        (-0.04, "-0.0"),
        (12.25, "12.2"),
        (12.35, "12.3"),
        (-99.95, "-100.0"),
        (1e-20, "0.0"),
        (-1e-20, "-0.0"),
        (0.0, "0.0"),
        (5.0, "5.0"),
    ]:
        var text = String()
        write_rounded_tenth(text, example[0])
        assert_equal(text, String(example[1]), String(example[0]))


def test_slot_index_separates_equal_changes_to_both_key_words() raises:
    var name = "abcdefghijklmnop"
    for i in range(8):
        for j in range(8, 16):
            var changed = List(name.as_bytes())
            changed[i] ^= 3
            changed[j] ^= 3
            var changed_name = String(unsafe_from_utf8=changed)
            assert_true(
                slot_index(key_of(name)) != slot_index(key_of(changed_name)), String("bytes ", i, " and ", j)
            )


def parse_in_lanes[K: Int](buf: Span[UInt8, _], until: Int) -> Tuple[String, Int]:
    """The stations from parsing the lines of `buf` starting before `until` in `K` parts side by side, and where
    parsing stopped."""
    var stations = OwnedPointer(StationTable())
    var parts = split_lines[K](buf, until)
    var consumed = parse_lanes(buf, masks(), stations[], parts[0].copy(), parts[1])[K - 1]
    return (format_stations(stations[].stations()), consumed)


def sample(name: String) raises -> String:
    with open(path.join("samples", name), "r") as file:
        return file.read()


def test_parse_lanes_matches_parsing_one_line_at_a_time() raises:
    var data = sample("measurements-1m.txt")
    var buf = padded(data)
    # stop mid-way through a line, like the read loop does before refilling its buffer
    var until = data.byte_length() - 1000
    var expected = record_rows(buf, until)
    comptime for k in range(1, 5):
        var lanes = parse_in_lanes[k](buf, until)
        assert_equal(lanes[0], expected[0], String(k, " lanes"))
        assert_equal(lanes[1], expected[1], String(k, " lanes"))


def test_parse_lanes_parses_up_to_a_last_line_without_a_newline() raises:
    var data = sample("measurements-1m.txt")
    var trimmed = String(data.removesuffix("\n"))
    data = trimmed^
    var buf = padded(data)
    var expected = record_rows(buf, data.byte_length())
    comptime for k in range(2, 5):
        var lanes = parse_in_lanes[k](buf, data.byte_length())
        assert_equal(lanes[0], expected[0], String(k, " lanes"))
        assert_equal(lanes[1], expected[1], String(k, " lanes"))


def long_name_slot_count(names: List[String]) -> Int:
    """How many slots `long_name_slot_index` spreads the station `names` over."""
    var slots = Set[Int]()
    for name in names:
        var row = line(padded(name + ";1.0\n"), 0)
        slots.add(long_name_slot_index(key_of(name), parse_long_name[LONG_TAIL_LEN // INLINE_KEY_LEN](row, masks()).tail))
    return len(slots)


def test_long_name_slot_index_spreads_names_alike_at_both_ends() raises:
    var names = List[String]()
    for i in range(10_000):
        var number = String(i)
        names.append(String("P" * 16, "0" * (5 - number.byte_length()), number, "S" * 8))
    var slots = long_name_slot_count(names)
    assert_true(slots > 9_000, String(slots, " distinct slots"))


def test_long_name_slot_index_spreads_names_differing_in_bits_of_adjacent_chunks() raises:
    for chunk in range(4):
        # a bit of one 16-byte chunk of the tail flipped together with the bit below it in the next one
        var names = List[String]()
        for i in range(10_000):
            var name = List(("a" * 96).as_bytes())
            for byte in range(14):
                if i >> byte & 1 == 1:
                    name[16 * (chunk + 1) + byte] ^= 2
                    name[16 * (chunk + 2) + byte] ^= 1
            names.append(String(unsafe_from_utf8=name))
        var slots = long_name_slot_count(names)
        assert_true(slots > 9_000, String("chunk ", chunk, ": ", slots, " distinct slots"))


def test_chunk_bounds_cover_the_file_in_shrinking_chunks() raises:
    for file_len in [0, 1, 1 << 20, 13_795_948_528]:
        for processes in [1, 8, 10_000]:
            var bounds = chunk_bounds(file_len, processes)
            var chunks = len(bounds) // 16
            assert_true(len(bounds) <= 1 << 16, String(chunks, " chunks overflow a pipe"))
            var end = 0
            var previous_len = file_len
            for chunk in range(chunks):
                var start = Int(load_u64(bounds, 16 * chunk))
                var chunk_end = Int(load_u64(bounds, 16 * chunk + 8))
                assert_equal(start, end)
                assert_true(chunk_end > start and chunk_end - start <= previous_len)
                previous_len = chunk_end - start
                end = chunk_end
            assert_equal(end, file_len)


def test_encoded_stations_merge_into_a_table() raises:
    var data = sample("measurements-10000-unique-keys.txt")
    var buf = padded(data)
    var halves = OwnedPointer(StationTable())
    var whole = OwnedPointer(StationTable())
    var row_masks = masks()
    var at = 0
    while at < data.byte_length():
        _ = record_row(line(buf, at), row_masks, whole[])
        _ = record_row(line(buf, at), row_masks, whole[])
        at += record_row(line(buf, at), row_masks, halves[])
    var encoded = halves[].encode()
    encoded.resize(len(encoded) + PADDING, 0)
    # every row once more, from the encoding
    halves[].merge_encoded(Span(encoded)[8:])
    assert_equal(format_stations(halves[].stations()), format_stations(whole[].stations()))


def test_calc_separates_long_names_sharing_a_prefix() raises:
    var prefix = "p" * 16
    var long = "q" * 100
    var file_name = temp_file(
        String(
            prefix, "A;1.0\n", prefix, "B;2.0\n", prefix, ";3.0\n", long, ";4.0\n", prefix, "A;5.0\n", long, ";-6.0\n"
        )
    )
    assert_equal(
        calc(file_name, String(argv()[0]), 1),
        String(
            "{", prefix, "=3.0/3.0/3.0, ", prefix, "A=1.0/3.0/5.0, ", prefix, "B=2.0/2.0/2.0, ", long, "=-6.0/-1.0/4.0}\n"
        ),
    )


def test_long_names_of_every_length_differing_in_their_last_byte_get_a_slot_each() raises:
    var names = List[String]()
    for len in range(16, 101):
        for last in ["a", "b"]:
            names.append(String("n" * (len - 1), last))
    var data = String()
    var expected = List[Tuple[String, Stats]]()
    # each name twice, so that different bytes follow it, which must not count
    for temperature in ["1.0", "-2.0"]:
        for name in names:
            data.write(name, ";", temperature, "\n")
    for name in names:
        expected.append((name, Stats(-20, 10, -10, 2)))
    assert_equal(record_rows(padded(data), data.byte_length())[0], format_stations(expected^))


def test_calc_prints_exactly_the_expected_output_for_every_sample() raises:
    for entry in listdir("samples"):
        if not entry.endswith(".txt"):
            continue
        var expected = sample(String(entry.removesuffix(".txt"), ".out"))
        # 3 processes start workers, copies of this program, which `main` runs
        for processes in [1, 3]:
            assert_equal(
                calc(path.join("samples", entry), String(argv()[0]), processes), expected, String(entry, ", ", processes)
            )


def main() raises:
    var job = getenv(WORKER_ENV)
    if job:
        run_worker(job)
        return
    TestSuite.discover_tests[__functions_in_module()]().run()
