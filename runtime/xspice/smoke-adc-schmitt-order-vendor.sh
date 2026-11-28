#!/usr/bin/env bash
# Independent SIMetrix 9.40d Schmitt band-hold original-source oracle.
set -euo pipefail
if (( $# != 2 )); then
    printf 'Usage: %s MODEL_CM SCRATCH_ROOT\n' "$0" >&2
    exit 2
fi
here="$(cd -- "$(dirname -- "$0")" && pwd)"
cm="$(realpath -- "$1")"
scratch="$(realpath -- "$2")"
work="$(mktemp -d "$scratch/ngspice_preprocess_schmitt_order_XXXXXXXX")"
trap 'rm -rf -- "$work"' EXIT
compare_to_vendor() {
    local case="$1" pins="$2" record="$3"
    awk -v name="$case" -v pins="$pins" \
        -v tolerance_ns="$(jq -er '.crossing_tolerance_ns' <<<"$record")" \
        -v sample_tolerance="$(jq -er '.sample_tolerance' <<<"$record")" \
        -v sample_relative_tolerance="$(jq -er '.sample_relative_tolerance // 0' <<<"$record")" \
        -v current_tolerance="$(jq -er '.current_tolerance' <<<"$record")" \
        -v voltage_time_tolerance_ns="$(jq -er '.voltage_time_tolerance_ns' <<<"$record")" \
        -v current_time_tolerance_ns="$(jq -er '.current_time_tolerance_ns' <<<"$record")" \
        -v unit=V -f "$here/check-stateful-physical-events.awk" \
        "$vendor/vendor-events.csv" "$work/native.dat"
}
for vendor in "$here/fixtures/vendor-adc-schmitt-order" \
              "$here/fixtures/vendor-adc-schmitt-clamp" \
              "$here/fixtures/vendor-adc-schmitt-ic" \
              "$here/fixtures/vendor-adc-schmitt-signed-rails"; do
while IFS= read -r record; do
    name="$(jq -er '.name' <<<"$record")"
    source="$vendor/$name.net"
    [[ $(sha256sum "$source" | cut -d' ' -f1) == "$(jq -er '.normalized_source_sha256' <<<"$record")" ]]
    pins="$(jq -er '.pins | join(",")' <<<"$record")"
    "$here/run-explicit-bridge-vendor-physical.sh" "$source" "$pins" "$cm" "$work"
    compare_to_vendor "$name" "$pins" "$record"
    case "$name" in
        baseline)
            sed 's/adc_schmitt(/adc_bridge(/' "$source" >"$work/mutated-schmitt-mode.net"
            mutated="$work/mutated-schmitt-mode.net"
            ;;
        reversed)
            sed 's/in_low=2.2 in_high=2.1/in_low=2.1 in_high=2.2/' "$source" >"$work/mutated-schmitt-thresholds.net"
            mutated="$work/mutated-schmitt-thresholds.net"
            ;;
        equal)
            sed 's/in_low=2.1 in_high=2.1/in_low=2.1 in_high=2.2/' "$source" >"$work/mutated-schmitt-equality.net"
            mutated="$work/mutated-schmitt-equality.net"
            ;;
        schmitt-ignored-wide|schmitt-ignored-reversed)
            # Unlike adc_bridge, adc_schmitt ignores clamp bounds. Changing
            # the model kind must reveal the physical clamp-current change.
            sed 's/adc_schmitt(/adc_bridge(/' "$source" >"$work/mutated-schmitt-clamp-kind.net"
            mutated="$work/mutated-schmitt-clamp-kind.net"
            ;;
        bridge-wide)
            sed -e 's/clamp_low=-100/clamp_low=0/' -e 's/clamp_high=100/clamp_high=5/' \
                "$source" >"$work/mutated-bridge-clamps.net"
            mutated="$work/mutated-bridge-clamps.net"
            ;;
        ic1)
            # IC=1 is unknown to the vendor and ignored. Non-Schmitt
            # UNKNOWN-band startup differs from the fixed Schmitt LOW.
            sed 's/adc_schmitt(/adc_bridge(/' "$source" >"$work/mutated-schmitt-ic-kind.net"
            mutated="$work/mutated-schmitt-ic-kind.net"
            ;;
        reversed-ic1)
            # In reversed overlap both Schmitt and non-Schmitt select LOW;
            # corrupt threshold ordering instead to ensure observable
            # startup-state and transition differences.
            sed 's/in_low=2.2 in_high=2.1/in_low=2.1 in_high=2.2/' \
                "$source" >"$work/mutated-schmitt-ic-threshold.net"
            mutated="$work/mutated-schmitt-ic-threshold.net"
            ;;
        schmitt-reversed)
            sed 's/out_low=5 out_high=0 /out_low=0 out_high=5 /' \
                "$source" >"$work/mutated-schmitt-rail-order.net"
            mutated="$work/mutated-schmitt-rail-order.net"
            ;;
        schmitt-negative-reversed)
            sed 's/out_low=-2 out_high=-5 /out_low=-2 out_high=5 /' \
                "$source" >"$work/mutated-schmitt-negative-rails.net"
            mutated="$work/mutated-schmitt-negative-rails.net"
            ;;
        schmitt-negative-bias)
            sed 's/clamp_bias=-2 /clamp_bias=.5 /' \
                "$source" >"$work/mutated-schmitt-negative-bias.net"
            mutated="$work/mutated-schmitt-negative-bias.net"
            ;;
        *) continue ;;
    esac
    if cmp -s "$source" "$mutated"; then
        echo "Schmitt corruption did not modify source: $name" >&2
        exit 1
    fi
    "$here/run-explicit-bridge-vendor-physical.sh" "$mutated" "$pins" "$cm" "$work"
    if compare_to_vendor "$name" "$pins" "$record" >"$work/$name-rejected.log" 2>&1; then
        echo "Schmitt corruption escaped original vendor oracle: $name" >&2
        exit 1
    fi
    rg -q "^$name (v\(q\)|i\(vd\))" "$work/$name-rejected.log"
    printf 'Schmitt corruption rejected: %s\n' "$name"
done < <(jq -ec '.cases[]' "$vendor/oracle-manifest.json")

# Both equal output load rails and zero physical clamp resistance are
# rejected by original Elements 9.40d. A failed simulation must produce
# the corresponding native diagnostic, never silently accept the model.
while IFS= read -r rejected; do
    name="$(jq -er '.name' <<<"$rejected")"
    source="$vendor/$name.net"
    [[ $(sha256sum "$source" | cut -d' ' -f1) == "$(jq -er '.normalized_source_sha256' <<<"$rejected")" ]]
    case "$name" in
        *-equal) diagnostic='needs distinct output load levels' ;;
        *-zero-res) diagnostic='needs finite timing and clamp parameters and nonzero clamp resistance' ;;
        *) echo "Unexpected vendor rejection: $name" >&2; exit 1 ;;
    esac
    if "$here/run-explicit-bridge-vendor-physical.sh" "$source" 'v(q),i(vd)' "$cm" "$work" >"$work/$name-reject.log" 2>&1; then
        echo "Vendor-rejected ADC model unexpectedly simulated: $name" >&2
        exit 1
    fi
    rg -F -q "$diagnostic" "$work/$name-reject.log"
    printf 'Vendor/native Schmitt domain rejection matched: %s\n' "$name"
done < <(jq -ec '.rejections[]?' "$vendor/oracle-manifest.json")
done
