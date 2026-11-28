#!/usr/bin/env bash
# Original directional resistance and signed current-limit vendor circuits.
set -euo pipefail
if (( $# != 4 )); then
    printf 'usage: %s CODEMODEL_CM RUST_BIN_DIR SCRATCH_ROOT EXTERNAL_MODEL_JSON\n' "$0" >&2
    exit 2
fi
here="$(cd -- "$(dirname -- "$0")" && pwd)"
cm="$(realpath -- "$1")"
bins="$(realpath -- "$2")"
scratch="$(realpath -- "$3")"
models="$(realpath -e -- "$4")"
work="$(mktemp -d "$scratch/ngspice_preprocess_directional_XXXXXXXX")"
trap 'rm -rf -- "$work"' EXIT
vendor="$here/fixtures/vendor-directional-drive"
for name in edge-directional enable-isolated enable-directional current-directional current-mean; do
    case "$name" in
        edge-directional) pins=q00,q01,q02,q10,q11,q12,q20,q21,q22; equal=q00:q10,q00:q20 ;;
        enable-*) pins=q0,q1,q2; equal="" ;;
        current-*) pins=q0,q1,q2,q3; equal=q0:q1 ;;
    esac
    "$here/run-compiled-vendor-physical.sh" "$vendor/$name.net" "$pins" "$cm" "$bins" "$work" "$models"
    awk -v name="$name" -v pins="$pins" -v equal_pairs="$equal" \
        -f "$here/check-stateful-physical-events.awk" "$vendor/vendor-events.csv" "$work/native.dat"
done
echo '5/5 original directional-drive circuits: 23 physical outputs, 129 threshold crossings'
pins='i(vforce2),i(vforce1),i(vforce0)'
"$here/run-compiled-vendor-physical.sh" "$vendor/enable-drive-history.net" "$pins" "$cm" "$bins" "$work" "$models"
awk -v name=enable-drive-history -v pins="$pins" -v sample_only=1 -v sample_tolerance=.0011 -v unit=A \
    -f "$here/check-stateful-physical-events.awk" "$vendor/vendor-drive-history-current.csv" "$work/native.dat"
pins='i(vforce8),i(vforce7),i(vforce6),i(vforce5),i(vforce4),i(vforce3),i(vforce2),i(vforce1),i(vforce0)'
for name in overlap-clamped overlap-stateless overlap-edge-clocked; do
    "$here/run-compiled-vendor-physical.sh" "$vendor/$name.net" "$pins" "$cm" "$bins" "$work" "$models"
    case "$name" in
        overlap-edge-clocked) model=sr_alb_stateful ;;
        *) model=sr_alb_lut_bit ;;
    esac
    rg -q "^\.model [^ ]+ $model\(" "$work/generated.inc"
    awk -v name="$name" -v pins="$pins" -v sample_only=1 -v sample_tolerance=.0011 -v unit=A \
        -f "$here/check-stateful-physical-events.awk" "$vendor/vendor-overlap-current.csv" "$work/native.dat"
done
name=overload-release
pins='i(vforce11),i(vforce10),i(vforce9),i(vforce8),i(vforce7),i(vforce6),i(vforce5),i(vforce4),i(vforce3),i(vforce2),i(vforce1),i(vforce0)'
"$here/run-compiled-vendor-physical.sh" "$vendor/$name.net" "$pins" "$cm" "$bins" "$work" "$models"
awk -v name="$name" -v pins="$pins" -v sample_only=1 -v sample_tolerance=.0011 -v unit=A \
    -f "$here/check-stateful-physical-events.awk" "$vendor/vendor-overlap-current.csv" "$work/native.dat"
