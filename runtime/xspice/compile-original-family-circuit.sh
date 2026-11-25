#!/usr/bin/env bash
# Translate one independently discovered original XSPICE family model,
# without making any claims about its physical waveform correctness.
set -euo pipefail
if (( $# != 4 )); then
    printf 'Usage: %s RUST_BINARY_DIRECTORY NATIVE_WORK_DIRECTORY ELECTRICAL_CONFIG BRIDGE_MODEL_CATALOG\n' "$0" >&2
    exit 2
fi
bins="$(realpath -- "$1")"
work="$(realpath -- "$2")"
config="$(realpath -- "$3")"
catalog="$(realpath -- "$4")"
test -s "$work/original.cir"
test -s "$config"
test -s "$catalog"
for name in alb-boundary-discover alb-logical-expand alb-family-autowire; do
    test -x "$bins/$name" || { printf 'Missing required Rust compiler %s\n' "$name" >&2; exit 1; }
done
"$bins/alb-boundary-discover" \
    "$work/original.cir" "$config" "$work/discovered.json"
"$bins/alb-logical-expand" \
    "$work/original.cir" "$work/discovered.json" \
    "$work/lowered.cir" "$work/generated.inc"
"$bins/alb-family-autowire" \
    "$work/lowered.cir" "$work/generated.inc" \
    "$work/discovered.json" "$catalog" "$work/circuit.cir"
for name in discovered.json lowered.cir generated.inc circuit.cir; do
    test -s "$work/$name" || { printf 'Missing generated original family artifact %s\n' "$name" >&2; exit 1; }
done
