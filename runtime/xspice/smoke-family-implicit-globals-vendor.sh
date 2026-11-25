#!/usr/bin/env bash
# Original ground and implicit global wiring, including mixed physical domains.
set -euo pipefail
if (( $# != 3 )); then
    printf 'usage: %s CODEMODEL_CM RUST_BIN_DIR SCRATCH_ROOT\n' "$0" >&2
    exit 2
fi
here="$(cd -- "$(dirname -- "$0")" && pwd)"
cm="$(realpath -- "$1")"
bins="$(realpath -- "$2")"
scratch="$(realpath -- "$3")"
fixture="$here/fixtures/vendor-implicit-globals"
work="$(mktemp -d "$scratch/ngspice_preprocess_implicit_globals_XXXXXXXX")"
trap 'rm -rf -- "$work"' EXIT
jq -er '.cases[] | [.name,.normalized_source_sha256,.vendor_ok,((.pins // [])|join(",")|if .=="" then "-" else . end),(.adc_count // 0),(.dac_count // 0)] | @tsv' "$fixture/oracle-manifest.json" > "$work/cases.tsv"
while IFS=$'\t' read -r name hash ok pins adc dac; do
    source="$fixture/$name.net"
    [[ $(sha256sum -- "$source" | cut -d' ' -f1) == "$hash" ]]
    if [[ "$ok" == false ]]; then
        rm -f -- "$work/rejected.json"
        if "$bins/alb-boundary-discover" "$source" "$here/fixtures/family/auto-discovery-5v.json" "$work/rejected.json" >"$work/rejected.log" 2>&1; then
            printf '%s: incorrectly accepted\n' "$name" >&2
            exit 1
        fi
        rg -q 'global node used as argument' "$work/rejected.log"
        test ! -e "$work/rejected.json"
        printf '%s: rejected as vendor does PASS\n' "$name"
        continue
    fi
    runner_pins="$(printf '%s\n' "$pins" | sed -E 's/v\(([a-z0-9_]+)\)/\1/g')"
    "$here/run-compiled-vendor-physical.sh" "$source" "$runner_pins" "$cm" "$bins" "$work"
    jq -e --argjson adc "$adc" --argjson dac "$dac" '
        ([.connections[] | select(.direction=="input")] | length)==$adc and
        ([.connections[] | select(.direction=="output")] | length)==$dac and
        all(.connections[] | select(.direction=="input"); .scope==[])
    ' "$work/boundaries.json" >/dev/null
    window=0
    # These independently measured coarse runs show timestep-quantized
    # ADC switching. The 0.5 ps run is still checked at the exact sample
    # timestamps. Keep the same 45 mV vertical and 15 ps crossing limits.
    case "$name" in
        global-child-physical|global-child-physical-p-observed) window=.015 ;;
    esac
    awk -v name="$name" -v pins="$pins" -v tolerance_ns=.015 -v sample_tolerance=.045 \
        -v voltage_time_tolerance_ns="$window" -v current_tolerance=1e-5 \
        -f "$here/check-stateful-physical-events.awk" "$fixture/vendor-events.csv" "$work/native.dat"
done < "$work/cases.tsv"
