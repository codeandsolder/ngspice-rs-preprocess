#!/usr/bin/env bash
# The complete original Georgia Tech d_source digital event alphabet:
# ZERO/ONE/UNKNOWN × STRONG/RESISTIVE/HI_IMPEDANCE/UNDETERMINED.
# An actual original d_to_real->real_gain->real_gain source-driven event
# cascade is physically observed at six loaded analog terminals, while
# ngspice's own eprint verifies exact original digital state AND strength
# and 350/250/300/430ps serial real-valued event timings.
set -euo pipefail
here="$(cd -- "$(dirname -- "$0")" && pwd)"
# shellcheck source=runtime/xspice/prepare-original-d-source-family.sh
source "$here/prepare-original-d-source-family.sh"
prepare_original_d_source_family "full-strength-cascade" "original-full-strength-source.txt" "$@"

# Type/shape and original parameter contracts are independent of the
# name-keyed JSON witness; poisoning both original source and its JSON
# must not permit an invalid original C model to be compiled.
for poison in missing-original-state original-source-scalar \
              wrong-original-kind wrong-chain-kind \
              wrong-chain-input illegal-analog-event-selector \
              null-chain-input duplicate-chain-gain \
              vector-chain-gain unknown-chain-parameter \
              shared-real-as-digital shared-digital-as-real; do
    dir="$work/invalid-$poison"
    mkdir -p "$dir"
    cp -- "$work/native/original.cir" \
          "$work/native/native-original-full-strength-cascade.lib" \
          "$work/native/original-full-strength-source.txt" \
          "$work/native/georgiatech-d_source-original.ifs" \
          "$work/native/original-xtraevt-"*.ifs "$dir/"
    invalid_config="$config"
    case "$poison" in
        missing-original-state)
            sed -i 's/Astate \[QEV\] ORIG_STATE/Adeleted [QEV] ORIG_STATE/' \
                "$dir/native-original-full-strength-cascade.lib" ;;
        original-source-scalar)
            sed -i 's/Astate \[QEV\] ORIG_STATE/Astate QEV ORIG_STATE/' \
                "$dir/native-original-full-strength-cascade.lib" ;;
        wrong-original-kind)
            sed -i 's/ORIG_STATE d_source(/ORIG_STATE d_ram(/' \
                "$dir/native-original-full-strength-cascade.lib" ;;
        wrong-chain-kind)
            sed -i 's/ORIG_GAIN_CHAIN real_gain(/ORIG_GAIN_CHAIN real_to_v(/' \
                "$dir/native-original-full-strength-cascade.lib" ;;
        wrong-chain-input)
            sed -i 's/AgainChain RPOS RCHAIN/AgainChain RNEG RCHAIN/' \
                "$dir/native-original-full-strength-cascade.lib" ;;
        illegal-analog-event-selector)
            sed -i 's/AgainChain RPOS RCHAIN/AgainChain %v(RPOS) RCHAIN/' \
                "$dir/native-original-full-strength-cascade.lib"
            jq '(.code_model_contracts[] | select(.instance=="AgainChain") | .ports.in)=["%v(RPOS)"]' \
                "$config" >"$dir/config.json"
            invalid_config="$dir/config.json" ;;
        null-chain-input)
            sed -i 's/AgainChain RPOS RCHAIN/AgainChain NULL RCHAIN/' \
                "$dir/native-original-full-strength-cascade.lib"
            jq '(.code_model_contracts[] | select(.instance=="AgainChain") | .ports.in)=["NULL"]' \
                "$config" >"$dir/config.json"
            invalid_config="$dir/config.json" ;;
        duplicate-chain-gain)
            sed -i 's/gain=-0.75 out_offset/gain=-0.75 gain=-0.6 out_offset/' \
                "$dir/native-original-full-strength-cascade.lib" ;;
        vector-chain-gain)
            sed -i 's/gain=-0.75 out_offset/gain=[-0.75 -0.6] out_offset/' \
                "$dir/native-original-full-strength-cascade.lib" ;;
        unknown-chain-parameter)
            sed -i 's/gain=-0.75 out_offset/gain=-0.75 mystery_gain=1 out_offset/' \
                "$dir/native-original-full-strength-cascade.lib" ;;
        shared-real-as-digital)
            sed -i 's/Ad2gate QEV EN RGATED/Ad2gate QEV RPOS RGATED/' \
                "$dir/native-original-full-strength-cascade.lib"
            jq '(.code_model_contracts[] | select(.instance=="Ad2gate") | .ports.enable)=["RPOS"]' \
                "$config" >"$dir/config.json"
            invalid_config="$dir/config.json" ;;
        shared-digital-as-real)
            sed -i 's/AgainChain RPOS RCHAIN/AgainChain EN RCHAIN/' \
                "$dir/native-original-full-strength-cascade.lib"
            jq '(.code_model_contracts[] | select(.instance=="AgainChain") | .ports.in)=["EN"]' \
                "$config" >"$dir/config.json"
            invalid_config="$dir/config.json" ;;
    esac
    if "$bins/alb-boundary-discover" "$dir/original.cir" "$invalid_config" \
         "$dir/discovered.json" >"$dir/rejected.log" 2>&1; then
        printf 'Invalid original complete digital-strength IFS accepted: %s\n' "$poison" >&2
        exit 1
    fi
    rg -i -q 'IFS|port|model|parameter|binding|shared' "$dir/rejected.log" || {
        tail -75 "$dir/rejected.log" >&2
        exit 1
    }
    test ! -e "$dir/discovered.json"
    printf 'Original complete-strength IFS preflight rejection PASS %s\n' "$poison"
done

"$here/compile-original-family-circuit.sh" \
    "$bins" "$work/native" "$config" "$fixtures/auto-models.json"
rg -F -x -q \
    'Xexternal LEN LEN_SR_SINK LEN_SR_SOURCE ANALOG_RAW ANALOG_GATED ANALOG_POS ANALOG_NEG ANALOG_CHAIN ANALOG_PROBE SR_ORIGINAL_FULL_STRENGTH' \
    "$work/native/circuit.cir" || {
    echo 'Original full strength physical subckt ABI mismatch' >&2
    exit 1
}
[[ $(rg -F -c 'sr_family_adc(' "$work/native/circuit.cir") == 1 ]] || {
    echo 'Expected one independently synthesized physical ALB enabled digital event' >&2
    exit 1
}
for original in \
    'Astate [QEV] ORIG_STATE' \
    'Ad2raw QEV NULL RRAW ORIG_D2R' \
    'Ad2gate QEV EN RGATED ORIG_D2R' \
    'AgainPos RRAW RPOS ORIG_GAIN_POS' \
    'AgainNeg RRAW RNEG ORIG_GAIN_NEG' \
    'AgainChain RPOS RCHAIN ORIG_GAIN_CHAIN' \
    'Araw RRAW ANALOG_RAW ORIG_R2V' \
    'Agated RGATED ANALOG_GATED ORIG_R2V' \
    'Apos RPOS ANALOG_POS ORIG_R2V' \
    'Aneg RNEG ANALOG_NEG ORIG_R2V' \
    'Achain RCHAIN ANALOG_CHAIN ORIG_R2V'; do
    rg -F -x -q "$original" "$work/native/native-original-full-strength-cascade.lib"
done
rg -F -x -q '.model ORIG_STATE d_source(input_file="original-full-strength-source.txt" input_load=0)' \
    "$work/native/native-original-full-strength-cascade.lib"
for route in stock native; do
    deck=original.cir
    [[ $route == native ]] && deck=circuit.cir
    "$here/run-original-ngspice.sh" "$work/$route" "$deck" result.dat original-events.txt
done
compare_physical() {
    awk -f "$here/family-dc-equilibrium.awk" -f "$here/compare-original-full-strength-cascade.awk" \
        "$work/stock/result.dat" "$1"
}
compare_events() {
    awk -f "$here/verify-original-full-strength-events.awk" \
        "$work/stock/original-events.txt" "$1"
}
compare_physical "$work/native/result.dat"
compare_events "$work/native/original-events.txt"

# This set includes poisonings of source digital strength which DO NOT
# change the real-valued or analog output of d_to_real: eprint must still
# reject wrong strength, without pretending the analog adapter resolves
# all 12 strengths into distinct terminal voltages.
for poison in changed-undetermined-zero changed-undetermined-unknown \
              changed-resistive-unknown changed-hi-z-zero changed-hi-z-unknown \
              wrong-real-one wrong-positive-gain wrong-negative-gain \
              wrong-serial-gain wrong-serial-offset wrong-serial-delay \
              wrong-serial-initial-condition wrong-gain-fanout \
              bypassed-enable missing-enable-receiver swapped-analog-outputs; do
    dir="$work/bad-physical-$poison"
    mkdir -p "$dir"
    cp -- "$work/native/circuit.cir" \
          "$work/native/generated.inc" \
          "$work/native/native-original-full-strength-cascade.lib" \
          "$work/native/original-full-strength-source.txt" \
          "$work/native/.spiceinit" "$dir/"
    case "$poison" in
        changed-undetermined-zero)
            sed -i 's/0u/0s/g' "$dir/original-full-strength-source.txt" ;;
        changed-undetermined-unknown)
            sed -i 's/Uu/Us/g' "$dir/original-full-strength-source.txt" ;;
        changed-resistive-unknown)
            sed -i 's/Ur/Us/g' "$dir/original-full-strength-source.txt" ;;
        changed-hi-z-zero)
            sed -i 's/0z/0s/g' "$dir/original-full-strength-source.txt" ;;
        changed-hi-z-unknown)
            sed -i 's/Uz/Us/g' "$dir/original-full-strength-source.txt" ;;
        wrong-real-one)
            sed -i 's/zero=-0.5 one=2/zero=-0.5 one=2.5/' \
                "$dir/native-original-full-strength-cascade.lib" ;;
        wrong-positive-gain)
            sed -i 's/gain=1.6 out_offset/gain=1.3 out_offset/' \
                "$dir/native-original-full-strength-cascade.lib" ;;
        wrong-negative-gain)
            sed -i 's/gain=-1.2 out_offset/gain=-.9 out_offset/' \
                "$dir/native-original-full-strength-cascade.lib" ;;
        wrong-serial-gain)
            sed -i 's/gain=-0.75 out_offset/gain=-.6 out_offset/' \
                "$dir/native-original-full-strength-cascade.lib" ;;
        wrong-serial-offset)
            sed -i 's/in_offset=-0.1 gain=/in_offset=0 gain=/' \
                "$dir/native-original-full-strength-cascade.lib" ;;
        wrong-serial-delay)
            sed -i 's/delay=430p ic=-2.3/delay=700p ic=-2.3/' \
                "$dir/native-original-full-strength-cascade.lib" ;;
        wrong-serial-initial-condition)
            sed -i 's/ic=-2.3/ic=-1.3/' \
                "$dir/native-original-full-strength-cascade.lib" ;;
        wrong-gain-fanout)
            sed -i 's/AgainChain RPOS RCHAIN/AgainChain RNEG RCHAIN/' \
                "$dir/native-original-full-strength-cascade.lib" ;;
        bypassed-enable)
            sed -i 's/Ad2gate QEV EN RGATED/Ad2gate QEV NULL RGATED/' \
                "$dir/native-original-full-strength-cascade.lib" ;;
        missing-enable-receiver)
            sed -i 's/capacitance=2p/capacitance=0/' \
                "$dir/native-original-full-strength-cascade.lib" ;;
        swapped-analog-outputs)
            sed -i -e 's/Apos RPOS ANALOG_POS/Apos RPOS ANALOG_CHAIN/' \
                -e 's/Achain RCHAIN ANALOG_CHAIN/Achain RCHAIN ANALOG_POS/' \
                "$dir/native-original-full-strength-cascade.lib" ;;
    esac
    "$here/run-original-ngspice.sh" "$dir" circuit.cir result.dat original-events.txt
    if compare_physical "$dir/result.dat" >"$dir/rejected-physical.log" 2>&1 &&
        compare_events "$dir/original-events.txt" >"$dir/rejected-events.log" 2>&1; then
        printf 'Original 12-code physical/event mutation escaped: %s\n' "$poison" >&2
        exit 1
    fi
    printf 'Original complete-strength physically simulated mutation rejection PASS %s\n' "$poison"
done
echo 'AUTO PHYSICAL PASS original-full-strength-cascade-x (exact all 12 digital strengths, eprint serial real event arithmetic and physical six-output oracle)'
