#!/usr/bin/env bash
# Compile source-identical vendor UNKNOWN data/clock/enable/reset history
# fixtures through automatic physical boundary discovery.
set -euo pipefail
if (( $# != 3 )); then
    printf 'usage: %s CODEMODEL_CM RUST_BIN_DIR SCRATCH_ROOT\n' "$0" >&2
    exit 2
fi
here="$(cd -- "$(dirname -- "$0")" && pwd)"
cm="$(realpath -- "$1")"
bins="$(realpath -- "$2")"
scratch="$(realpath -- "$3")"
work="$(mktemp -d "$scratch/ngspice_preprocess_unknown_XXXXXXXX")"
trap 'rm -rf -- "$work"' EXIT
vendor="$here/fixtures/vendor-unknown-history"
for name in unknown-data-clock unknown-qualifiers; do
    if [[ "$name" == unknown-data-clock ]]; then
        pins=qd,qe,ql,qc,qt,qtr
    else
        pins=qh,qm,qr,qs
    fi
    "$here/run-compiled-vendor-physical.sh" "$vendor/$name.net" "$pins" "$cm" "$bins" "$work"
    rg -q "^Asr_stateful " "$work/generated.inc"
    awk -v name="$name" -v pins="$pins" -f "$here/check-stateful-physical-events.awk" "$vendor/vendor-events.csv" "$work/native.dat"
done
echo '2/2 original UNKNOWN-state matrices: 10 physical outputs, 31 reference crossings'
