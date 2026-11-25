#!/usr/bin/env bash
# Independently measured original model lexical semantics (including failures).
set -euo pipefail
if (( $# != 3 )); then
    printf 'usage: %s CODEMODEL_CM RUST_BIN_DIR SCRATCH_ROOT\n' "$0" >&2
    exit 2
fi
here="$(cd -- "$(dirname -- "$0")" && pwd)"
cm="$(realpath -- "$1")"
bins="$(realpath -- "$2")"
scratch="$(realpath -- "$3")"
fixture="$here/fixtures/vendor-inline-def"
family="$here/fixtures/family"
manifest="$fixture/oracle-manifest.json"
work="$(mktemp -d "$scratch/ngspice_preprocess_inline_def_XXXXXXXX")"
trap 'rm -rf -- "$work"' EXIT
jq -s '.[0] + {bridge_models: (map(.bridge_models) | add)}' \
    "$family/auto-discovery-5v.json" "$family/auto-discovery-univ-5v.json" >"$work/catalog.json"
while IFS=$'\t' read -r name expected hash pins adc dac; do
    source="$fixture/$name.net"
    [[ $(sha256sum -- "$source" | cut -d' ' -f1) == "$hash" ]]
    if [[ $expected == false ]]; then
        rm -f -- "$work/negative.json"
        if "$bins/alb-boundary-discover" "$source" "$work/catalog.json" "$work/negative.json" >"$work/negative.log" 2>&1; then
            echo "$name: original vendor rejected but discovery accepted" >&2
            exit 1
        fi
        [[ ! -e "$work/negative.json" ]]
        printf '%s: vendor rejection reproduced\n' "$name"
        continue
    fi
    runner_pins="$(printf '%s\n' "$pins" | sed -E 's/v\(([a-z0-9_]+)\)/\1/g')"
    "$here/run-compiled-vendor-physical.sh" "$source" "$runner_pins" "$cm" "$bins" "$work"
    # Check actual automatically selected vendor family, not just output
    # voltage equality (HC and UNIV share nominal thresholds and rails).
    jq -e --arg adc "$adc" --arg dac "$dac" '
        (.connections | length == 2) and
        ([.connections[] | if .direction == "input" then .in_family + "_ADC"
            else .out_family + "_DAC" end] | sort == ([$adc,$dac] | sort))
    ' "$work/boundaries.json" >/dev/null
    awk -v name="$name" -v pins="$pins" -v tolerance_ns=.015 -v sample_tolerance=.045 \
        -v current_tolerance=1e-5 -f "$here/check-stateful-physical-events.awk" \
        "$fixture/vendor-events.csv" "$work/native.dat"
done < <(jq -r '.cases[] | [.name,.vendor_ok,.normalized_source_sha256,
    ((.pins // ["-"]) | join(",")),
    (([.bridges[]? | select(contains("_adc :")) | capture(" (?<model>[A-Z]+_adc) :").model | ascii_upcase][0]) // "-"),
    (([.bridges[]? | select(contains("_dac :")) | capture(" (?<model>[A-Z]+_dac) :").model | ascii_upcase][0]) // "-")] | @tsv' "$manifest")
