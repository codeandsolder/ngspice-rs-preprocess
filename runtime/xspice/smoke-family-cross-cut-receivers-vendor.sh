#!/usr/bin/env bash
# Physical analog net/domain grouping survives independently observed lexical cuts.
set -euo pipefail
if (( $# != 3 )); then
    printf 'usage: %s CODEMODEL_CM RUST_BIN_DIR SCRATCH_ROOT\n' "$0" >&2
    exit 2
fi
here="$(cd -- "$(dirname -- "$0")" && pwd)"
cm="$(realpath -- "$1")"
bins="$(realpath -- "$2")"
scratch="$(realpath -- "$3")"
fixture="$here/fixtures/vendor-cross-cut-receivers"
work="$(mktemp -d "$scratch/ngspice_preprocess_cross_cut_XXXXXXXX")"
trap 'rm -rf -- "$work"' EXIT
jq -er '.cases[] | [.name,.normalized_source_sha256,(.pins|join(",")),.adc_count,.dac_count] | @tsv' "$fixture/oracle-manifest.json" > "$work/cases.tsv"
while IFS=$'\t' read -r name hash pins adc dac; do
    source="$fixture/$name.net"
    [[ $(sha256sum -- "$source" | cut -d' ' -f1) == "$hash" ]]
    runner_pins="$(printf '%s\n' "$pins" | sed -E 's/v\(([a-z0-9_]+)\)/\1/g')"
    "$here/run-compiled-vendor-physical.sh" "$source" "$runner_pins" "$cm" "$bins" "$work"
    # All expected ADC owners in these root-driven circuits are root nets,
    # not one copy per INNER instance.
    jq -e --argjson adc "$adc" --argjson dac "$dac" '
        ([.connections[] | select(.direction == "input")] | length == $adc and all((.scope // []) == [])) and
        ([.connections[] | select(.direction == "output")] | length == $dac)
    ' "$work/boundaries.json" >/dev/null
    awk -v name="$name" -v pins="$pins" -v tolerance_ns=.015 -v sample_tolerance=.045 \
        -v current_tolerance=1e-5 -f "$here/check-stateful-physical-events.awk" \
        "$fixture/vendor-events.csv" "$work/native.dat"
done < "$work/cases.tsv"
