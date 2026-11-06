#!/bin/sh
# The README "Measured" figures: release build, synthetic store of 100 000 messages
# (IMPORTIN_MEASURE_MESSAGES to change), one warm-up launch, median of five. Prints the table.
set -eu
cd "$(dirname "$0")/.."
echo "machine: $(sysctl -n machdep.cpu.brand_string), macOS $(sw_vers -productVersion), $(date +%Y-%m-%d)"
cargo build --release
cargo test --release --test protocol -- --ignored measure_release --nocapture
