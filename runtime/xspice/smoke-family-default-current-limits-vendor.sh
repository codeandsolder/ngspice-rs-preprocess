#!/usr/bin/env bash
# Independent direct-digital overload and inclusive explicit override matrix.
set -euo pipefail
if (( $# != 4 )); then
    printf 'usage: %s CODEMODEL_CM RUST_BIN_DIR SCRATCH_ROOT EXTERNAL_MODEL_JSON\n' "$0" >&2
    exit 2
fi
here="$(cd -- "$(dirname -- "$0")" && pwd)"
cm="$(realpath -- "$1")"
bins="$(realpath -- "$2")"
scratch="$(realpath -- "$3")"
models="$(realpath -e -- "$4")"
fixture="$here/fixtures/vendor-default-current-limits"
work="$(mktemp -d "$scratch/ngspice_preprocess_current_limits_XXXXXXXX")"
trap 'rm -rf -- "$work"' EXIT
jq -er '.cases[] | [.name,.normalized_source_sha256,(.pins|join(",")),.voltage_time_tolerance_ns,(.quiet_pins|join(",")),(.equal_pairs|join(","))] | @tsv' "$fixture/oracle-manifest.json" > "$work/cases.tsv"
while IFS=$'\t' read -r name hash pins window quiet equal; do
    source="$fixture/$name.net"
    [[ $(sha256sum -- "$source" | cut -d' ' -f1) == "$hash" ]]
    runner_pins="$(printf '%s\n' "$pins" | sed -E 's/v\(([a-z0-9_]+)\)/\1/g')"
    "$here/run-compiled-vendor-physical.sh" "$source" "$runner_pins" "$cm" "$bins" "$work" "$models"
    jq -e --arg pins "$pins" '
        ([.connections[] | select(.direction == "input") | .analog_net] == ["D"]) and
        ([.connections[] | select(.direction == "output") | .analog_net] | sort == ($pins | split(",") | map(ltrimstr("v(")|rtrimstr(")")|ascii_upcase) | sort)) and
        all(.connections[]; .scope == [])
    ' "$work/boundaries.json" >/dev/null
    awk -v name="$name" -v pins="$pins" -v tolerance_ns=.015 -v sample_tolerance=.045 \
        -v voltage_time_tolerance_ns="$window" \
        -v quiet_pins="$quiet" -v equal_pairs="$equal" \
        -f "$here/check-stateful-physical-events.awk" "$fixture/vendor-events.csv" "$work/native.dat"
done < "$work/cases.tsv"
