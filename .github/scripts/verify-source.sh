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
# Use the same configured simulator source to build supplemental code models.
# A distro package with the same version number is not an ABI guarantee.
bash "$temporary/source/.github/scripts/install-reference-ngspice.sh" "$temporary/reference" --source "$RUSTC_WRAPPER"
export PATH="$temporary/reference/bin:$PATH"
cd "$temporary/source"
cargo metadata --locked --format-version 1 >/dev/null
bash .github/scripts/verify-packages.sh --metadata-only
# Formatting and Clippy use the canonical strict CI job, including its
# explicit per-target lint policy. Do not run a second, incompatible gate.
cargo test --locked --workspace --all-targets
# These tests deliberately require an independent native simulator; running
# ordinary cargo test alone skips all of them.
cargo test --locked -p model-kernel-vdmos --test ngspice47 -- --ignored
cargo run --locked -p model-translate-simplis --example logic_ngspice -- \
    "$temporary/reference/lib/ngspice/ngspice_preprocess_logic.cm"
cargo run --locked -p ngspice-rs-preprocess --example logic_bundle_ngspice -- \
    "$temporary/reference/lib/ngspice"
RUSTDOCFLAGS="-D warnings" cargo doc --locked --workspace --no-deps
# Production release builds use the default members; experimental crates
# remain an explicit opt-in and are tested by the workspace-wide checks above.
cargo build --locked --release
cargo build --locked -p model-alb-experimental -p model-alb-xspice-ffi --release
