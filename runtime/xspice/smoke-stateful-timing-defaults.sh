#!/usr/bin/env bash
# Source-identical model-level timing defaults, explicit overrides, and
# complete waveform pair checks against independent vendor controls.
set -euo pipefail
if (( $# != 3 )); then
    printf 'usage: %s CODEMODEL_CM RUST_BIN_DIR SCRATCH_ROOT\n' "$0" >&2
    exit 2
fi
here="$(cd -- "$(dirname -- "$0")" && pwd)"
cm="$(realpath -- "$1")"
bins="$(realpath -- "$2")"
scratch="$(realpath -- "$3")"
work="$(mktemp -d "$scratch/ngspice_preprocess_timing_defaults_XXXXXXXX")"
trap 'rm -rf -- "$work"' EXIT
vendor="$here/fixtures/vendor-timing-defaults"
for name in hold-default minclock-default setup-default comb-defaults edge-setup-default-applicability level-hold-default-applicability level-minclock-default-applicability; do
    case "$name" in
        hold-default|minclock-default|setup-default)
            pins=q0,q1,q2,q3
            equal=q0:q3,q1:q2
            distinct=q0:q1
            ;;
        *)
            pins=q0,q1
            equal=q0:q1
            distinct=""
            ;;
    esac
    "$here/run-compiled-vendor-physical.sh" "$vendor/$name.net" "$pins" "$cm" "$bins" "$work"
    awk -v name="$name" -v pins="$pins" -v equal_pairs="$equal" -v distinct_pairs="$distinct" \
        -f "$here/check-stateful-physical-events.awk" "$vendor/vendor-events.csv" "$work/native.dat"
done
echo '7/7 original vendor timing-default matrices: 20 physical outputs, 30 crossings, 10 equal and 3 distinct waveform pairs'
