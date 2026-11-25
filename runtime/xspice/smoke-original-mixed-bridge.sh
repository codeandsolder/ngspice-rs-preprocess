#!/usr/bin/env bash
# Original public-domain Georgia Tech mixed XSPICE ADC → digital AND → DAC.
# Source-authored differential terminals and an independent original-source
# ngspice47 oracle check true physical analog/digital event crossings.
set -euo pipefail
here="$(cd -- "$(dirname -- "$0")" && pwd)"
# shellcheck source=runtime/xspice/prepare-original-smoke.sh
source "$here/prepare-original-smoke.sh"
prepare_original_smoke "mixed_bridge" "$@"
stage_original_family_circuit "mixed-bridge"
for model in adc_bridge dac_bridge; do
    cp -- "$fixtures/georgiatech-$model-original.ifs" "$work/native/"
done
cp -- "$fixtures/georgiatech-d_and-original.ifs" "$work/native/"

# Independent original IFS interface verification before emitting discovery.
# Equal-width checks are *additional original C-model safety constraints*:
# original ADC indexes out from PORT_SIZE(in), original DAC loops over in.
for corrupt in reversed-differential wrong-analog-kind unknown-analog-kind \
               malformed-selector mismatched-adc-width mismatched-dac-width \
               swapped-original-gate-inputs wrong-original-bridge-kind \
               impossible-dac-rise-delay; do
    dir="$work/invalid-$corrupt"
    mkdir -p "$dir"
    cp -- "$work/native/original.cir" "$work/native/native-original-mixed-bridge.lib" \
          "$work/native/georgiatech-"*-original.ifs "$dir/"
    invalid_config="$config"
    case "$corrupt" in
        reversed-differential)
            sed -i 's/%vd(APLUS VCM)/%vd(VCM APLUS)/' \
                "$dir/native-original-mixed-bridge.lib" ;;
        wrong-analog-kind)
            sed -i 's/%vd(APLUS VCM)/%gd(APLUS VCM)/' \
                "$dir/native-original-mixed-bridge.lib"
            jq '.code_model_contracts[0].ports.in=["%gd(APLUS VCM)"]' \
                "$config" >"$dir/config.json"
            invalid_config="$dir/config.json" ;;
        unknown-analog-kind)
            sed -i 's/%vd(APLUS VCM)/%alien(APLUS VCM)/' \
                "$dir/native-original-mixed-bridge.lib"
            jq '.code_model_contracts[0].ports.in=["%alien(APLUS VCM)"]' \
                "$config" >"$dir/config.json"
            invalid_config="$dir/config.json" ;;
        malformed-selector)
            sed -i 's/%vd(APLUS VCM)/%vd(APLUS VCM/' \
                "$dir/native-original-mixed-bridge.lib" ;;
        mismatched-adc-width)
            sed -i 's/\[%vd(APLUS VCM)\]/[%vd(APLUS VCM) %vd(APLUS VCM)]/' \
                "$dir/native-original-mixed-bridge.lib"
            jq '.code_model_contracts[0].ports.in=["%vd(APLUS VCM)","%vd(APLUS VCM)"]' \
                "$config" >"$dir/config.json"
            invalid_config="$dir/config.json" ;;
        mismatched-dac-width)
            sed -i 's/\[ANALOG_PROBE ANALOG_OUT\] GT_DAC/[ANALOG_PROBE] GT_DAC/' \
                "$dir/native-original-mixed-bridge.lib"
            jq '.code_model_contracts[2].ports.out=["ANALOG_PROBE"]' \
                "$config" >"$dir/config.json"
            invalid_config="$dir/config.json" ;;
        swapped-original-gate-inputs)
            sed -i 's/Aand \[A_BIT D0\]/Aand [D0 A_BIT]/' \
                "$dir/native-original-mixed-bridge.lib" ;;
        wrong-original-bridge-kind)
            sed -i 's/GT_ADC adc_bridge(/GT_ADC dac_bridge(/' \
                "$dir/native-original-mixed-bridge.lib" ;;
        impossible-dac-rise-delay)
            sed -i 's/t_rise=50p/t_rise=0/' \
                "$dir/native-original-mixed-bridge.lib" ;;
    esac
    if "$bins/alb-boundary-discover" "$dir/original.cir" \
         "$invalid_config" "$dir/discovered.json" >"$dir/rejection.log" 2>&1; then
        printf 'Incorrect original mixed IFS contract escaped: %s\n' "$corrupt" >&2
        exit 1
    fi
    rg -i -q 'IFS|port|selector|model|parameter|vector|miswired' \
        "$dir/rejection.log" || {
        tail -65 "$dir/rejection.log" >&2
        exit 1
    }
    test ! -e "$dir/discovered.json"
    printf 'Original typed mixed IFS preflight rejection PASS %s\n' "$corrupt"
done

"$here/compile-original-family-circuit.sh" \
    "$bins" "$work/native" "$config" "$fixtures/auto-models.json"

rg -F -x -q \
    'Xexternal APLUS VCM D0 D0_SR_SINK D0_SR_SOURCE ANALOG_PROBE ANALOG_OUT SR_ORIGINAL_MIXED' \
    "$work/native/circuit.cir" || {
    echo 'Mixed native physical ABI mismatch (differential analog + digital triple)' >&2
    exit 1
}
[[ $(rg -F -c 'sr_family_adc(' "$work/native/circuit.cir") == 1 ]] || {
    echo 'Mixed native X expects exactly one discovered digital ALB ADC' >&2
    exit 1
}
rg -F -x -q 'Aadc [%vd(APLUS VCM)] [A_BIT] GT_ADC' \
    "$work/native/native-original-mixed-bridge.lib"
rg -F -x -q 'Aand [A_BIT D0] OUT_BIT GT_AND' \
    "$work/native/native-original-mixed-bridge.lib"
rg -F -x -q 'Adac [A_BIT OUT_BIT] [ANALOG_PROBE ANALOG_OUT] GT_DAC' \
    "$work/native/native-original-mixed-bridge.lib"

for route in stock native; do
    deck=original.cir
    [[ $route == native ]] && deck=circuit.cir
    "$here/run-original-ngspice.sh" "$work/$route" "$deck" result.dat
done
compare() {
    awk -f "$here/compare-original-mixed-bridge.awk" \
        "$work/stock/result.dat" "$1"
}
compare "$work/native/result.dat" || {
    echo 'Direct original versus native original mixed analog/digital divergence' >&2
    exit 1
}

# All corrupted variants below execute in original ngspice47 as the same
# three unmodified original C models, but must fail the independent analog
# waveform oracle (voltages, differential input or causal 2pF timing).
for corrupt in reversed-analog-input wrong-adc-threshold \
               wrong-original-logic swapped-dac-outputs \
               missing-digital-receiver-load wrong-dac-high-level; do
    dir="$work/wrong-physical-$corrupt"
    mkdir -p "$dir"
    cp -- "$work/native/circuit.cir" "$work/native/generated.inc" \
          "$work/native/native-original-mixed-bridge.lib" \
          "$work/native/.spiceinit" "$dir/"
    case "$corrupt" in
        reversed-analog-input)
            sed -i 's/%vd(APLUS VCM)/%vd(VCM APLUS)/' \
                "$dir/native-original-mixed-bridge.lib" ;;
        wrong-adc-threshold)
            sed -i 's/in_low=1.386 in_high=1.452/in_low=4.0 in_high=4.1/' \
                "$dir/native-original-mixed-bridge.lib" ;;
        wrong-original-logic)
            sed -i 's/GT_AND d_and(/GT_AND d_or(/' \
                "$dir/native-original-mixed-bridge.lib" ;;
        swapped-dac-outputs)
            sed -i 's/Adac \[A_BIT OUT_BIT\]/Adac [OUT_BIT A_BIT]/' \
                "$dir/native-original-mixed-bridge.lib" ;;
        missing-digital-receiver-load)
            sed -i 's/capacitance=2p/capacitance=0/' \
                "$dir/native-original-mixed-bridge.lib" ;;
        wrong-dac-high-level)
            sed -i 's/out_high=3.3/out_high=2.8/' \
                "$dir/native-original-mixed-bridge.lib" ;;
    esac
    "$here/run-original-ngspice.sh" "$dir" circuit.cir result.dat
    if compare "$dir/result.dat" >"$dir/rejected.log" 2>&1; then
        printf 'Original mixed ADC/AND/DAC physical corruption escaped: %s\n' "$corrupt" >&2
        exit 1
    fi
    printf 'Original mixed original-model physical mutation rejection PASS %s\n' "$corrupt"
done
echo 'AUTO PHYSICAL PASS original-mixed-differential-bridge-x (original differential voltage ADC, digital event AND, original analog DAC)'
