#!/usr/bin/env bash
# Actual original ngspice47 XSPICE real-event NaN/Inf/overflow source proof.
# Nonfinite user-defined REAL events remain isolated from the analog MNA,
# while genuine original finite C-model outputs feed physical loaded probes.
set -euo pipefail
here="$(cd -- "$(dirname -- "$0")" && pwd)"
# shellcheck source=runtime/xspice/prepare-original-smoke.sh
source "$here/prepare-original-smoke.sh"
prepare_original_smoke "real_nonfinite" "$@"
for variant in overflow-first overflow-middle nan-first nan-middle; do
    dir="$work/$variant"
    mkdir -p "$dir/stock" "$dir/native"
    cp -- "$fixtures/original-real-nonfinite-$variant-reference.cir" "$dir/stock/original.cir"
    cp -- "$fixtures/logical-original-real-nonfinite-$variant-x.cir" "$dir/native/original.cir"
    cp -- "$fixtures/native-original-real-nonfinite-$variant.lib" "$dir/native/"
    cp -- "$fixtures/original-real-sum-driver-"*.txt "$dir/stock/"
    cp -- "$fixtures/original-real-sum-driver-"*.txt "$dir/native/"
    cp -- "$fixtures/georgiatech-d_source-original.ifs" "$dir/native/"
    for model in d_to_real real_gain real_to_v; do
        cp -- "$fixtures/original-xtraevt-$model.ifs" "$dir/native/"
    done
    printf 'codemodel %s\n' "$cm" >"$dir/stock/.spiceinit"
    cp -- "$dir/stock/.spiceinit" "$dir/native/"
    config="$fixtures/auto-discovery-original-real-nonfinite-$variant-x.json"
    library="native-original-real-nonfinite-$variant.lib"
    for poison in missing-original-source scalar-original-source \
                  wrong-original-source-kind mismatched-original-real-input \
                  mismatched-original-real-output duplicate-original-gain \
                  wrong-original-real-gain-kind; do
        bad="$dir/invalid-$poison"
        mkdir -p "$bad"
        cp -- "$dir/native/original.cir" "$dir/native/$library" \
              "$dir/native/georgiatech-d_source-original.ifs" \
              "$dir/native/original-xtraevt-"*.ifs "$bad/"
        bad_config="$config"
        case "$poison" in
            missing-original-source)
                sed -i 's/Asource0 \[Q0\] ORIGINAL_SOURCE0/Abroken [Q0] ORIGINAL_SOURCE0/' "$bad/$library" ;;
            scalar-original-source)
                sed -i 's/Asource0 \[Q0\] ORIGINAL_SOURCE0/Asource0 Q0 ORIGINAL_SOURCE0/' "$bad/$library" ;;
            wrong-original-source-kind)
                sed -i 's/ORIGINAL_SOURCE0 d_source(/ORIGINAL_SOURCE0 d_ram(/' "$bad/$library" ;;
            mismatched-original-real-input)
                sed -i 's/AgainNonfinite REAL_SUM R_META/AgainNonfinite Q0 R_META/' "$bad/$library"
                jq '(.code_model_contracts[] | select(.instance=="AgainNonfinite") | .ports.in)=["Q0"]' \
                    "$config" >"$bad/poisoned.json"
                bad_config="$bad/poisoned.json" ;;
            mismatched-original-real-output)
                sed -i 's/AgainNonfinite REAL_SUM R_META/AgainNonfinite REAL_SUM Q0/' "$bad/$library"
                jq '(.code_model_contracts[] | select(.instance=="AgainNonfinite") | .ports.out)=["Q0"]' \
                    "$config" >"$bad/poisoned.json"
                bad_config="$bad/poisoned.json" ;;
            duplicate-original-gain)
                sed -i 's/ORIGINAL_META real_gain(in_offset=0 gain=1 out_offset/ORIGINAL_META real_gain(in_offset=0 gain=1 gain=2 out_offset/' \
                    "$bad/$library" ;;
            wrong-original-real-gain-kind)
                sed -i 's/ORIGINAL_META real_gain(/ORIGINAL_META real_to_v(/' "$bad/$library" ;;
        esac
        if "$bins/alb-boundary-discover" "$bad/original.cir" "$bad_config" \
            "$bad/discovered.json" >"$bad/rejected.log" 2>&1; then
            printf 'Invalid original real special-value IFS accepted: %s %s\n' \
                "$variant" "$poison" >&2
            exit 1
        fi
        rg -i -q 'IFS|port|model|binding|parameter|NULL|shared' "$bad/rejected.log" || {
            tail -85 "$bad/rejected.log" >&2
            exit 1
        }
        test ! -e "$bad/discovered.json"
        echo "Original real nonfinite $variant original-IFS rejection PASS $poison"
    done
    "$bins/alb-boundary-discover" "$dir/native/original.cir" "$config" \
        "$dir/native/discovered.json"
    "$bins/alb-logical-expand" "$dir/native/original.cir" \
        "$dir/native/discovered.json" "$dir/native/lowered.cir" "$dir/native/generated.inc"
    "$bins/alb-family-autowire" "$dir/native/lowered.cir" \
        "$dir/native/generated.inc" "$dir/native/discovered.json" \
        "$fixtures/auto-models.json" "$dir/native/circuit.cir"
    rg -F -x -q \
        'Xexternal LEN LEN_SR_SINK LEN_SR_SOURCE ANALOG_SUM ANALOG_POST ANALOG_ENABLED ANALOG_PROBE SR_ORIGINAL_REAL_SUM' \
        "$dir/native/circuit.cir" || {
        echo "Original real special value $variant source ABI changed" >&2
        exit 1
    }
    rg -F -q 'AgainNonfinite REAL_SUM R_META ORIGINAL_META' "$dir/native/$library" || {
        echo "Missing genuine original real-to-real propagation" >&2
        exit 1
    }
    for route in stock native; do
        (
            cd "$dir/$route"
            deck=circuit.cir
            if [[ $route == stock ]]; then deck=original.cir; fi
            ngspice -b "$deck" >run.log 2>&1 || { tail -100 run.log >&2; exit 1; }
            if rg -i 'error:|fatal error|timestep too small|no such model' run.log; then
                printf 'Original real special event simulation failed %s/%s\n' "$variant" "$route" >&2
                exit 1
            fi
            test -s original-events.txt && test -s result.dat
        )
    done
    oracle() {
        awk -v VARIANT="$variant" -f "$here/family-dc-equilibrium.awk" -f "$here/verify-original-real-nonfinite.awk" \
            "$dir/stock/original-events.txt" "$1" \
            "$dir/stock/result.dat" "$2"
    }
    oracle "$dir/native/original-events.txt" "$dir/native/result.dat"

    for poison in reversed-driver-order corrupt-q0-source wrong-finite-bridge \
                  wrong-finite-gain wrong-real-propagation removed-receiver \
                  swapped-analog-outputs wrong-large-A; do
        if [[ "$poison" == wrong-large-A && "$variant" == nan-middle ]]; then
            continue
        fi
        bad="$dir/corrupted-$poison"
        mkdir -p "$bad"
        cp -- "$dir/native/circuit.cir" "$dir/native/generated.inc" \
              "$dir/native/$library" "$dir/native/.spiceinit" \
              "$dir/native/original-real-sum-driver-"*.txt "$bad/"
        case "$poison" in
            reversed-driver-order)
                alternative=overflow-first
                case "$variant" in
                    overflow-first) alternative=overflow-middle ;;
                    overflow-middle) alternative=overflow-first ;;
                    nan-first) alternative=nan-middle ;;
                    nan-middle) alternative=nan-first ;;
                esac
                cp -- "$fixtures/native-original-real-nonfinite-$alternative.lib" \
                    "$bad/$library" ;;
            corrupt-q0-source)
                sed -i 's/0s/1s/g' "$bad/original-real-sum-driver-0.txt" ;;
            wrong-finite-bridge)
                sed -i 's/zero=1.25 one=-0.75/zero=2.25 one=-0.75/' \
                    "$bad/$library" ;;
            wrong-finite-gain)
                sed -i 's/ORIGINAL_GAIN_POST real_gain(in_offset=0.375 gain=-0.5/ORIGINAL_GAIN_POST real_gain(in_offset=0.375 gain=-0.75/' \
                    "$bad/$library" ;;
            wrong-real-propagation)
                sed -i 's/AgainNonfinite REAL_SUM R_META/AgainNonfinite BPRE R_META/' \
                    "$bad/$library" ;;
            removed-receiver)
                sed -i 's/capacitance=2p/capacitance=0/' "$bad/$library" ;;
            swapped-analog-outputs)
                sed -i \
                    -e 's/Areadout BPRE ANALOG_SUM/Areadout BPRE ANALOG_POST/' \
                    -e 's/Apost RPOST ANALOG_POST/Apost RPOST ANALOG_SUM/' \
                    "$bad/$library" ;;
            wrong-large-A)
                sed -i 's/zero=8e307 one=8e307/zero=0 one=0/' \
                    "$bad/$library" ;;
        esac
        (
            cd "$bad"
            ngspice -b circuit.cir >run.log 2>&1 || { tail -100 run.log >&2; exit 1; }
            if rg -i 'error:|fatal error|timestep too small|no such model' run.log; then
                printf 'Original nonfinite C-model corruption cannot simulate: %s/%s\n' "$variant" "$poison" >&2
                exit 1
            fi
            test -s original-events.txt && test -s result.dat
        )
        if oracle "$bad/original-events.txt" "$bad/result.dat" >"$bad/rejected.log" 2>&1; then
            printf 'Original REAL NaN/Inf source corruption escaped: %s/%s\n' \
                "$variant" "$poison" >&2
            exit 1
        fi
        printf 'Original REAL nonfinite %s actual C simulation corruption rejection PASS %s\n' \
            "$variant" "$poison"
    done
    printf 'AUTO PHYSICAL PASS original-real-nonfinite-%s (true original C NaN/Inf/overflow event data and independent finite electrical oracle)\n' "$variant"
done
echo 'Original 1991 XSPICE REAL UDN special Inf/NaN event classification, source order, downstream original real gain propagation and finite physical probes PASS'
