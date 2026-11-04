#!/bin/sh
# Release budgets (DESIGN §2) on a 2 000-message synthetic store: first MCP reply < 50 ms, one
# header search < 50 ms, one get_email < 30 ms. Prints the measured numbers; exits non-zero on a miss.
set -eu
cd "$(dirname "$0")/.."
cargo build --release
cargo test --release --test protocol -- --ignored budgets_release --nocapture
