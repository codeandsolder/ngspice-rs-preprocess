#!/usr/bin/env bash
# Independent signed current limits, receiver capacitance and physical bridges.
set -euo pipefail
if (( $# != 3 )); then
    printf 'usage: %s CODEMODEL_CM RUST_BIN_DIR SCRATCH_ROOT\n' "$0" >&2
    exit 2
fi
here="$(cd -- "$(dirname -- "$0")" && pwd)"
cm="$(realpath -- "$1")"
bins="$(realpath -- "$2")"
scratch="$(realpath -- "$3")"
fixture="$here/fixtures/vendor-signed-domains"
work="$(mktemp -d "$scratch/ngspice_preprocess_signed_domains_XXXXXXXX")"
trap 'rm -rf -- "$work"' EXIT
while IFS= read -r case_json; do
    name="$(jq -er '.name' <<< "$case_json")"
    hash="$(jq -er '.normalized_source_sha256' <<< "$case_json")"
    pins="$(jq -er '.pins | join(",")' <<< "$case_json")"
    source="$fixture/$name.net"
    [[ $(sha256sum -- "$source" | cut -d' ' -f1) == "$hash" ]]
    runner_pins="$(printf '%s\n' "$pins" | sed -E 's/v\(([a-z0-9_]+)\)/\1/g')"
    "$here/run-compiled-vendor-physical.sh" "$source" "$runner_pins" "$cm" "$bins" "$work"
    jq -e --argjson oracle "$case_json" '
        ([.connections[] | select(.direction == "input") | .analog_net] | sort) == $oracle.input_nets and
        ([.connections[] | select(.direction == "output") | .analog_net] | sort) == $oracle.output_nets and
        all(.connections[]; .scope == [])
    ' "$work/boundaries.json" >/dev/null
    awk -v name="$name" -v pins="$pins" -v tolerance_ns=.015 -v sample_tolerance=.045 \
        -v current_tolerance=.00001 \
        -v voltage_time_tolerance_ns="$(jq -er '.voltage_time_tolerance_ns' <<< "$case_json")" \
        -v quiet_pins="$(jq -er '.quiet_pins|join(",")' <<< "$case_json")" \
        -v equal_pairs="$(jq -er '.equal_pairs|join(",")' <<< "$case_json")" \
        -v relative_pairs="$(jq -er '(.relative_pairs // [])|join(",")' <<< "$case_json")" \
        -v relative_tolerance_ns="$(jq -er '.relative_tolerance_ns // 0' <<< "$case_json")" \
        -f "$here/check-stateful-physical-events.awk" "$fixture/vendor-events.csv" "$work/native.dat"
done < <(jq -ec '.cases[]' "$fixture/oracle-manifest.json")
