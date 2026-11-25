#!/usr/bin/env bash
# Original vendor resistance/load/PORT delay matrices. Explicit vendor
# DACs are mapped to verified opaque native X wrappers, preserving all
# electrical parameter values and the original ALB source/model statements.
set -euo pipefail
if (( $# != 3 )); then
    printf 'usage: %s CODEMODEL_CM RUST_BIN_DIR SCRATCH_ROOT\n' "$0" >&2
    exit 2
fi
here="$(cd -- "$(dirname -- "$0")" && pwd)"
cm="$(realpath -- "$1")"
bins="$(realpath -- "$2")"
scratch="$(realpath -- "$3")"
work="$(mktemp -d "$scratch/ngspice_preprocess_output_load_XXXXXXXX")"
trap 'rm -rf -- "$work"' EXIT
vendor="$here/fixtures/vendor-output-load"
for name in res-load-matrix level-load-matrix comb-load-matrix port-explicit-zero-load port-implicit-load; do
    "$here/run-compiled-vendor-physical.sh" "$vendor/$name.net" q00,q01,q02,q10,q11,q12,q20,q21,q22 "$cm" "$bins" "$work"
    quiet=""
    if [[ "$name" == *-load-matrix ]]; then quiet=q22; fi
    awk -v name="$name" -v pins=q00,q01,q02,q10,q11,q12,q20,q21,q22 -v quiet_pins="$quiet" \
        -f "$here/check-stateful-physical-events.awk" "$vendor/vendor-events.csv" "$work/native.dat"
done
echo '5/5 original vendor output-timing matrices: 45/45 physical output traces, 132 reference crossings'
