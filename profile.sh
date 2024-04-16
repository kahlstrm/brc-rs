#!/bin/sh
set -e

cargo build --profile profiling

samply record ./target/profiling/brc-rs $1