#!/bin/sh
set -e

cargo build --quiet --release


hyperfine --warmup 0 --runs 5 "./target/release/brc-rs $1"
