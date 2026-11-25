#!/usr/bin/env bash
# Original ngspice47 XSPICE user-defined real event pipeline:
# d_to_real -> real_delay (positive clock sample) -> real_to_v.
# Independently verifies actual original model IFS and C code models through
# stock ngspice and original-source ALB compilation, including 2pF loading.
set -euo pipefail
here="$(cd -- "$(dirname -- "$0")" && pwd)"
# shellcheck source=runtime/xspice/prepare-original-smoke.sh
source "$here/prepare-original-smoke.sh"
prepare_original_smoke "real_event" "$@"
stage_original_family_circuit "real-event-chain"
for model in d_to_real real_delay real_to_v; do
    cp -- "$fixtures/original-xtraevt-$model.ifs" "$work/native/"
done

# The native verifier parses original IFS port types/NULL allowances and
# parameters, source-reachable original A instances and model declarations.
# Altered independent named witnesses must not override the actual IFS.
for corrupt in reversed-real-clock null-nonnullable-real null-digital-data \
               wrong-real-input-kind wrong-clock-type wrong-original-model-kind \
               swapped-original-raw-output forbidden-real-delay \
               forbidden-d2r-delay forbidden-real-ramp \
               undeclared-real-parameter duplicate-real-delay \
               digital-node-used-as-real real-node-used-as-clock \
               analog-node-used-as-real; do
    dir="$work/preflight-$corrupt"
    mkdir -p "$dir"
    cp -- "$work/native/original.cir" "$work/native/native-original-real-event-chain.lib" \
          "$work/native/original-xtraevt-"*.ifs "$dir/"
    invalid_config="$config"
    case "$corrupt" in
        reversed-real-clock)
            sed -i 's/Adelay RDATA CLOCK RHELD/Adelay CLOCK RDATA RHELD/' \
                "$dir/native-original-real-event-chain.lib" ;;
        null-nonnullable-real)
            sed -i 's/Adelay RDATA CLOCK RHELD/Adelay NULL CLOCK RHELD/' \
                "$dir/native-original-real-event-chain.lib"
            jq '.code_model_contracts[1].ports.in=["NULL"]' \
                "$config" >"$dir/config.json"
            invalid_config="$dir/config.json" ;;
        null-digital-data)
            sed -i 's/Ad2r DATA NULL RDATA/Ad2r NULL NULL RDATA/' \
                "$dir/native-original-real-event-chain.lib"
            jq '.code_model_contracts[0].ports.in=["NULL"]' \
                "$config" >"$dir/config.json"
            invalid_config="$dir/config.json" ;;
        wrong-real-input-kind)
            sed -i 's/Adelay RDATA CLOCK RHELD/Adelay %v(RDATA) CLOCK RHELD/' \
                "$dir/native-original-real-event-chain.lib"
            jq '.code_model_contracts[1].ports.in=["%v(RDATA)"]' \
                "$config" >"$dir/config.json"
            invalid_config="$dir/config.json" ;;
        wrong-clock-type)
            sed -i 's/Adelay RDATA CLOCK RHELD/Adelay RDATA %real(CLOCK) RHELD/' \
                "$dir/native-original-real-event-chain.lib"
            jq '.code_model_contracts[1].ports.clk=["%real(CLOCK)"]' \
                "$config" >"$dir/config.json"
            invalid_config="$dir/config.json" ;;
        wrong-original-model-kind)
            sed -i 's/ORIGINAL_D2R d_to_real(/ORIGINAL_D2R real_gain(/' \
                "$dir/native-original-real-event-chain.lib" ;;
        swapped-original-raw-output)
            sed -i 's/Aobserve RDATA ANALOG_RAW/Aobserve RHELD ANALOG_RAW/' \
                "$dir/native-original-real-event-chain.lib" ;;
        forbidden-real-delay)
            sed -i 's/ORIGINAL_DELAY real_delay(delay=800p)/ORIGINAL_DELAY real_delay(delay=0)/' \
                "$dir/native-original-real-event-chain.lib" ;;
        forbidden-d2r-delay)
            sed -i 's/d_to_real(zero=-0.5 one=2 delay=350p)/d_to_real(zero=-0.5 one=2 delay=0)/' \
                "$dir/native-original-real-event-chain.lib" ;;
        forbidden-real-ramp)
            sed -i 's/transition_time=400p/transition_time=0/' \
                "$dir/native-original-real-event-chain.lib" ;;
        undeclared-real-parameter)
            sed -i 's/transition_time=400p/transition_time=400p unknown_real_arg=1/' \
                "$dir/native-original-real-event-chain.lib" ;;
        duplicate-real-delay)
            sed -i 's/real_delay(delay=800p)/real_delay(delay=800p delay=2n)/' \
                "$dir/native-original-real-event-chain.lib" ;;
        digital-node-used-as-real)
            sed -i 's/Adelay RDATA CLOCK RHELD/Adelay DATA CLOCK RHELD/' \
                "$dir/native-original-real-event-chain.lib"
            jq '.code_model_contracts[1].ports.in=["DATA"]' \
                "$config" >"$dir/config.json"
            invalid_config="$dir/config.json" ;;
        real-node-used-as-clock)
            sed -i 's/Adelay RDATA CLOCK RHELD/Adelay RDATA RDATA RHELD/' \
                "$dir/native-original-real-event-chain.lib"
            jq '.code_model_contracts[1].ports.clk=["RDATA"]' \
                "$config" >"$dir/config.json"
            invalid_config="$dir/config.json" ;;
        analog-node-used-as-real)
            sed -i 's/Aheld RHELD ANALOG_SAMPLED/Aheld ANALOG_RAW ANALOG_SAMPLED/' \
                "$dir/native-original-real-event-chain.lib"
            jq '.code_model_contracts[3].ports.in=["ANALOG_RAW"]' \
                "$config" >"$dir/config.json"
            invalid_config="$dir/config.json" ;;
    esac
    if "$bins/alb-boundary-discover" \
        "$dir/original.cir" "$invalid_config" "$dir/discovered.json" \
        >"$dir/rejected.log" 2>&1; then
        printf 'Invalid original real-event IFS escaped: %s\n' "$corrupt" >&2
        exit 1
    fi
    rg -i -q 'IFS|code.model|port|parameter|native|miswired' "$dir/rejected.log" || {
        tail -65 "$dir/rejected.log" >&2
        exit 1
    }
    test ! -e "$dir/discovered.json"
    printf 'Original real-event IFS preflight rejection PASS %s\n' "$corrupt"
done

"$here/compile-original-family-circuit.sh" \
    "$bins" "$work/native" "$config" "$fixtures/auto-models.json"
rg -F -x -q \
    'Xexternal NDATA NDATA_SR_SINK NDATA_SR_SOURCE NCLOCK NCLOCK_SR_SINK NCLOCK_SR_SOURCE ANALOG_RAW ANALOG_SAMPLED SR_ORIGINAL_REAL_EVENT' \
    "$work/native/circuit.cir" || {
    echo 'Original real-valued event pipeline changed physical X ABI' >&2
    exit 1
}
[[ $(rg -F -c 'sr_family_adc(' "$work/native/circuit.cir") == 2 ]] || {
    echo 'Expected two source-derived original ALB event input ADCs' >&2
    exit 1
}
for actual in \
    'Ad2r DATA NULL RDATA ORIGINAL_D2R' \
    'Adelay RDATA CLOCK RHELD ORIGINAL_DELAY' \
    'Aobserve RDATA ANALOG_RAW ORIGINAL_R2V' \
    'Aheld RHELD ANALOG_SAMPLED ORIGINAL_R2V'; do
    rg -F -x -q "$actual" "$work/native/native-original-real-event-chain.lib"
done

for route in stock native; do
    deck=original.cir
    [[ $route == native ]] && deck=circuit.cir
    "$here/run-original-ngspice.sh" "$work/$route" "$deck" result.dat
done
compare() {
    awk -f "$here/compare-original-real-event-chain.awk" \
        "$work/stock/result.dat" "$1"
}
compare "$work/native/result.dat" || {
    echo 'Original real-event C model and ALB-compiled native X waveforms differ' >&2
    exit 1
}

# Each corruption runs as actual original C code in ngspice47, and must fail
# the independent event state / sampled real / 2pF timing oracle.
for corrupt in wrong-real-zero wrong-real-one wrong-real-delay \
               wrong-d2r-latency wrong-real-gain wrong-real-ramp \
               swapped-raw-held removed-event-receiver-load \
               wrong-sample-clock; do
    dir="$work/bad-physical-$corrupt"
    mkdir -p "$dir"
    cp -- "$work/native/circuit.cir" "$work/native/generated.inc" \
          "$work/native/native-original-real-event-chain.lib" \
          "$work/native/.spiceinit" "$dir/"
    case "$corrupt" in
        wrong-real-zero)
            sed -i 's/zero=-0.5 one=2/zero=0 one=2/' \
                "$dir/native-original-real-event-chain.lib" ;;
        wrong-real-one)
            sed -i 's/zero=-0.5 one=2/zero=-0.5 one=2.5/' \
                "$dir/native-original-real-event-chain.lib" ;;
        wrong-real-delay)
            sed -i 's/real_delay(delay=800p)/real_delay(delay=2n)/' \
                "$dir/native-original-real-event-chain.lib" ;;
        wrong-d2r-latency)
            sed -i 's/zero=-0.5 one=2 delay=350p/zero=-0.5 one=2 delay=2n/' \
                "$dir/native-original-real-event-chain.lib" ;;
        wrong-real-gain)
            sed -i 's/real_to_v(gain=1 transition_time=400p)/real_to_v(gain=1.25 transition_time=400p)/' \
                "$dir/native-original-real-event-chain.lib" ;;
        wrong-real-ramp)
            sed -i 's/transition_time=400p/transition_time=2n/' \
                "$dir/native-original-real-event-chain.lib" ;;
        swapped-raw-held)
            sed -i \
                -e 's/Aobserve RDATA ANALOG_RAW/Aobserve RDATA ANALOG_SAMPLED/' \
                -e 's/Aheld RHELD ANALOG_SAMPLED/Aheld RHELD ANALOG_RAW/' \
                "$dir/native-original-real-event-chain.lib" ;;
        removed-event-receiver-load)
            sed -i 's/capacitance=2p/capacitance=0/' \
                "$dir/native-original-real-event-chain.lib" ;;
        wrong-sample-clock)
            sed -i 's/Adelay RDATA CLOCK RHELD/Adelay RDATA DATA RHELD/' \
                "$dir/native-original-real-event-chain.lib" ;;
    esac
    "$here/run-original-ngspice.sh" "$dir" circuit.cir result.dat
    if compare "$dir/result.dat" >"$dir/rejected.log" 2>&1; then
        printf 'Original real-event physical corruption escaped: %s\n' "$corrupt" >&2
        exit 1
    fi
    printf 'Original real-event physical mutation rejection PASS %s\n' "$corrupt"
done
echo 'AUTO PHYSICAL PASS original-real-event-chain-x (original digital→real sample/hold→analog with 200ps input loading)'
