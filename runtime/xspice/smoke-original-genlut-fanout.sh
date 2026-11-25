#!/usr/bin/env bash
# Real multi-output original d_genlut (2016 Tim Edwards) → original
# Georgia Tech d_and/d_xor (1991) with independent physical fanout.
set -euo pipefail
if (( $# != 3 )); then
    printf 'usage: %s CODEMODEL_CM RUST_BIN_DIR SCRATCH_ROOT\n' "$0" >&2
    exit 2
fi
here="$(cd -- "$(dirname -- "$0")" && pwd)"
cm="$(realpath -- "$1")"
bins="$(realpath -- "$2")"
scratch="$(realpath -- "$3")"
for command in ngspice rg awk; do
    command -v "$command" >/dev/null
done
for compiler in alb-boundary-discover alb-logical-expand alb-family-autowire; do
    test -x "$bins/$compiler" || { printf 'Missing %s\n' "$compiler" >&2; exit 1; }
done
work="$(mktemp -d "$scratch/ngspice_preprocess_genlut_fanout_XXXXXXXX")"
trap 'rm -rf -- "$work"' EXIT
fixtures="$here/fixtures/family"
config="$fixtures/auto-discovery-original-genlut-fanout-x.json"
mkdir -p "$work/stock" "$work/native"
cp -- "$fixtures/original-genlut-fanout-reference.cir" "$work/stock/original.cir"
cp -- "$fixtures/logical-original-genlut-fanout-x.cir" "$work/native/original.cir"
cp -- "$fixtures/native-original-genlut-fanout.lib" "$work/native/"
cp -- "$fixtures/tim-edwards-d_genlut-original.ifs" "$work/native/"
for model in d_and d_xor; do
    cp -- "$fixtures/georgiatech-$model-original.ifs" "$work/native/"
done
printf 'codemodel %s\n' "$cm" >"$work/native/.spiceinit"
cp -- "$work/native/.spiceinit" "$work/stock/"

# All three independent, genuine IFS contracts are checked against the
# actual original-model A instances, not inferred from the desired wrapper.
for sabotage in swapped-inputs swapped-genlut-outputs wrong-and-fanout \
                wrong-xor-bus wrong-code-model restricted-ifs-input \
                short-truth-table long-truth-table undeclared-truth-symbol \
                rise-vector-short fall-vector-short input-delay-short \
                input-load-short missing-truth-table duplicate-truth-table \
                extra-parameter invalid-delay nonnumeric-delay \
                invalid-boolean scalar-as-vector;
do
    dir="$work/bad-ifs-$sabotage"
    mkdir -p "$dir"
    cp -- "$work/native/original.cir" "$work/native/native-original-genlut-fanout.lib" \
          "$work/native/tim-edwards-d_genlut-original.ifs" \
          "$work/native/georgiatech-d_and-original.ifs" \
          "$work/native/georgiatech-d_xor-original.ifs" "$dir/"
    case "$sabotage" in
        swapped-inputs)
            sed -i 's/AGEN \[I0 I1\]/AGEN [I1 I0]/' \
                "$dir/native-original-genlut-fanout.lib" ;;
        swapped-genlut-outputs)
            sed -i 's/\[Q0_EVT Q1_EVT Q2_EVT\]/[Q1_EVT Q0_EVT Q2_EVT]/' \
                "$dir/native-original-genlut-fanout.lib" ;;
        wrong-and-fanout)
            sed -i 's/AAND \[Q0_EVT Q2_EVT\]/AAND [Q1_EVT Q2_EVT]/' \
                "$dir/native-original-genlut-fanout.lib" ;;
        wrong-xor-bus)
            sed -i 's/AXOR \[Q0_EVT Q1_EVT Q2_EVT\]/AXOR [Q0_EVT Q2_EVT]/' \
                "$dir/native-original-genlut-fanout.lib" ;;
        wrong-code-model)
            sed -i 's/ORIG_GEN d_genlut(/ORIG_GEN d_lut(/' \
                "$dir/native-original-genlut-fanout.lib" ;;
        restricted-ifs-input)
            sed -i 's/Vector_Bounds:       -                  -/Vector_Bounds:       [3 -]              -/' \
                "$dir/tim-edwards-d_genlut-original.ifs" ;;
        short-truth-table)
            sed -i 's/table_values="01100001z01z"/table_values="01100001z01"/' \
                "$dir/native-original-genlut-fanout.lib" ;;
        long-truth-table)
            sed -i 's/table_values="01100001z01z"/table_values="01100001z01z0"/' \
                "$dir/native-original-genlut-fanout.lib" ;;
        undeclared-truth-symbol)
            sed -i 's/table_values="01100001z01z"/table_values="01100001Z01z"/' \
                "$dir/native-original-genlut-fanout.lib" ;;
        rise-vector-short)
            sed -i 's/rise_delay=\[550p 750p 900p\]/rise_delay=[550p 750p]/' \
                "$dir/native-original-genlut-fanout.lib" ;;
        fall-vector-short)
            sed -i 's/fall_delay=\[650p 850p 1100p\]/fall_delay=[650p 850p]/' \
                "$dir/native-original-genlut-fanout.lib" ;;
        input-delay-short)
            sed -i 's/input_delay=\[150p 250p\]/input_delay=[150p]/' \
                "$dir/native-original-genlut-fanout.lib" ;;
        input-load-short)
            sed -i 's/input_load=\[0 0\]/input_load=[0]/' \
                "$dir/native-original-genlut-fanout.lib" ;;
        missing-truth-table)
            sed -i 's/table_values="01100001z01z" //' \
                "$dir/native-original-genlut-fanout.lib" ;;
        duplicate-truth-table)
            sed -i 's/input_delay=\[150p 250p\])/input_delay=[150p 250p] table_values="01100001z01z")/' \
                "$dir/native-original-genlut-fanout.lib" ;;
        extra-parameter)
            sed -i 's/input_delay=\[150p 250p\])/input_delay=[150p 250p] undefined_parameter=1)/' \
                "$dir/native-original-genlut-fanout.lib" ;;
        invalid-delay)
            sed -i 's/rise_delay=\[550p 750p 900p\]/rise_delay=[0 750p 900p]/' \
                "$dir/native-original-genlut-fanout.lib" ;;
        nonnumeric-delay)
            sed -i 's/rise_delay=\[550p 750p 900p\]/rise_delay=[wrong 750p 900p]/' \
                "$dir/native-original-genlut-fanout.lib" ;;
        invalid-boolean)
            sed -i 's/inertial_delay=false/inertial_delay=maybe/' \
                "$dir/native-original-genlut-fanout.lib" ;;
        scalar-as-vector)
            sed -i 's/inertial_delay=false/inertial_delay=[false]/' \
                "$dir/native-original-genlut-fanout.lib" ;;
    esac
    if "$bins/alb-boundary-discover" "$dir/original.cir" "$config" \
        "$dir/invalid-discovery.json" >"$dir/error.log" 2>&1; then
        printf 'Wrong original multi-code-model contract accepted: %s\n' "$sabotage" >&2
        exit 1
    fi
    rg -i -q 'IFS|code.model|lookup|parameter' "$dir/error.log" || {
        tail -45 "$dir/error.log" >&2
        exit 1
    }
    test ! -e "$dir/invalid-discovery.json"
    printf 'Original multi-model IFS preflight rejection PASS %s\n' "$sabotage"
done
"$bins/alb-boundary-discover" \
    "$work/native/original.cir" "$config" "$work/native/discovered.json"
"$bins/alb-logical-expand" \
    "$work/native/original.cir" "$work/native/discovered.json" \
    "$work/native/lowered.cir" "$work/native/generated.inc"
"$bins/alb-family-autowire" \
    "$work/native/lowered.cir" "$work/native/generated.inc" \
    "$work/native/discovered.json" "$fixtures/auto-models.json" \
    "$work/native/circuit.cir"

rg -F -x -q 'Xexternal I0 I0_SR_SINK I0_SR_SOURCE I1 I1_SR_SINK I1_SR_SOURCE ANALOG_Q0 ANALOG_Q1 ANALOG_Q2 ANALOG_F0 ANALOG_F1 SR_GENLUT_FANOUT' \
    "$work/native/circuit.cir" || {
    echo 'Wrong fully physical multi-output native X ABI' >&2
    exit 1
}
[[ $(rg -F -c 'sr_family_adc(' "$work/native/circuit.cir") == 2 ]] || {
    echo 'Expected two original vector inputs to create precisely two physical ADCs' >&2
    exit 1
}
[[ $(rg -c '^ALOAD' "$work/native/native-original-genlut-fanout.lib") == 2 ]] || {
    echo 'Expected two independent native physical event receiver bundles' >&2
    exit 1
}
for original in \
    'AGEN [I0 I1] [Q0_EVT Q1_EVT Q2_EVT] ORIG_GEN' \
    'AAND [Q0_EVT Q2_EVT] F0_EVT ORIG_AND' \
    'AXOR [Q0_EVT Q1_EVT Q2_EVT] F1_EVT ORIG_XOR'; do
    rg -F -x -q "$original" "$work/native/native-original-genlut-fanout.lib"
done
for mode in stock native; do
    (
        cd "$work/$mode"
        if [[ $mode == stock ]]; then deck=original.cir; else deck=circuit.cir; fi
        ngspice -b "$deck" >run.log 2>&1 || {
            tail -95 run.log >&2
            exit 1
        }
        if rg -i 'error:|fatal error|timestep too small|no such model' run.log; then
            echo "Original XSPICE multi-output fanout simulator failed $mode" >&2
            exit 1
        fi
        test -s result.dat && test -s result.events
    )
done
compare() {
    awk -f "$here/compare-original-genlut-fanout.awk" \
        "$work/stock/result.dat" "$1"
}
verify_events() {
    awk -f "$here/verify-original-genlut-events.awk" "$1"
}
verify_events "$work/stock/result.events"
verify_events "$work/native/result.events"
compare "$work/native/result.dat" || {
    echo 'Original XSPICE d_genlut digital fanout and physical loads diverged' >&2
    exit 1
}

# Each mutated original native model executes in the real ngspice47
# simulator with original C code models, but must fail the independent
# five-output physical-voltage/KCL/timing or exact event-state oracle.
# Floating undetermined and high-Z UNKNOWN have the same physical
# equilibrium; eprint distinguishes their actual event strength.
for sabotage in wrong-lut-contents z-to-unknown wrong-downstream-and \
                removed-input-load reversed-fanout-analog;
do
    dir="$work/bad-behavior-$sabotage"
    mkdir -p "$dir"
    cp -- "$work/native/circuit.cir" "$work/native/generated.inc" \
          "$work/native/native-original-genlut-fanout.lib" \
          "$work/native/.spiceinit" "$dir/"
    case "$sabotage" in
        wrong-lut-contents)
            sed -i 's/table_values="01100001z01z"/table_values="01100001z11z"/' \
                "$dir/native-original-genlut-fanout.lib" ;;
        z-to-unknown)
            sed -i 's/table_values="01100001z01z"/table_values="01100001x01x"/' \
                "$dir/native-original-genlut-fanout.lib" ;;
        wrong-downstream-and)
            sed -i 's/ORIG_AND d_and(/ORIG_AND d_or(/' \
                "$dir/native-original-genlut-fanout.lib" ;;
        removed-input-load)
            sed -i 's/capacitance=2p/capacitance=0/' \
                "$dir/native-original-genlut-fanout.lib" ;;
        reversed-fanout-analog)
            sed -i 's/Aout3 F0_EVT/Aout3 F1_EVT/' \
                "$dir/native-original-genlut-fanout.lib" ;;
    esac
    (
        cd "$dir"
        ngspice -b circuit.cir >run.log 2>&1 || {
            tail -85 run.log >&2
            exit 1
        }
        if rg -i 'error:|fatal error|timestep too small|no such model' run.log; then
            printf 'Original genlut behavior mutation cannot simulate: %s\n' "$sabotage" >&2
            exit 1
        fi
        test -s result.dat && test -s result.events
    )
    if compare "$dir/result.dat" >"$dir/error.log" 2>&1 &&
       verify_events "$dir/result.events" >>"$dir/error.log" 2>&1; then
        printf 'Original genlut fanout corruption escaped waveform oracle: %s\n' "$sabotage" >&2
        exit 1
    fi
    printf 'Original multi-model physical mutation rejection PASS %s\n' "$sabotage"
done
echo 'AUTO PHYSICAL PASS original-genlut-fanout-x (three original code models, 5 physical outputs, HI_Z and UNKNOWN, 24 timed transitions)'
