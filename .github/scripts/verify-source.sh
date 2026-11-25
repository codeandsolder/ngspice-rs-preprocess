#!/usr/bin/env bash
# Build and test exactly the tracked release sources, without Git metadata.
set -euo pipefail
root=$(git rev-parse --show-toplevel)
: "${RUSTC_WRAPPER:?mandatory managed sccache wrapper is required}"
# The shared workflow sets RUNNER_TEMP and CARGO_TARGET_DIR; a local worker
# supplies its own isolated target directory and may use the same script.
: "${CARGO_TARGET_DIR:?isolated Cargo target directory is required}"
[[ "$CARGO_TARGET_DIR" == /* ]]
# Preserve intermediates in the isolated target across the consecutive checks.
export CARGO_BUILD_EPHEMERAL_INTERMEDIATES=false
temporary=$(mktemp -d "${RUNNER_TEMP:-$(dirname "$CARGO_TARGET_DIR")}/preprocess-ci.XXXXXXXX")
trap 'rm -rf -- "$temporary"' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
mkdir "$temporary/source"
git -C "$root" archive --format=tar HEAD | tar -xf - -C "$temporary/source"
bash "$temporary/source/.github/scripts/install-reference-ngspice.sh" "$temporary/reference"
export PATH="$temporary/reference/bin:$PATH"
cd "$temporary/source"
cargo metadata --locked --format-version 1 >/dev/null
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace --all-targets
# These tests deliberately require an independent native simulator; running
# ordinary cargo test alone skips all of them.
cargo test --locked -p model-kernel-vdmos --test ngspice47 -- --ignored
RUSTDOCFLAGS="-D warnings" cargo doc --locked --workspace --no-deps
cargo build --locked --workspace --release
