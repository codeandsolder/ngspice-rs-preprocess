#!/usr/bin/env bash
# Genuine SIMetrix Elements 9.40d equal and reversed DAC rail current oracle.
set -euo pipefail
if (( $# != 2 )); then
    printf 'Usage: %s MODEL_CM SCRATCH_ROOT\n' "$0" >&2
    exit 2
fi
here="$(cd -- "$(dirname -- "$0")" && pwd)"
cm="$(realpath -- "$1")"
scratch="$(realpath -- "$2")"
vendor="$here/fixtures/vendor-dac-rails"
work="$(mktemp -d "$scratch/ngspice_preprocess_dac_rails_XXXXXXXX")"
trap 'rm -rf -- "$work"' EXIT
while IFS= read -r case_json; do
    name="$(jq -er '.name' <<< "$case_json")"
    source="$vendor/$name.net"
    [[ $(sha256sum -- "$source" | cut -d' ' -f1) == "$(jq -er '.normalized_source_sha256' <<< "$case_json")" ]]
    sidecar="$(jq -er '.sidecar.name' <<< "$case_json")"
    [[ $(sha256sum -- "$vendor/$sidecar" | cut -d' ' -f1) == "$(jq -er '.sidecar.normalized_sha256' <<< "$case_json")" ]]
    cp -- "$vendor/$sidecar" "$work/"
    pins="$(jq -er '.pins | join(",")' <<< "$case_json")"
    "$here/run-explicit-bridge-vendor-physical.sh" "$source" "$pins" "$cm" "$work"
    current_time_window="$(jq -er '.current_time_tolerance_ns' <<< "$case_json")"
    awk -v name="$name" -v pins="$pins" -v sample_only=1 \
        -v sample_tolerance=1e-6 -v current_time_tolerance_ns="$current_time_window" -v unit=A \
        -f "$here/check-stateful-physical-events.awk" "$vendor/vendor-current.csv" "$work/native.dat"
    if [[ "$name" == normal ]]; then
        # A 30ps slew change is an intentional physical timing error, far
        # outside the vendor's 2ps transient sampling discretization.
        sed 's/t_rise=1n t_fall=1n/t_rise=1.03n t_fall=1.03n/' \
            "$source" >"$work/mutated-rail-slew.net"
        if cmp -s -- "$source" "$work/mutated-rail-slew.net"; then
            echo "DAC rail slew mutation did not change its source" >&2
            exit 1
        fi
        "$here/run-explicit-bridge-vendor-physical.sh" \
            "$work/mutated-rail-slew.net" "$pins" "$cm" "$work"
        if awk -v name="$name" -v pins="$pins" -v sample_only=1 \
            -v sample_tolerance=1e-6 -v current_time_tolerance_ns="$current_time_window" -v unit=A \
            -f "$here/check-stateful-physical-events.awk" \
            "$vendor/vendor-current.csv" "$work/native.dat" \
            >"$work/rejected-rail-slew.log" 2>&1; then
            echo "30ps corrupted DAC rail slew escaped the current oracle" >&2
            exit 1
        fi
        rg -F -q 'normal i(vforce) sample' "$work/rejected-rail-slew.log"
        echo "$name: 30ps corrupted DAC slew rejected"
    fi
done < <(jq -ec '.cases[]' "$vendor/oracle-manifest.json")
