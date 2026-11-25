#!/usr/bin/env bash
set -euo pipefail
if (( $# != 2 )); then
    printf 'Usage: %s NGSPICE_47_BUILD_TREE OUTPUT_DIRECTORY\n' "$0" >&2
    exit 2
fi
source_dir="$(cd -- "$(dirname -- "$0")" && pwd)"
repo="$(realpath -- "$source_dir/../..")"
out="$(realpath -m -- "$2")"
mkdir -p -- "$out"
scratch="$(mktemp -d "$out/.stateful-rust-XXXXXXXX")"
trap 'rm -rf -- "$scratch"' EXIT
export CARGO_TARGET_DIR="$scratch/target"
export RUSTC_WRAPPER=/usr/local/libexec/sccache-client
[[ -x "$RUSTC_WRAPPER" ]] || { echo "Required existing sccache not available" >&2; exit 1; }
(cd "$repo" && cargo build --locked --release -p model-alb-xspice-ffi --target-dir "$scratch/target")
archive="$scratch/target/release/libmodel_alb_xspice_ffi.a"
[[ -s "$archive" ]] || { echo "Rust staticlib not produced: $archive" >&2; exit 1; }
"$source_dir/build.sh" "$1" "$out" "$archive"
