#!/usr/bin/env bash
# Direct original ADC->DAC source with explicit bridge parameters.
set -euo pipefail
if (( $# != 2 )); then
    printf 'Usage: %s MODEL_CM SCRATCH_ROOT\n' "$0" >&2
    exit 2
fi
here="$(cd -- "$(dirname -- "$0")" && pwd)"
cm="$(realpath -- "$1")"
scratch="$(realpath -- "$2")"
vendor="$here/fixtures/vendor-adc-clamp"
work="$(mktemp -d "$scratch/ngspice_preprocess_adc_clamp_XXXXXXXX")"
trap 'rm -rf -- "$work"' EXIT
printf 'codemodel %s\n' "$cm" >"$work/.spiceinit"
pins=""
for ((pin=11; pin>=0; pin--)); do
    pins="${pins:+$pins,}i(vforce$pin)"
done
for name in clamp-matrix clamp-temp100; do
    "$here/run-explicit-bridge-vendor-physical.sh" "$vendor/$name.net" "$pins" "$cm" "$work"
    printf '%s: ' "$name"
    awk -v name=clamp -v pins="$pins" -v sample_only=1 -v sample_tolerance=1e-5 -v unit=A \
        -f "$here/check-stateful-physical-events.awk" "$vendor/vendor-current.csv" "$work/native.dat"
done
