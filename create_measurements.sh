#!/bin/sh
set -e

cargo r --quiet --bin create_measurements --features=generate --release $@