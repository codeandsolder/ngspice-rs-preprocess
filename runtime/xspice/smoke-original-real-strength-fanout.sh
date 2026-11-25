#!/usr/bin/env bash
# Original 1991 Georgia Tech d_source: STRONG ZERO/ONE, STRONG UNKNOWN, HI_Z
# UNKNOWN; the original d_to_real intentionally maps both UNKNOWN strengths
# to the *same* real midpoint. Two unmodified original real_gain C models
# independently fan out that real event, while a nonlinear physical DAC
# distinguishes the two original digital drive strengths electrically.
set -euo pipefail
here="$(cd -- "$(dirname -- "$0")" && pwd)"
# shellcheck source=runtime/xspice/prepare-original-d-source-family.sh
source "$here/prepare-original-d-source-family.sh"
prepare_original_d_source_family "real-strength-fanout" "original-real-strength-source.txt" "$@"

for poison in missing-original-source scalar-d-source-output \
              wrong-original-source-model wrong-real-gain-kind \
              missing-original-real-gain-input wrong-real-gain-parameter \
              duplicated-real-gain-parameter scalar-as-vector-gain \
              original-real-fed-as-digital-enable \
              original-digital-fed-as-real-input; do
    dir="$work/bad-ifs-$poison"
    mkdir -p "$dir"
    cp -- "$work/native/original.cir" \
          "$work/native/native-original-real-strength-fanout.lib" \
          "$work/native/original-real-strength-source.txt" \
          "$work/native/georgiatech-d_source-original.ifs" \
          "$work/native/original-xtraevt-"*.ifs "$dir/"
    invalid_config="$config"
    case "$poison" in
        missing-original-source)
            sed -i 's/Astate \[QEV\] ORIG_STATE/Awrong [QEV] ORIG_STATE/' \
                "$dir/native-original-real-strength-fanout.lib" ;;
        scalar-d-source-output)
            sed -i 's/Astate \[QEV\] ORIG_STATE/Astate QEV ORIG_STATE/' \
                "$dir/native-original-real-strength-fanout.lib" ;;
        wrong-original-source-model)
            sed -i 's/ORIG_STATE d_source(/ORIG_STATE d_genlut(/' \
                "$dir/native-original-real-strength-fanout.lib" ;;
        wrong-real-gain-kind)
            sed -i 's/ORIG_GAIN_POS real_gain(/ORIG_GAIN_POS real_to_v(/' \
                "$dir/native-original-real-strength-fanout.lib" ;;
        missing-original-real-gain-input)
            sed -i 's/AgainPos RRAW RPOS/AgainPos NULL RPOS/' \
                "$dir/native-original-real-strength-fanout.lib"
            jq '(.code_model_contracts[] | select(.instance=="AgainPos") | .ports.in)=["NULL"]' \
                "$config" >"$dir/config.json"
            invalid_config="$dir/config.json" ;;
        wrong-real-gain-parameter)
            sed -i 's/gain=1.6 out_offset/gain=1.6 unknown_gain=1 out_offset/' \
                "$dir/native-original-real-strength-fanout.lib" ;;
        duplicated-real-gain-parameter)
            sed -i 's/gain=1.6 out_offset/gain=1.6 gain=1.7 out_offset/' \
                "$dir/native-original-real-strength-fanout.lib" ;;
        scalar-as-vector-gain)
            sed -i 's/gain=1.6 out_offset/gain=[1.6 1.7] out_offset/' \
                "$dir/native-original-real-strength-fanout.lib" ;;
        original-real-fed-as-digital-enable)
            sed -i 's/Ad2gate QEV EN RGATED/Ad2gate QEV RRAW RGATED/' \
                "$dir/native-original-real-strength-fanout.lib"
            jq '(.code_model_contracts[] | select(.instance=="Ad2gate") | .ports.enable)=["RRAW"]' \
                "$config" >"$dir/config.json"
            invalid_config="$dir/config.json" ;;
        original-digital-fed-as-real-input)
            sed -i 's/AgainPos RRAW RPOS/AgainPos EN RPOS/' \
                "$dir/native-original-real-strength-fanout.lib"
            jq '(.code_model_contracts[] | select(.instance=="AgainPos") | .ports.in)=["EN"]' \
                "$config" >"$dir/config.json"
            invalid_config="$dir/config.json" ;;
    esac
    if "$bins/alb-boundary-discover" "$dir/original.cir" "$invalid_config" \
         "$dir/discovered.json" >"$dir/error.log" 2>&1; then
        printf 'Invalid original strength/real_gain IFS accepted: %s\n' "$poison" >&2
        exit 1
    fi
    rg -i -q 'IFS|port|model|parameter|binding|shared' "$dir/error.log" || {
        tail -80 "$dir/error.log" >&2
        exit 1
    }
    test ! -e "$dir/discovered.json"
    printf 'Original real-strength IFS preflight rejection PASS %s\n' "$poison"
done

"$here/compile-original-family-circuit.sh" \
    "$bins" "$work/native" "$config" "$fixtures/auto-models.json"
rg -F -x -q \
    'Xexternal LEN LEN_SR_SINK LEN_SR_SOURCE ANALOG_RAW ANALOG_GATED ANALOG_POS ANALOG_NEG ANALOG_PROBE SR_ORIGINAL_REAL_STRENGTH' \
    "$work/native/circuit.cir" || {
    echo 'Original real strength X digital enable ABI changed' >&2
    exit 1
}
[[ $(rg -F -c 'sr_family_adc(' "$work/native/circuit.cir") == 1 ]] || {
    echo 'Original real strength requires exactly one source-discovered enable ADC' >&2
    exit 1
}
for original in \
    'Astate [QEV] ORIG_STATE' \
    'Ad2raw QEV NULL RRAW ORIG_D2R' \
    'Ad2gate QEV EN RGATED ORIG_D2R' \
    'AgainPos RRAW RPOS ORIG_GAIN_POS' \
    'AgainNeg RRAW RNEG ORIG_GAIN_NEG' \
    'Araw RRAW ANALOG_RAW ORIG_R2V' \
    'Agated RGATED ANALOG_GATED ORIG_R2V' \
    'Apos RPOS ANALOG_POS ORIG_R2V' \
    'Aneg RNEG ANALOG_NEG ORIG_R2V'; do
    rg -F -x -q "$original" "$work/native/native-original-real-strength-fanout.lib"
done
rg -F -x -q '.model ORIG_STATE d_source(input_file="original-real-strength-source.txt" input_load=0)' \
    "$work/native/native-original-real-strength-fanout.lib"
for route in stock native; do
    deck=original.cir
    [[ $route == native ]] && deck=circuit.cir
    "$here/run-original-ngspice.sh" "$work/$route" "$deck" result.dat
done
compare() {
    awk -f "$here/compare-original-real-strength-fanout.awk" \
        "$work/stock/result.dat" "$1"
}
compare "$work/native/result.dat" || {
    echo 'Original strong UNKNOWN/HI_Z, enabled real_gain fanout physical mismatch' >&2
    exit 1
}

for poison in wrong-strong-unknown wrong-high-z-strength \
              wrong-zero-real-value wrong-one-real-value \
              wrong-positive-gain wrong-negative-gain wrong-gain-initial \
              bypassed-enable missing-enable-receiver \
              wrong-high-z-dac swapped-gain-analog-outputs; do
    dir="$work/bad-behavior-$poison"
    mkdir -p "$dir"
    cp -- "$work/native/circuit.cir" \
          "$work/native/generated.inc" \
          "$work/native/native-original-real-strength-fanout.lib" \
          "$work/native/original-real-strength-source.txt" \
          "$work/native/.spiceinit" "$dir/"
    case "$poison" in
        wrong-strong-unknown)
            sed -i 's/Us/1s/g' "$dir/original-real-strength-source.txt" ;;
        wrong-high-z-strength)
            sed -i 's/Uz/Us/g' "$dir/original-real-strength-source.txt" ;;
        wrong-zero-real-value)
            sed -i 's/zero=-0.5 one=2/zero=0 one=2/' \
                "$dir/native-original-real-strength-fanout.lib" ;;
        wrong-one-real-value)
            sed -i 's/zero=-0.5 one=2/zero=-0.5 one=2.5/' \
                "$dir/native-original-real-strength-fanout.lib" ;;
        wrong-positive-gain)
            sed -i 's/gain=1.6 out_offset/gain=1.3 out_offset/' \
                "$dir/native-original-real-strength-fanout.lib" ;;
        wrong-negative-gain)
            sed -i 's/gain=-1.2 out_offset/gain=-0.6 out_offset/' \
                "$dir/native-original-real-strength-fanout.lib" ;;
        wrong-gain-initial)
            sed -i 's/ic=-0.8/ic=-0.4/' \
                "$dir/native-original-real-strength-fanout.lib" ;;
        bypassed-enable)
            sed -i 's/Ad2gate QEV EN RGATED/Ad2gate QEV NULL RGATED/' \
                "$dir/native-original-real-strength-fanout.lib" ;;
        missing-enable-receiver)
            sed -i 's/capacitance=2p/capacitance=0/' \
                "$dir/native-original-real-strength-fanout.lib" ;;
        wrong-high-z-dac)
            sed -i 's/g_hiz=1e-8/g_hiz=1e-2/' \
                "$dir/native-original-real-strength-fanout.lib" ;;
        swapped-gain-analog-outputs)
            sed -i \
                -e 's/Apos RPOS ANALOG_POS/Apos RPOS ANALOG_NEG/' \
                -e 's/Aneg RNEG ANALOG_NEG/Aneg RNEG ANALOG_POS/' \
                "$dir/native-original-real-strength-fanout.lib" ;;
    esac
    "$here/run-original-ngspice.sh" "$dir" circuit.cir result.dat
    if compare "$dir/result.dat" >"$dir/rejected.log" 2>&1; then
        printf 'Original real-strength physical mutation escaped: %s\n' "$poison" >&2
        exit 1
    fi
    printf 'Original real-strength physically simulated corruption rejection PASS %s\n' "$poison"
done
echo 'AUTO PHYSICAL PASS original-real-strength-fanout-x (1991 original d_source Us/Uz + active enable + original dual real_gain fanout)'
