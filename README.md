# One Billion Row Challenge in Rust

This repository contains my implementation in Rust for the One Billion Row Challenge (1BRC), which tests the limits of processing one billion rows from a text file. [Original challenge repository](https://github.com/gunnarmorling/1brc)

The main idea is to explore optiziming performance of a program through profiling and parallellism, with also trying to use only ["Safe Rust"](https://doc.rust-lang.org/nomicon/meet-safe-and-unsafe.html#meet-safe-and-unsafe). All error handling of this project is handled poorly knowingly, and only the happy path is considered.

## Challenge

The text file contains temperature values for a range of weather stations.
Each row is one measurement in the format `<string: station name>;<double: measurement>`, with the measurement value having exactly one fractional digit.
The following shows ten rows as an example:

```
Hamburg;12.0
Bulawayo;8.9
Palembang;38.8
St. John's;15.2
Cracow;12.6
Bridgetown;26.9
Istanbul;6.2
Roseau;34.4
Conakry;31.2
Istanbul;23.0
```

The task is to write a program which reads the file, calculates the min, mean, and max temperature value per weather station, and emits the results on stdout like this
(i.e. sorted alphabetically by station name, and the result values per station in the format `<min>/<mean>/<max>`, rounded to one fractional digit):

```
{Abha=-23.0/18.0/59.2, Abidjan=-16.2/26.0/67.3, Abéché=-10.0/29.4/69.0, Accra=-10.1/26.4/66.4, Addis Ababa=-23.7/16.0/67.0, Adelaide=-27.8/17.3/58.5, ...}
```

## Getting Started

### Prerequisites

Install [Rust](https://rustup.rs/), [samply](https://github.com/mstange/samply) for profiling, [hyperfine](https://github.com/sharkdp/hyperfine) for benchmarks

### Building the Project

Compile the project in release mode for optimal performance:

```sh
cargo build --release
```

### Running Tests

To run tests, especially designed to handle smaller datasets for quick feedback:

```sh
cargo test
```

This tests the example files inside `samples`-directory

### Generating measurements

To use the profiling and benchmarking, it's useful to generate a bigger file for measurable differences.
Usage for creating a `measurements.txt` with 1 billion measurements

```sh
./create_measurements.sh 1000000000
```

Usage for creating a `measurements-1m.txt` with 1 million measurements

```sh
./create_measurements.sh 1000000 measurements-1m.txt
```

**NOTE**: The file with 1 billion measurements is over 10 GB in size.

### Profiling

```sh
./profile.sh <file-name-without-extension>
```

This script runs the program with profiling enabled using samply, and opens the profile in Firefox Profiler in browser after the program exits.

### Benchmarks

```sh
./bench.sh <file-name-without-extension>
```

This script uses hyperfine to measure an average run time of 10 runs, with no warmup.

**NOTE**: For profiling and benchmarking, it is recommended to generate a bigger input file to see measurable differences. see [Generating measurements](#generating-measurements)

## Optimization Results

Here is a table for a quick summary of the current progress of the optimizations.
All tests are run using a 10-core 14" Macbook M1 Max 32 GB

| Version                             | Time (mean ± σ): | Improvement | Notes                                                     |
| ----------------------------------- | ---------------- | ----------- | --------------------------------------------------------- |
| [Initial Version](#initial-version) | 149.403 ± 0.452  | N/A         | Naive single-core implementation with BufReader & HashMap |
|                                     |                  |             |                                                           |

### Initial Version

Nothing special really, just a quick version to get things going with a `HashMap<String,WeatherStationStats`.

Struggled too long with getting a correct implementation of the rounding calculation to pass the original test suite.

Going forward, the rounding conventions will change, with the test files being generated with the initial version of this project with only the conversion logic changed.

```sh
~/src/github/brc-rs (master*) » ./bench.sh
Benchmark 1: ./target/release/brc-rs
  Time (mean ± σ):     149.403 s ±  0.452 s    [User: 144.499 s, System: 2.486 s]
  Range (min … max):   149.037 s … 150.110 s    5 runs
```

![Flame Graph of initial implementation](initial.png)
