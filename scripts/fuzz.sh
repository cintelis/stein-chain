#!/usr/bin/env bash
# Runs the fuzz targets in fuzz/ (SPEC §9): each for a number of seconds, or
# one of them for as long as you like.
#
#   scripts/fuzz.sh                    every target, 60 s each
#   scripts/fuzz.sh 600                every target, 10 minutes each
#   scripts/fuzz.sh 0 history          one target, until it finds something
#
# A finding stops the run and is saved under fuzz/artifacts/<target>/; run
# it again with
#   cargo fuzz run <target> fuzz/artifacts/<target>/<file>
# What the fuzzer learns is kept in fuzz/corpus/ between runs (not committed).
#
# Needs Linux or macOS, the nightly compiler named in fuzz/rust-toolchain.toml
# (rustup installs it on first use) and cargo-fuzz:
#   cargo install cargo-fuzz --version 0.13.2 --locked
# On Windows, run it in the build image:
#   docker run --rm -v "$PWD":/src -w /src rust:1.98.1-bookworm \
#     sh -c 'apt-get update && apt-get install -y cmake clang && \
#            cargo install cargo-fuzz --version 0.13.2 --locked && scripts/fuzz.sh'

set -euo pipefail
cd "$(dirname "$0")/../fuzz"

seconds=${1:-60}
shift || true
if [ "$#" -gt 0 ]; then
  targets=("$@")
else
  mapfile -t targets < <(cargo fuzz list)
fi

# Leak detection is off: the targets keep keys in statics for the whole run.
for target in "${targets[@]}"; do
  echo "fuzz: $target for ${seconds} s"
  cargo fuzz run "$target" -- -max_total_time="$seconds" -detect_leaks=0 -rss_limit_mb=4096
done
echo "fuzz: no findings in ${#targets[@]} targets"
