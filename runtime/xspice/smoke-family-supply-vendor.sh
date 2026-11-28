#!/usr/bin/env bash
# Original mixed-supply ALBs: no hand-assigned pin groups or voltage scales.
set -euo pipefail
if (( $# != 4 )); then
    printf 'Usage: %s MODEL_CM RUST_BIN_DIR SCRATCH_ROOT EXTERNAL_MODEL_JSON\n' "$0" >&2
    exit 2
fi
here="$(cd -- "$(dirname -- "$0")" && pwd)"
model="$(realpath -- "$1")"
bins="$(realpath -- "$2")"
scratch="$(realpath -- "$3")"
models="$(realpath -e -- "$4")"
work="$(mktemp -d "$scratch/ngspice_preprocess_supply_XXXXXXXX")"
trap 'rm -rf -- "$work"' EXIT
for name in implicit-supply-matrix implicit-supply-loaded implicit-supply-fine implicit-supply-matrix-univ implicit-supply-loaded-univ implicit-supply-fine-univ shared-receivers; do
    reference_name="${name%-univ}"
    vendor="$here/fixtures/vendor-supply-scaling"
    current_tolerance=1e-5
    voltage_tolerance=1e-4
    dac_count=12
    equal=""
    all_voltage_samples=0
    case "$reference_name" in
        implicit-supply-matrix) ref="$vendor/vendor-sparse.csv"; tolerance_ns=.1 ;;
        implicit-supply-loaded) ref="$vendor/vendor-loaded-sparse.csv"; tolerance_ns=.1 ;;
        implicit-supply-fine) ref="$vendor/vendor-fine-sparse.csv"; tolerance_ns=.015 ;;
        shared-receivers)
            vendor="$here/fixtures/vendor-receiver-groups"
            ref="$vendor/vendor-sparse.csv"
            tolerance_ns=.015
            current_tolerance=2e-7
            voltage_tolerance=.03
            all_voltage_samples=1
            dac_count=7
            equal="v(q1):v(q4),v(q2):v(q5),v(q2):v(q7),v(q3):v(q6)"
            ;;
    esac
    if [[ "$name" != "$reference_name" ]]; then
        jq -e --arg name "$name" --arg reference "$reference_name" \
            --arg actual "$(sha256sum "$vendor/$name.net" | cut -d' ' -f1)" \
            --arg hash "$(jq -er --arg reference "$reference_name" '.[] | select(.case == $reference) | .waveform_sha256' "$vendor/oracle-manifest.json")" \
            '.[] | select(.name == $name and .reference == $reference) |
            .normalized_source_sha256 == $actual and .csv_sha256 == $hash and .hc_csv_sha256 == $hash' \
            "$vendor/univ-manifest.json" >/dev/null
    fi
    pins="$(head -1 "$ref" | cut -d, -f2-)"
    runner_pins="$(printf '%s\n' "$pins" | sed -E 's/v\(([a-z0-9_]+)\)/\1/g')"
    "$here/run-compiled-vendor-physical.sh" "$vendor/$name.net" "$runner_pins" "$model" "$bins" "$work" "$models"
    [[ $(rg -c '^\.model .*sr_family_adc\(' "$work/circuit.cir") == 3 ]]
    [[ $(rg -c '^\.model .*sr_family_dac\(' "$work/circuit.cir") == "$dac_count" ]]
    # Check all retained current values and dynamic voltage crossings.
    # Do not time-align the traces.
    awk -F, -v name="$reference_name" '
        NR==1 {for(i=2;i<=NF;i++)pin[i]=$i; next}
        {for(i=2;i<=NF;i++) if(pin[i]~/^i\(/) print name,pin[i],NR==2?"initial":"sample",$1*1e9,$i}
    ' "$ref" >"$work/current.events"
    current_pins="$(printf '%s\n' "$pins" | tr ',' '\n' | rg '^i\(' | paste -sd,)"
    volt_pins="$(printf '%s\n' "$pins" | tr ',' '\n' | rg '^v\(' | paste -sd,)"
    # Extract selected wrdata columns while retaining their time stamps.
    awk -v pins="$pins" '
        BEGIN {n=split(pins,p,",")}
        {for(i=1;i<=n;i++)if(p[i]~/^i\(/)printf "%s %s ",$(2*i-1),$(2*i);print ""}
    ' "$work/native.dat" >"$work/current.dat"
    awk -v pins="$pins" '
        BEGIN {n=split(pins,p,",")}
        {for(i=1;i<=n;i++)if(p[i]~/^v\(/)printf "%s %s ",$(2*i-1),$(2*i);print ""}
    ' "$work/native.dat" >"$work/voltage.dat"
    awk -v name="$reference_name" -v pins="$current_pins" -v sample_only=1 -v sample_tolerance="$current_tolerance" -v unit=A \
        -f "$here/check-stateful-physical-events.awk" "$work/current.events" "$work/current.dat"
    awk -F, -v name="$reference_name" -v all_samples="$all_voltage_samples" '
        NR==1 {for(i=2;i<=NF;i++)pin[i]=$i;next}
        {for(i=2;i<=NF;i++)if(pin[i]~/^v\(/){
            if(NR==2)print name,pin[i],"initial",0,$i
            else if(all_samples)print name,pin[i],"sample",$1*1e9,$i
            last[i]=$i
        };t=$1}
        END {if(!all_samples)for(i=2;i<=length(pin)+1;i++)if(pin[i]~/^v\(/)print name,pin[i],"sample",t*1e9,last[i]}
    ' "$ref" >"$work/voltage.events"
    printf "Checking %s against original %s waveform\n" "$name" "$reference_name"
    cat "$vendor/vendor-crossings.csv" >>"$work/voltage.events"
    awk -v name="$reference_name" -v pins="$volt_pins" -v sample_tolerance="$voltage_tolerance" -v tolerance_ns="$tolerance_ns" -v equal_pairs="$equal" \
        -f "$here/check-stateful-physical-events.awk" "$work/voltage.events" "$work/voltage.dat"
done
echo 'All original HC and UNIV mixed-supply voltage/current matrices passed.'
