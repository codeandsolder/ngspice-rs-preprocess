#!/usr/bin/env bash
# Exact unchanged ngspice47 original d_to_real UNKNOWN midpoint overflow,
# with true original XSPICE event observations at +180ps. The nonfinite
# real event remains separate from physically loaded finite analog probes.
set -euo pipefail
here="$(cd -- "$(dirname -- "$0")" && pwd)"
# shellcheck source=runtime/xspice/prepare-original-smoke.sh
source "$here/prepare-original-smoke.sh"
prepare_original_smoke "original_midpoint" "$@"
# The two independent bug reproducers use only the built-in ngspice47
# original C models: no project code model or ALB compiler involved.
standalone="$work/stock-original-midpoint-only"
mkdir -p "$standalone"
cp -- "$here/fixtures/upstream-repros/d_to_real-midpoint-overflow/"*.cir "$standalone/"
cp -- "$here/fixtures/upstream-repros/d_to_real-midpoint-overflow/"*.txt "$standalone/"
for mode in ungated disabled; do
    (
        cd "$standalone"
        if [[ $mode == ungated ]]; then deck=repro.cir; else deck=repro-enabled.cir; fi
        ngspice -b "$deck" >"run-$mode.log" 2>&1 || {
            tail -85 "run-$mode.log" >&2
            exit 1
        }
        test -s events.txt
        cp -- events.txt "events-$mode.txt"
        if [[ $mode == ungated ]]; then disabled=0; else disabled=1; fi
        awk -v DISABLED="$disabled" -f "$here/verify-original-real-midpoint-standalone.awk"             "events-$mode.txt"
    )
done

stage_original_family_circuit "real-nonfinite-midpoint"
cp -- "$fixtures/original-real-sum-driver-"*.txt "$work/stock/"
cp -- "$fixtures/original-real-sum-driver-"*.txt "$work/native/"
cp -- "$fixtures/georgiatech-d_source-original.ifs" "$work/native/"
for model in d_to_real real_gain real_to_v; do
    cp -- "$fixtures/original-xtraevt-$model.ifs" "$work/native/"
done
library="native-original-real-nonfinite-midpoint.lib"

for poison in missing-midpoint-source wrong-midpoint-type \
              scalar-digital-source forbidden-midpoint-null \
              duplicate-midpoint-parameter wrong-original-midpoint-model-kind; do
    bad="$work/invalid-$poison"
    mkdir -p "$bad"
    cp -- "$work/native/original.cir" "$work/native/$library" \
          "$work/native/georgiatech-d_source-original.ifs" \
          "$work/native/original-xtraevt-"*.ifs "$bad/"
    bad_config="$config"
    case "$poison" in
        missing-midpoint-source)
            sed -i 's/Amidpoint Q0 NULL R_MIDPOINT ORIGINAL_MIDPOINT/Amissing Q0 NULL R_MIDPOINT ORIGINAL_MIDPOINT/' \
                "$bad/$library" ;;
        wrong-midpoint-type)
            sed -i 's/Amidpoint Q0 NULL R_MIDPOINT ORIGINAL_MIDPOINT/Amidpoint Q0 NULL Q0 ORIGINAL_MIDPOINT/' "$bad/$library"
            jq '(.code_model_contracts[] | select(.instance=="Amidpoint") | .ports.out)=["Q0"]' "$config" >"$bad/poisoned.json"
            bad_config="$bad/poisoned.json" ;;
        scalar-digital-source)
            sed -i 's/Asource0 \[Q0\] ORIGINAL_SOURCE0/Asource0 Q0 ORIGINAL_SOURCE0/' "$bad/$library" ;;
        forbidden-midpoint-null)
            sed -i 's/Amidpoint Q0 NULL R_MIDPOINT ORIGINAL_MIDPOINT/Amidpoint NULL NULL R_MIDPOINT ORIGINAL_MIDPOINT/' "$bad/$library"
            jq '(.code_model_contracts[] | select(.instance=="Amidpoint") | .ports.in)=["NULL"]' "$config" >"$bad/poisoned.json"
            bad_config="$bad/poisoned.json" ;;
        duplicate-midpoint-parameter)
            sed -i 's/ORIGINAL_MIDPOINT d_to_real(zero=1e308 one=1e308 delay=180p)/ORIGINAL_MIDPOINT d_to_real(zero=1e308 zero=1e307 one=1e308 delay=180p)/' "$bad/$library" ;;
        wrong-original-midpoint-model-kind)
            sed -i 's/ORIGINAL_MIDPOINT d_to_real(/ORIGINAL_MIDPOINT real_gain(/' "$bad/$library" ;;
    esac
    if "$bins/alb-boundary-discover" "$bad/original.cir" "$bad_config" \
        "$bad/discovered.json" >"$bad/rejected.log" 2>&1; then
        printf 'Invalid original C midpoint source/IFS escaped: %s\n' "$poison" >&2
        exit 1
    fi
    rg -i -q 'IFS|port|parameter|model|binding|NULL' "$bad/rejected.log" || {
        tail -75 "$bad/rejected.log" >&2
        exit 1
    }
    test ! -e "$bad/discovered.json"
    echo "Original XSPICE midpoint IFS rejection PASS $poison"
done
"$bins/alb-boundary-discover" "$work/native/original.cir" "$config" \
    "$work/native/discovered.json"
"$bins/alb-logical-expand" "$work/native/original.cir" \
    "$work/native/discovered.json" "$work/native/lowered.cir" \
    "$work/native/generated.inc"
"$bins/alb-family-autowire" "$work/native/lowered.cir" \
    "$work/native/generated.inc" "$work/native/discovered.json" \
    "$fixtures/auto-models.json" "$work/native/circuit.cir"
rg -F -q 'Amidpoint Q0 NULL R_MIDPOINT ORIGINAL_MIDPOINT' "$work/native/$library" || {
    echo 'Missing original 1991 digital→real midpoint event C model' >&2;exit 1;
}
for route in stock native;do
    (
        cd "$work/$route"
        deck=circuit.cir
        if [[ $route == stock ]]; then deck=original.cir; fi
        ngspice -b "$deck" >run.log 2>&1 || { tail -95 run.log >&2; exit 1; }
        if rg -i 'error:|fatal error|timestep too small|no such model' run.log; then
            echo "Original midpoint source $route simulator failed" >&2;exit 1;
        fi
        test -s original-events.txt && test -s result.dat
    )
done
check_midpoint() {
    awk -f "$here/verify-original-real-midpoint.awk" \
        "$work/stock/original-events.txt" "$1"
}
# The established original nonfinite and finite electrical oracle uses
# the same signals after dropping BOTH extra original midpoint event
# columns from the genuine original C-model eprint table.
strip_midpoint() {
    awk '$1 ~ /^[0-9]/ {
        printf "%s",$1
        for(i=4;i<=NF;i++)printf " %s",$i
        printf "\n"
    }' "$1" >"$2"
}
strip_midpoint "$work/stock/original-events.txt" "$work/stock/without-midpoint.txt"
strip_midpoint "$work/native/original-events.txt" "$work/native/without-midpoint.txt"
check_base() {
    awk -v VARIANT=overflow-first -f "$here/family-dc-equilibrium.awk" -f "$here/verify-original-real-nonfinite.awk" \
        "$work/stock/without-midpoint.txt" "$1" \
        "$work/stock/result.dat" "$2"
}
check_midpoint "$work/native/original-events.txt"
check_base "$work/native/without-midpoint.txt" "$work/native/result.dat"

for poison in wrong-midpoint-average wrong-midpoint-input wrong-midpoint-delay \
              bypassed-midpoint-enable wrong-gated-midpoint-input \
              wrong-finite-original-gain missing-physical-receiver \
              wrong-real-meta-input swapped-finite-analog-outputs;do
    bad="$work/poisoned-$poison"
    mkdir -p "$bad"
    cp -- "$work/native/circuit.cir" "$work/native/generated.inc" \
          "$work/native/$library" "$work/native/.spiceinit" \
          "$work/native/original-real-sum-driver-"*.txt "$bad/"
    case "$poison" in
        wrong-midpoint-average)
            sed -i 's/zero=1e308 one=1e308 delay=180p/zero=8e307 one=8e307 delay=180p/' \
                "$bad/$library" ;;
        wrong-midpoint-input)
            sed -i 's/Amidpoint Q0 NULL R_MIDPOINT/Amidpoint EN NULL R_MIDPOINT/' "$bad/$library" ;;
        wrong-midpoint-delay)
            sed -i 's/zero=1e308 one=1e308 delay=180p/zero=1e308 one=1e308 delay=250p/' \
                "$bad/$library" ;;
        bypassed-midpoint-enable)
            sed -i 's/AmidpointGated Q0 EN R_MIDPOINT_GATED/AmidpointGated Q0 NULL R_MIDPOINT_GATED/' \
                "$bad/$library" ;;
        wrong-gated-midpoint-input)
            sed -i 's/AmidpointGated Q0 EN R_MIDPOINT_GATED/AmidpointGated EN EN R_MIDPOINT_GATED/' \
                "$bad/$library" ;;
        wrong-finite-original-gain)
            sed -i 's/ORIGINAL_GAIN_POST real_gain(in_offset=0.375 gain=-0.5/ORIGINAL_GAIN_POST real_gain(in_offset=0.375 gain=-0.75/' \
                "$bad/$library" ;;
        missing-physical-receiver)
            sed -i 's/capacitance=2p/capacitance=0/' "$bad/$library" ;;
        wrong-real-meta-input)
            sed -i 's/AgainNonfinite REAL_SUM R_META/AgainNonfinite BPRE R_META/' "$bad/$library" ;;
        swapped-finite-analog-outputs)
            sed -i \
                -e 's/Areadout BPRE ANALOG_SUM/Areadout BPRE ANALOG_POST/' \
                -e 's/Apost RPOST ANALOG_POST/Apost RPOST ANALOG_SUM/' "$bad/$library" ;;
    esac
    (
        cd "$bad"
        ngspice -b circuit.cir >run.log 2>&1 || { tail -100 run.log >&2; exit 1; }
        if rg -i 'error:|fatal error|timestep too small|no such model' run.log; then
            echo "Original midpoint C-model poison cannot simulate: $poison" >&2;exit 1;
        fi
        test -s original-events.txt && test -s result.dat
    )
    strip_midpoint "$bad/original-events.txt" "$bad/without-midpoint.txt"
    if check_midpoint "$bad/original-events.txt" >"$bad/midpoint-rejected.log" 2>&1 &&
       check_base "$bad/without-midpoint.txt" "$bad/result.dat" >"$bad/physical-rejected.log" 2>&1;then
        echo "Original midpoint actual C-model corruption escaped: $poison" >&2
        exit 1
    fi
    echo "Original midpoint actual C-model simulation mutation rejection PASS $poison"
done
echo 'AUTO PHYSICAL PASS original-real-nonfinite-midpoint (true original d_to_real UNKNOWN midpoint overflow with independently loaded analog state)'
