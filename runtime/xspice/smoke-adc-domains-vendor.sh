#!/usr/bin/env bash
# Independent explicit ADC parameter-domain differential tests.
set -euo pipefail
if (( $# != 2 )); then
    printf 'Usage: %s MODEL_CM SCRATCH_ROOT\n' "$0" >&2
    exit 2
fi
here="$(cd -- "$(dirname -- "$0")" && pwd)"
cm="$(realpath -- "$1")"
scratch="$(realpath -- "$2")"
vendor="$here/fixtures/vendor-adc-domains"
work="$(mktemp -d "$scratch/ngspice_preprocess_adc_domains_XXXXXXXX")"
trap 'rm -rf -- "$work"' EXIT
while IFS= read -r case_json; do
    name="$(jq -er '.name' <<< "$case_json")"
    source="$vendor/$name.net"
    [[ $(sha256sum -- "$source" | cut -d' ' -f1) == "$(jq -er '.normalized_source_sha256' <<< "$case_json")" ]]
    pins="$(jq -er '.pins | join(",")' <<< "$case_json")"
    "$here/run-explicit-bridge-vendor-physical.sh" "$source" "$pins" "$cm" "$work"
    awk -v name="$name" -v pins="$pins" -v tolerance_ns=.015 \
        -v sample_only="$(jq -er '.sample_only' <<< "$case_json")" \
        -v sample_tolerance="$(jq -er '.sample_tolerance' <<< "$case_json")" \
        -v sample_relative_tolerance="$(jq -er '.sample_relative_tolerance' <<< "$case_json")" \
        -v unit="$(jq -er '.unit' <<< "$case_json")" \
        -v equal_pairs="$(jq -er '.equal_pairs|join(",")' <<< "$case_json")" \
        -v relative_pairs="$(jq -er '.relative_pairs|join(",")' <<< "$case_json")" \
        -v relative_tolerance_ns="$(jq -er '.relative_tolerance_ns' <<< "$case_json")" \
        -f "$here/check-stateful-physical-events.awk" "$vendor/vendor-events.csv" "$work/native.dat"
done < <(jq -ec '.cases[]' "$vendor/oracle-manifest.json")
