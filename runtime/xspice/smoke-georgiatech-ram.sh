#!/usr/bin/env bash
# Functional vector-bus regression for the original Georgia Tech d_ram.
# Independent direct ngspice stock code model versus ALB-compiled X wrapper.
set -euo pipefail
here="$(cd -- "$(dirname -- "$0")" && pwd)"
# shellcheck source=runtime/xspice/prepare-original-smoke.sh
source "$here/prepare-original-smoke.sh"
prepare_original_smoke "ram" "$@"
config="$fixtures/auto-discovery-georgiatech-ram-x.json"
mkdir -p "$work/stock" "$work/native"
cp -- "$fixtures/georgiatech-ram-original-reference.cir" "$work/stock/original.cir"
cp -- "$fixtures/logical-georgiatech-ram-x.cir" "$work/native/original.cir"
cp -- "$fixtures/native-georgiatech-ram.lib" "$work/native/"
cp -- "$fixtures/georgiatech-ram-original.ifs" "$work/native/"
printf 'codemodel %s\n' "$cm" >"$work/native/.spiceinit"
cp -- "$work/native/.spiceinit" "$work/stock/"
expected_q='X,1,X,0,1,X,1,X,1,X'
expected_q1='X,0,X,1,0,X,0,X,0,X'

# A correct outer .subckt signature does not establish a correct original
# internal code-model port contract. All these variants keep the outer header.
for sabotage in swap-data-vector swap-output-vector swap-address-vector wrong-original-model restricted-select-bounds missing-ifs; do
    dir="$work/bad-ifs-$sabotage"
    mkdir -p "$dir"
    cp -- "$work/native/original.cir" "$work/native/native-georgiatech-ram.lib" \
          "$work/native/georgiatech-ram-original.ifs" "$dir/"
    case "$sabotage" in
        swap-data-vector)
            sed -i 's/Aram \[D0 D1\]/Aram [D1 D0]/' "$dir/native-georgiatech-ram.lib" ;;
        swap-output-vector)
            sed -i 's/\[Q0 Q1\] \[A0 A1\]/[Q1 Q0] [A0 A1]/' "$dir/native-georgiatech-ram.lib" ;;
        swap-address-vector)
            sed -i 's/\[A0 A1\] WE/[A1 A0] WE/' "$dir/native-georgiatech-ram.lib" ;;
        wrong-original-model)
            sed -i 's/GT_ORIGINAL_RAM d_ram(/GT_ORIGINAL_RAM d_dlatch(/' "$dir/native-georgiatech-ram.lib" ;;
        restricted-select-bounds)
            sed -i 's/\[1 16\]/[1 1]/' "$dir/georgiatech-ram-original.ifs" ;;
        missing-ifs)
            rm -- "$dir/georgiatech-ram-original.ifs" ;;
    esac
    if "$bins/alb-boundary-discover" \
        "$dir/original.cir" "$config" "$dir/invalid.json" >"$dir/error.log" 2>&1; then
        echo "Incorrect original d_ram IFS/native A contract accepted: $sabotage" >&2
        exit 1
    fi
    rg -i -q 'IFS|code.model' "$dir/error.log" || { cat "$dir/error.log" >&2; exit 1; }
    test ! -e "$dir/invalid.json" || {
        echo "Failed original RAM preflight wrote output: $sabotage" >&2
        exit 1
    }
done

"$here/compile-original-family-circuit.sh" \
    "$bins" "$work/native" "$config" "$fixtures/auto-models.json"

# Seven independent digital input pins become seven physical ALB ADCs.
# Each is then passed as a complete state/sink/source triple to the native
# wrapper, which reconstructs the true original digital A-instance vectors.
expected_x='Xexternal D0 D0_SR_SINK D0_SR_SOURCE D1 D1_SR_SINK D1_SR_SOURCE A0 A0_SR_SINK A0_SR_SOURCE A1 A1_SR_SINK A1_SR_SOURCE WE WE_SR_SINK WE_SR_SOURCE S0 S0_SR_SINK S0_SR_SOURCE S1 S1_SR_SINK S1_SR_SOURCE ANALOG_Q0 ANALOG_Q1 SR_GT_RAM'
rg -F -x -q "$expected_x" "$work/native/circuit.cir" || {
    echo 'Original RAM native X digital-bundle/formal order changed' >&2
    exit 1
}
[[ $(rg -F -c 'sr_family_adc(' "$work/native/circuit.cir") == 7 ]] || {
    echo 'Original RAM missing or duplicated a physical input ADC' >&2
    exit 1
}
[[ $(rg -c '^Aload_' "$work/native/native-georgiatech-ram.lib") == 7 ]] || {
    echo 'Original RAM native wrapper missing physical receiver loads' >&2
    exit 1
}
rg -F -x -q 'Aram [D0 D1] [Q0 Q1] [A0 A1] WE [S0 S1] GT_ORIGINAL_RAM' \
    "$work/native/native-georgiatech-ram.lib"

for mode in stock native; do
    deck=original.cir
    [[ $mode == native ]] && deck=circuit.cir
    "$here/run-original-ngspice.sh" "$work/$mode" "$deck" result.dat
done
compare() {
    awk -v expected_q="$expected_q" -v expected_nq="$expected_q1" \
        -v model_name=d_ram -v label_q=Q0 -v label_nq=Q1 \
        -v min_delta_ps=150 -v max_delta_ps=250 \
        -f "$here/compare-event-traces.awk" \
        "$work/stock/result.dat" "$1"
}
compare "$work/native/result.dat" || {
    echo 'Original d_ram read/write/address/select events diverged after ALB lowering' >&2
    exit 1
}

# These poisons leave the external X ABI and IFS valid, so detection must
# come from *functional* output-state and timing comparison, not preflight.
for sabotage in wrong-select-value wrong-read-delay swapped-analog-outputs removed-receiver-loading; do
    dir="$work/bad-behavior-$sabotage"
    mkdir -p "$dir"
    cp -- "$work/native/circuit.cir" "$work/native/generated.inc" \
          "$work/native/.spiceinit" "$work/native/native-georgiatech-ram.lib" "$dir/"
    case "$sabotage" in
        wrong-select-value)
            sed -i 's/d_ram(select_value=1/d_ram(select_value=0/' "$dir/native-georgiatech-ram.lib" ;;
        wrong-read-delay)
            sed -i 's/read_delay=600p/read_delay=1500p/' "$dir/native-georgiatech-ram.lib" ;;
        swapped-analog-outputs)
            sed -i 's/\[OUT0 OUT1\] GT_ORIGINAL_DAC/[OUT1 OUT0] GT_ORIGINAL_DAC/' "$dir/native-georgiatech-ram.lib" ;;
        removed-receiver-loading)
            sed -i 's/capacitance=2p/capacitance=0/' "$dir/native-georgiatech-ram.lib" ;;
    esac
    "$here/run-original-ngspice.sh" "$dir" circuit.cir result.dat
    if compare "$dir/result.dat" >"$dir/expected-rejection.log" 2>&1; then
        echo "Corrupted original RAM behavior escaped oracle: $sabotage" >&2
        exit 1
    fi
    printf 'Original RAM expected rejection PASS %s\n' "$sabotage"
done

# The original RAM marks write and de-select outputs HI_IMPEDANCE, but its
# original uninitialized read is STRONG UNKNOWN. Stock dac_bridge conflates
# both as out_undef. Independently repeat the *same original d_ram* model
# through the already verified physical family DAC, with a 1.2 V pull-source
# and two 1-kohm resistors. HI_Z: ~1.2 V / ~0 A; strong UNKNOWN:
# ~0.4333 V / -1.533 mA total; driven LOW/HIGH have different signatures.
# These are physical ngspice node voltages and source currents.
for mode in stock native; do
    dir="$work/strength-$mode"
    mkdir -p "$dir"
    if [[ $mode == stock ]]; then
        cp -- "$work/stock/original.cir" "$dir/"
        source="$dir/original.cir"
        sed -i '/^Aout /c\Aout0 q0 q0_sink q0_source %gd(analog_q0 0) RAM_FAMILY\nAout1 q1 q1_sink q1_source %gd(analog_q1 0) RAM_FAMILY' "$source"
        sed -i '/^\.model GT_DAC /c\.model RAM_FAMILY sr_family_dac(out_low=0 out_high=3.3 g_pullup=.024 g_pulldown=.034 g_resistive=.001 g_hiz=1e-8 knee_low=2 knee_high=2.75 v_smooth=.5 input_load=0 t_rise=50p t_fall=50p)' "$source"
    else
        cp -- "$work/native/circuit.cir" "$work/native/generated.inc" \
              "$work/native/native-georgiatech-ram.lib" "$dir/"
        source="$dir/circuit.cir"
        sed -i '/^Aout /c\Aout0 Q0 Q0_SINK Q0_SOURCE %gd(OUT0 0) RAM_FAMILY\nAout1 Q1 Q1_SINK Q1_SOURCE %gd(OUT1 0) RAM_FAMILY' "$dir/native-georgiatech-ram.lib"
        sed -i '/^\.model GT_ORIGINAL_DAC /c\.model RAM_FAMILY sr_family_dac(out_low=0 out_high=3.3 g_pullup=.024 g_pulldown=.034 g_resistive=.001 g_hiz=1e-8 knee_low=2 knee_high=2.75 v_smooth=.5 input_load=0 t_rise=50p t_fall=50p)' "$dir/native-georgiatech-ram.lib"
    fi
    cp -- "$work/native/.spiceinit" "$dir/"
    sed -i 's/wrdata result.dat v(analog_q0) v(analog_q1)/wrdata result.dat i(Vpull) v(analog_q0) v(analog_q1)/' "$source"
    rg -F -q 'wrdata result.dat i(Vpull) v(analog_q0) v(analog_q1)' "$source"
    "$here/run-original-ngspice.sh" "$dir" "$(basename "$source")" result.dat
done

compare_strength() {
    awk -f "$here/compare-original-ram-strength.awk" \
        "$work/strength-stock/result.dat" "$1"
}
compare_strength "$work/strength-native/result.dat" || {
    echo 'Original RAM true HI_Z vs STRONG UNKNOWN output-strength semantics diverged' >&2
    exit 1
}
# A correctly ordered interface can still silently lose electrical drive
# semantics. Poison each independently; each negative performs a real
# ngspice physical re-simulation, not a mock parser-only test.
for sabotage in suppressed-highz weakened-pulldown wrong-pull-voltage; do
    dir="$work/strength-poison-$sabotage"
    mkdir -p "$dir"
    cp -- "$work/strength-native/circuit.cir" \
          "$work/strength-native/generated.inc" \
          "$work/strength-native/native-georgiatech-ram.lib" \
          "$work/strength-native/.spiceinit" "$dir/"
    case "$sabotage" in
        suppressed-highz)
            sed -i 's/g_hiz=1e-8/g_hiz=.01/' "$dir/native-georgiatech-ram.lib" ;;
        weakened-pulldown)
            sed -i 's/g_pulldown=.034/g_pulldown=.001/' "$dir/native-georgiatech-ram.lib" ;;
        wrong-pull-voltage)
            sed -i 's/^Vpull vpull 0 1.2$/Vpull vpull 0 0.8/' "$dir/circuit.cir" ;;
    esac
    if "$here/run-original-ngspice.sh" "$dir" circuit.cir result.dat; then
        # A corrupt physical parameter can be rejected by a waveform
        # mismatch even when the altered circuit converges.
        if compare_strength "$dir/result.dat" >"$dir/expected-rejection.log" 2>&1; then
            echo "Original RAM strength corruption escaped physical oracle: $sabotage" >&2
            exit 1
        fi
    else
        # A true circuit-level nonlinear divergence also rejects the
        # poison. Only the specific solver diagnostic counts; malformed
        # models, missing libraries and arbitrary simulator failures do not.
        if ! rg -iq 'Timestep too small' "$dir/run.log"; then
            echo "Original RAM poison failed for an unexpected reason: $sabotage" >&2
            exit 1
        fi
        printf 'Original RAM strength poison %s: nonlinear solver rejected corrupt circuit\n' "$sabotage"
    fi
    printf 'Original RAM physical strength rejection PASS %s\n' "$sabotage"
done
printf 'AUTO PHYSICAL PASS georgiatech-ram-x (20 digital states, 52 physical bit/path checks, independent HI_Z vs UNKNOWN)\n'
