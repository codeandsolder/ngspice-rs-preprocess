#!/usr/bin/env bash
# Direct SIMetrix Elements 9.40d ADC threshold and clamp ordering oracle.
set -euo pipefail
if (( $# != 2 )); then
    printf 'Usage: %s MODEL_CM SCRATCH_ROOT\n' "$0" >&2
    exit 2
fi
here="$(cd -- "$(dirname -- "$0")" && pwd)"
cm="$(realpath -- "$1")"
scratch="$(realpath -- "$2")"
work="$(mktemp -d "$scratch/ngspice_preprocess_adc_order_XXXXXXXX")"
trap 'rm -rf -- "$work"' EXIT
compare_to_vendor() {
    local name="$1" pins="$2" reference="$3"
    awk -v name="$name" -v pins="$pins" \
        -v tolerance_ns="$(jq -er '.crossing_tolerance_ns' <<< "$reference")" \
        -v sample_tolerance="$(jq -er '.sample_tolerance' <<< "$reference")" \
        -v current_tolerance="$(jq -er '.current_tolerance' <<< "$reference")" \
        -v current_time_tolerance_ns="$(jq -er '.current_time_tolerance_ns' <<< "$reference")" \
        -v voltage_time_tolerance_ns="$(jq -er '.voltage_time_tolerance_ns' <<< "$reference")" \
        -v unit=V -f "$here/check-stateful-physical-events.awk" \
        "$vendor/vendor-events.csv" "$work/native.dat"
}
# Compare both direct threshold/clamp and signed reflected-load matrices
# using the same original-source physical oracle and corruption checks.
for vendor in "$here/fixtures/vendor-adc-order" \
              "$here/fixtures/vendor-adc-reflected-order"; do
    while IFS= read -r case_json; do
        name="$(jq -er '.name' <<< "$case_json")"
        source="$vendor/$name.net"
        [[ $(sha256sum -- "$source" | cut -d' ' -f1) == "$(jq -er '.normalized_source_sha256' <<< "$case_json")" ]]
        pins="$(jq -er '.pins | join(",")' <<< "$case_json")"
        "$here/run-explicit-bridge-vendor-physical.sh" "$source" "$pins" "$cm" "$work"
        compare_to_vendor "$name" "$pins" "$case_json"
        case "$name" in
            in-reversed)
                # Restoring conventional input thresholds must alter the
                # independently measured falling-edge timing by ~20 ps.
                sed 's/in_low=2.2 in_high=2.1/in_low=2.1 in_high=2.2/' \
                    "$source" >"$work/mutated-input-thresholds.net"
                mutated="$work/mutated-input-thresholds.net"
                expected_failure='in-reversed v(q)'
                ;;
            clamp-reversed)
                # The reversed clamp sources hundreds of mA. Replacing it
                # with an ordinary ordered clamp cannot satisfy the oracle.
                sed 's/clamp_low=5 clamp_high=0/clamp_low=0 clamp_high=5/' \
                    "$source" >"$work/mutated-clamp-order.net"
                mutated="$work/mutated-clamp-order.net"
                expected_failure='clamp-reversed i(vd) sample'
                ;;
            reversed)
                # Signed reflected input load changes slope and sign
                # when the ADC output load reference rails are swapped.
                sed 's/out_low=5 out_high=0 /out_low=0 out_high=5 /' \
                    "$source" >"$work/mutated-reflected-rails.net"
                mutated="$work/mutated-reflected-rails.net"
                expected_failure='reversed i(vd) initial'
                ;;
            negative-to-positive)
                # A distinct receiver demand must remain visible on the
                # physical input source, independently of digital logic.
                sed 's/sink_current=-10m source_current=20m/sink_current=0m source_current=20m/' \
                    "$source" >"$work/mutated-reflected-load.net"
                mutated="$work/mutated-reflected-load.net"
                expected_failure='negative-to-positive i(vd) initial'
                ;;
            *) continue ;;
        esac
        if cmp -s -- "$source" "$mutated"; then
            printf 'ADC order mutation did not modify source: %s\n' "$name" >&2
            exit 1
        fi
        "$here/run-explicit-bridge-vendor-physical.sh" \
            "$mutated" "$pins" "$cm" "$work"
        if compare_to_vendor "$name" "$pins" "$case_json" >"$work/$name-mutation.log" 2>&1; then
            printf 'ADC ordering corruption escaped oracle: %s\n' "$name" >&2
            exit 1
        fi
        rg -F -q "$expected_failure" "$work/$name-mutation.log"
        printf 'ADC ordering corruption rejected: %s\n' "$name"
    done < <(jq -ec '.cases[]' "$vendor/oracle-manifest.json")

    # Vendor rejected the equal output-load levels through a failed operating
    # point. The native implementation must not silently accept division by zero.
    while IFS= read -r rejected; do
        source="$vendor/$(jq -er '.name' <<< "$rejected").net"
        [[ $(sha256sum -- "$source" | cut -d' ' -f1) == "$(jq -er '.normalized_source_sha256' <<< "$rejected")" ]]
        if "$here/run-explicit-bridge-vendor-physical.sh" "$source" 'v(q),i(vd)' "$cm" "$work" \
                >"$work/rejected-rail.log" 2>&1; then
            echo "Equal ADC output-load rails unexpectedly simulated successfully" >&2
            exit 1
        fi
        rg -F -q 'needs distinct output load levels' "$work/rejected-rail.log"
        echo "ADC equal output-load rails: vendor/native rejection matched"
    done < <(jq -ec '.rejections[]' "$vendor/oracle-manifest.json")

done
