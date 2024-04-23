use std::{
    collections::HashMap,
    fs::File,
    io::{BufRead, BufReader},
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
fn parse_line(line: &str) -> (&str, i64) {
    // we know that the measurement is pure ASCII and is at max 5 characters long
    // based on this we can find the semicolon faster by doing at most 6 byte comparisons by iterating the reversed bytes
    // At the same time, we _are_ iterating through the measurement from the least significant character to the biggest
    let mut semicolon_idx = 0;
    let mut is_negative = false;
    let mut measurement = 0;
    for (idx, b) in line.bytes().rev().take(6).enumerate() {
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
            _ => unreachable!(),
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

fn calc(file_name: Option<String>) -> String {
    let f = File::open(file_name.as_deref().unwrap_or("measurements.txt")).unwrap();
    let reader = BufReader::new(f);
    let stations = aggregate_measurements(reader);

    let mut res = stations.into_iter().collect::<Vec<_>>();
    res.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    String::from("{")
        + &res
            .into_iter()
            .map(|(station, stats)| {
                format!(
                    "{}={:.1}/{:.1}/{:.1}",
                    station,
                    stats.min as f64 / 10.0,
                    stats.mean(),
                    stats.max as f64 / 10.0
                )
            })
            .collect::<Vec<_>>()
            .join(", ")
        + &String::from("}\n")
}

fn aggregate_measurements(mut reader: impl BufRead) -> HashMap<String, WeatherStationStats> {
    let mut stations: HashMap<String, WeatherStationStats> = HashMap::new();

    // TODO: create an iterator out of the chunked reader logic, requires some lifetime magic
    let mut kontsa = String::new();

    reader.read_to_string(&mut kontsa).unwrap();
    kontsa.lines().for_each(|line| {
        let (station, measurement) = parse_line(line);
        match stations.get_mut(station) {
            None => {
                stations.insert(
                    station.to_string(),
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
    });
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
                assert_eq!(parse_line($line), $expected)
            }
        };
    }
    tst_parse_line!(
        parse_line_works_negative_double_digit,
        "StationName;-12.3",
        ("StationName", -123)
    );
    tst_parse_line!(
        parse_line_works_negative_only_decimal,
        "StationName;-0.3",
        ("StationName", -03)
    );
    tst_parse_line!(
        parse_line_works_positive_single_digit,
        "StationName;3.0",
        ("StationName", 30)
    );
    tst_parse_line!(
        parse_line_works_positive_only_decimal,
        "StationName;0.6",
        ("StationName", 6)
    );
    tst_parse_line!(
        parse_line_works_positive_double_digit,
        "StationName;99.9",
        ("StationName", 999)
    );

    macro_rules! tst {
        ($func:ident,$file_name:expr) => {
            #[test]
            fn $func() {
                println!($file_name);
                let res = read_to_string(format!("{}.out", $file_name)).unwrap();
                assert_eq!(calc(Some(format!("{}.txt", $file_name))), res)
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
