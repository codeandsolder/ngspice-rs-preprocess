#!/usr/bin/env bash
# Original physical terminal witnesses, with and without explicit observation.
set -euo pipefail
if (( $# != 3 )); then
    printf 'usage: %s CODEMODEL_CM RUST_BIN_DIR SCRATCH_ROOT\n' "$0" >&2
    exit 2
fi
here="$(cd -- "$(dirname -- "$0")" && pwd)"
cm="$(realpath -- "$1")"
bins="$(realpath -- "$2")"
scratch="$(realpath -- "$3")"
fixture="$here/fixtures/vendor-passive-witness"
work="$(mktemp -d "$scratch/ngspice_preprocess_passive_witness_XXXXXXXX")"
trap 'rm -rf -- "$work"' EXIT
jq -er '.cases[] | [.name,.normalized_source_sha256,(.pins|join(","))] | @tsv' "$fixture/oracle-manifest.json" > "$work/cases.tsv"
while IFS=$'\t' read -r name hash pins; do
    source="$fixture/$name.net"
    [[ $(sha256sum -- "$source" | cut -d' ' -f1) == "$hash" ]]
    runner_pins="$(printf '%s\n' "$pins" | sed -E 's/v\(([a-z0-9_]+)\)/\1/g')"
    "$here/run-compiled-vendor-physical.sh" "$source" "$runner_pins" "$cm" "$bins" "$work"
    jq -e '
        ([.connections[] | select(.direction == "input") | .analog_net] | sort == ["D","P"]) and
        ([.connections[] | select(.direction == "output") | .analog_net] | sort == ["P","Q"]) and
        all(.connections[]; .scope == [])
    ' "$work/boundaries.json" >/dev/null
    # Scalar physical call interfaces must retain their original sizes,
    # including the no-ALB forwarding and independent-load controls.
    rg -F -x -q '.subckt LOAD P' "$work/circuit.cir"
    rg -F -x -q 'Xload P LOAD' "$work/circuit.cir"
    awk -v name="$name" -v pins="$pins" -v tolerance_ns=.015 -v sample_tolerance=.045 \
        -v current_tolerance=1e-5 -f "$here/check-stateful-physical-events.awk" \
        "$fixture/vendor-events.csv" "$work/native.dat"
done < "$work/cases.tsv"
