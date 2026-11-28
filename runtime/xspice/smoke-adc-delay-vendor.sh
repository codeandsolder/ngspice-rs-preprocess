#!/usr/bin/env bash
# Original ADC propagation delays and cancellation at the UNKNOWN band.
set -euo pipefail
if (( $# != 2 )); then
    printf 'Usage: %s MODEL_CM SCRATCH_ROOT\n' "$0" >&2
    exit 2
fi
here="$(cd -- "$(dirname -- "$0")" && pwd)"
model="$(realpath -- "$1")"
scratch="$(realpath -- "$2")"
vendor="$here/fixtures/vendor-adc-delay"
work="$(mktemp -d "$scratch/ngspice_preprocess_adc_delay_XXXXXXXX")"
trap 'rm -rf -- "$work"' EXIT
pins='i(vforce5),i(vforce4),i(vforce3),i(vforce2),i(vforce1),i(vforce0)'
for name in adc-interrupted adc-unknown-direction adc-schmitt-interrupted adc-delay-equality adc-delay-equality-coarse adc-delay-boundary; do
    "$here/run-explicit-bridge-vendor-physical.sh" "$vendor/$name.net" "$pins" "$model" "$work"
    awk -v name="$name" -v pins="$pins" -v sample_tolerance=.0011 -v unit=A \
        -f "$here/check-stateful-physical-events.awk" "$vendor/vendor-events.csv" "$work/native.dat"
done
