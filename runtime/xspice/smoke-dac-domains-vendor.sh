#!/usr/bin/env bash
# Independent explicit DAC current parameter-domain and full-strength tests.
set -euo pipefail
if (( $# != 2 )); then
    printf 'Usage: %s MODEL_CM SCRATCH_ROOT\n' "$0" >&2
    exit 2
fi
here="$(cd -- "$(dirname -- "$0")" && pwd)"
cm="$(realpath -- "$1")"
scratch="$(realpath -- "$2")"
vendor="$here/fixtures/vendor-dac-domains"
work="$(mktemp -d "$scratch/ngspice_preprocess_dac_domains_XXXXXXXX")"
trap 'rm -rf -- "$work"' EXIT
while IFS= read -r case_json; do
    name="$(jq -er '.name' <<< "$case_json")"
    source="$vendor/$name.net"
    [[ $(sha256sum -- "$source" | cut -d' ' -f1) == "$(jq -er '.normalized_source_sha256' <<< "$case_json")" ]]
    if jq -e '.sidecar' <<< "$case_json" >/dev/null; then
        sidecar="$(jq -er '.sidecar.name' <<< "$case_json")"
        [[ $(sha256sum -- "$vendor/$sidecar" | cut -d' ' -f1) == "$(jq -er '.sidecar.normalized_sha256' <<< "$case_json")" ]]
        cp -- "$vendor/$sidecar" "$work/"
    fi
    pins="$(jq -er '.pins | join(",")' <<< "$case_json")"
    "$here/run-explicit-bridge-vendor-physical.sh" "$source" "$pins" "$cm" "$work"
    sample_tolerance="$(jq -er '.sample_tolerance // 1e-6' <<< "$case_json")"
    current_time_window="$(jq -er '.current_time_tolerance_ns // 0' <<< "$case_json")"
    awk -v name="$name" -v pins="$pins" -v sample_only=1 -v sample_tolerance="$sample_tolerance" -v current_time_tolerance_ns="$current_time_window" -v unit=A \
        -f "$here/check-stateful-physical-events.awk" "$vendor/vendor-current.csv" "$work/native.dat"
    if [[ "$name" == dac-signed-ramp-matrix ]]; then
        # The vendor ADC timestamps are rounded to half-ps precision;
        # this case allows at most 1 ps of physical event displacement.
        # Mutating both ADC delays by 4 ps must still fail that bound.
        sed -e 's/rise_delay=1n/rise_delay=1.004n/' \
            -e 's/fall_delay=1n/fall_delay=1.004n/' \
            "$source" >"$work/mutated-dac-ramp.net"
        if cmp -s -- "$source" "$work/mutated-dac-ramp.net"; then
            echo "DAC ramp timing mutation did not alter the source" >&2
            exit 1
        fi
        "$here/run-explicit-bridge-vendor-physical.sh" \
            "$work/mutated-dac-ramp.net" "$pins" "$cm" "$work"
        if awk -v name="$name" -v pins="$pins" -v sample_only=1 \
            -v sample_tolerance="$sample_tolerance" \
            -v current_time_tolerance_ns="$current_time_window" -v unit=A \
            -f "$here/check-stateful-physical-events.awk" \
            "$vendor/vendor-current.csv" "$work/native.dat" \
            >"$work/dac-ramp-mutation-rejection.log" 2>&1; then
            echo "4 ps ADC delay mutation escaped bounded ramp oracle" >&2
            exit 1
        fi
        rg -q "$name i\\(vforce[0-9]+\\) sample" \
            "$work/dac-ramp-mutation-rejection.log"
        echo "$name: 4 ps ADC timing mutation rejected"
    fi
done < <(jq -ec '.cases[]' "$vendor/oracle-manifest.json")
