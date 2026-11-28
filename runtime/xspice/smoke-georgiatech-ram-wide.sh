#!/usr/bin/env bash
# Original Georgia Tech XSPICE d_ram at 4 data bits x 8 words / 3 select bits.
# Independent direct stock code-model oracle versus fully ALB-compiled X wrapper.
set -euo pipefail
here="$(cd -- "$(dirname -- "$0")" && pwd)"
# shellcheck source=runtime/xspice/prepare-original-smoke.sh
source "$here/prepare-original-smoke.sh"
prepare_original_smoke "ram_wide" "$@"
config="$fixtures/auto-discovery-georgiatech-ram-wide-x.json"
mkdir -p "$work/stock" "$work/native"
cp -- "$fixtures/georgiatech-ram-wide-original-reference.cir" "$work/stock/original.cir"
cp -- "$fixtures/logical-georgiatech-ram-wide-x.cir" "$work/native/original.cir"
cp -- "$fixtures/native-georgiatech-ram-wide.lib" "$work/native/"
cp -- "$fixtures/georgiatech-ram-original.ifs" "$work/native/"
printf 'codemodel %s\n' "$cm" >"$work/native/.spiceinit"
cp -- "$work/native/.spiceinit" "$work/stock/"

# Strict original IFS contract checks: wrong A-instance vector permutation,
# missing independently named port, and a wrong bus width all fail before
# generating boundary output. Each malformed deck has a correct outer X ABI.
for sabotage in reversed-data-bus wrong-output-width missing-port unequal-bus-width \
                out-of-range-ram-select invalid-ram-initial-state fractional-ram-integer; do
    dir="$work/reject-ifs-$sabotage"
    mkdir -p "$dir"
    cp -- "$work/native/original.cir" "$work/native/native-georgiatech-ram-wide.lib" \
          "$work/native/georgiatech-ram-original.ifs" "$dir/"
    invalid_config="$config"
    case "$sabotage" in
        reversed-data-bus)
            sed -i 's/\[D0 D1 D2 D3\]/[D1 D0 D2 D3]/' "$dir/native-georgiatech-ram-wide.lib" ;;
        wrong-output-width)
            jq '.code_model_contracts[0].ports.data_out=["Q0","Q1","Q2"]' \
                "$config" >"$dir/invalid-config.json"
            invalid_config="$dir/invalid-config.json" ;;
        missing-port)
            jq 'del(.code_model_contracts[0].ports.address)' \
                "$config" >"$dir/invalid-config.json"
            invalid_config="$dir/invalid-config.json" ;;
        unequal-bus-width)
            # A three-bit data_out separately satisfies IFS [1 -], but
            # the original RAM's C loop indexes it using data_in width=4.
            sed -i 's/\[Q0 Q1 Q2 Q3\]/[Q0 Q1 Q2]/' "$dir/native-georgiatech-ram-wide.lib"
            jq '.code_model_contracts[0].ports.data_out=["Q0","Q1","Q2"]' \
                "$config" >"$dir/invalid-config.json"
            invalid_config="$dir/invalid-config.json" ;;
        out-of-range-ram-select)
            sed -i 's/select_value=5/select_value=32768/' \
                "$dir/native-georgiatech-ram-wide.lib" ;;
        invalid-ram-initial-state)
            sed -i 's/ic=2/ic=3/'                 "$dir/native-georgiatech-ram-wide.lib" ;;
        fractional-ram-integer)
            sed -i 's/select_value=5/select_value=1.5/' \
                "$dir/native-georgiatech-ram-wide.lib" ;;
    esac
    if "$bins/alb-boundary-discover" "$dir/original.cir" "$invalid_config" \
        "$dir/invalid-output.json" >"$dir/rejected.log" 2>&1; then
        echo "Malformed original 4-bit RAM IFS binding accepted: $sabotage" >&2
        exit 1
    fi
    rg -i -q 'IFS|code.model' "$dir/rejected.log" || {
        tail -35 "$dir/rejected.log" >&2
        exit 1
    }
    test ! -e "$dir/invalid-output.json"
done
"$here/compile-original-family-circuit.sh" \
    "$bins" "$work/native" "$config" "$fixtures/auto-models.json"

expected_x='Xexternal D0 D0_SR_SINK D0_SR_SOURCE D1 D1_SR_SINK D1_SR_SOURCE D2 D2_SR_SINK D2_SR_SOURCE D3 D3_SR_SINK D3_SR_SOURCE A0 A0_SR_SINK A0_SR_SOURCE A1 A1_SR_SINK A1_SR_SOURCE A2 A2_SR_SINK A2_SR_SOURCE WE WE_SR_SINK WE_SR_SOURCE S0 S0_SR_SINK S0_SR_SOURCE S1 S1_SR_SINK S1_SR_SOURCE S2 S2_SR_SINK S2_SR_SOURCE ANALOG_Q0 ANALOG_Q1 ANALOG_Q2 ANALOG_Q3 SR_GT_RAM_WIDE'
rg -F -x -q "$expected_x" "$work/native/circuit.cir" || {
    echo 'Original wide RAM native X full physical triple order changed' >&2
    exit 1
}
[[ $(rg -F -c 'sr_family_adc(' "$work/native/circuit.cir") == 11 ]] || {
    echo 'Original wide RAM must instantiate eleven physical input ADCs' >&2
    exit 1
}
[[ $(rg -c '^Aload_' "$work/native/native-georgiatech-ram-wide.lib") == 11 ]] || {
    echo 'Original wide RAM must have eleven independent physical receiver loads' >&2
    exit 1
}
for mode in stock native; do
    deck=original.cir
    [[ $mode == native ]] && deck=circuit.cir
    "$here/run-original-ngspice.sh" "$work/$mode" "$deck" result.dat
done
compare() {
    awk -f "$here/compare-original-ram-wide.awk" \
        "$work/stock/result.dat" "$1"
}
compare "$work/native/result.dat" || {
    echo 'Original 4-bit RAM physical state/strength/timing diverged' >&2
    exit 1
}
# These keep the original IFS and ordered native contract valid. The
# independent original-event waveform oracle must catch real behavioral
# corruption, including silently missing capacitive receiver loading.
for sabotage in incorrect-select-code reversed-analog-bits missing-input-capacitance incorrect-read-delay; do
    dir="$work/reject-behavior-$sabotage"
    mkdir -p "$dir"
    cp -- "$work/native/circuit.cir" "$work/native/generated.inc" \
          "$work/native/native-georgiatech-ram-wide.lib" "$work/native/.spiceinit" "$dir/"
    case "$sabotage" in
        incorrect-select-code)
            sed -i 's/select_value=5/select_value=4/' "$dir/native-georgiatech-ram-wide.lib" ;;
        reversed-analog-bits)
            sed -i 's/%gd(OUT0 0) RAM_FAMILY/%gd(TMP_OUT 0) RAM_FAMILY/;s/%gd(OUT1 0) RAM_FAMILY/%gd(OUT0 0) RAM_FAMILY/;s/%gd(TMP_OUT 0) RAM_FAMILY/%gd(OUT1 0) RAM_FAMILY/' "$dir/native-georgiatech-ram-wide.lib" ;;
        missing-input-capacitance)
            sed -i 's/capacitance=2p/capacitance=0/' "$dir/native-georgiatech-ram-wide.lib" ;;
        incorrect-read-delay)
            sed -i 's/read_delay=600p/read_delay=1500p/' "$dir/native-georgiatech-ram-wide.lib" ;;
    esac
    "$here/run-original-ngspice.sh" "$dir" circuit.cir result.dat
    if compare "$dir/result.dat" >"$dir/rejected.log" 2>&1; then
        echo "Original 4-bit RAM behavioral corruption escaped: $sabotage" >&2
        exit 1
    fi
    printf 'Original wide RAM expected rejection PASS %s\n' "$sabotage"
done
echo 'AUTO PHYSICAL PASS georgiatech-ram-wide-x (4-bit x 8-word original d_ram, 3-bit select, eleven input ADCs)'
