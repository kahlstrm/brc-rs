#!/bin/sh
# Compares the Mojo port with the Rust version on the given file, measurements.txt by default
set -e

cargo build --quiet --release
mkdir -p target/mojo
mojo build -I mojo -o target/mojo/brc mojo/main.mojo

file=${1:-measurements.txt}
hyperfine -N --warmup 1 --runs 10 "./target/release/brc-rs $file" "./target/mojo/brc $file"
