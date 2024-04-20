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
    min: f64,
    max: f64,
    sum: f64,
    count: usize,
}
impl WeatherStationStats {
    fn mean(&self) -> f64 {
        self.sum / self.count as f64
    }
}
fn parse_line(mut line: String) -> (String, f64) {
    let semicolon_idx = line.find(";").unwrap();
    let measurement: f64 = (&line[semicolon_idx + 1..]).parse().unwrap();
    line.truncate(semicolon_idx);
    (line, measurement)
}
fn calc(file_name: Option<String>) -> String {
    let f = File::open(format!(
        "{}.txt",
        file_name.as_deref().unwrap_or("measurements")
    ))
    .unwrap();
    let reader = BufReader::new(f);
    let lines = reader.lines();
    let mut res = lines
        .fold(
            HashMap::new(),
            |mut map: HashMap<String, WeatherStationStats>, line| {
                let line = line.unwrap();
                let (station, measurement) = parse_line(line);
                map.entry(station)
                    .and_modify(|s| {
                        s.max = s.max.max(measurement);
                        s.min = s.min.min(measurement);
                        s.count += 1;
                        s.sum += measurement;
                    })
                    .or_insert(WeatherStationStats {
                        min: measurement,
                        max: measurement,
                        sum: measurement,
                        count: 1,
                    });
                map
            },
        )
        .into_iter()
        .collect::<Vec<_>>();
    res.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    String::from("{")
        + &res
            .into_iter()
            .map(|(station, stats)| {
                format!(
                    "{}={:.1}/{:.1}/{:.1}",
                    station,
                    stats.min,
                    stats.mean(),
                    stats.max
                )
            })
            .collect::<Vec<_>>()
            .join(", ")
        + &String::from("}\n")
}

#[cfg(test)]
mod tests {
    use std::fs::read_to_string;

    use crate::calc;
    macro_rules! tst {
        ($func:ident,$file_name:expr) => {
            #[test]
            fn $func() {
                println!($file_name);
                let res = read_to_string(format!("{}.out", $file_name)).unwrap();
                assert_eq!(calc(Some($file_name.into())), res)
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
