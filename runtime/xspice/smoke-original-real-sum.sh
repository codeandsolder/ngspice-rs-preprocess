#!/usr/bin/env bash
# Source-verified original XSPICE real UDN multi-driver sum.
set -euo pipefail
here="$(cd -- "$(dirname -- "$0")" && pwd)"
# shellcheck source=runtime/xspice/prepare-original-smoke.sh
source "$here/prepare-original-smoke.sh"
# Deliberately misleading scratch variant for independently typed oracles.
prepare_original_smoke "real_udn" "$@"
for kind in pair triple triple-reordered; do
    dir="$work/$kind"
    mkdir -p "$dir/stock" "$dir/native"
    cp -- "$fixtures/original-real-sum-$kind-reference.cir" "$dir/stock/original.cir"
    cp -- "$fixtures/logical-original-real-sum-$kind-x.cir" "$dir/native/original.cir"
    cp -- "$fixtures/native-original-real-sum-$kind.lib" "$dir/native/"
    for id in 0 1 2; do
        cp -- "$fixtures/original-real-sum-driver-$id.txt" "$dir/stock/"
        cp -- "$fixtures/original-real-sum-driver-$id.txt" "$dir/native/"
    done
    cp -- "$fixtures/georgiatech-d_source-original.ifs" "$dir/native/"
    for model in d_to_real real_gain real_to_v; do
        cp -- "$fixtures/original-xtraevt-$model.ifs" "$dir/native/"
    done
    printf 'codemodel %s\n' "$cm" >"$dir/stock/.spiceinit"
    cp -- "$dir/stock/.spiceinit" "$dir/native/"
    config="$fixtures/auto-discovery-original-real-sum-$kind-x.json"
    library="native-original-real-sum-$kind.lib"

    for poison in missing-source scalar-source wrong-source-kind \
                  wrong-gain-kind duplicate-gain-parameter \
                  wrong-digital-input null-real-output; do
        invalid="$dir/invalid-$poison"
        mkdir -p "$invalid"
        cp -- "$dir/native/original.cir" "$dir/native/$library" \
              "$dir/native/georgiatech-d_source-original.ifs" \
              "$dir/native/original-xtraevt-"*.ifs "$invalid/"
        bad_config="$config"
        case "$poison" in
            missing-source)
                sed -i 's/Asource0 \[Q0\] ORIGINAL_SOURCE0/Awrong [Q0] ORIGINAL_SOURCE0/' "$invalid/$library" ;;
            scalar-source)
                sed -i 's/Asource0 \[Q0\] ORIGINAL_SOURCE0/Asource0 Q0 ORIGINAL_SOURCE0/' "$invalid/$library" ;;
            wrong-source-kind)
                sed -i 's/ORIGINAL_SOURCE0 d_source(/ORIGINAL_SOURCE0 d_ram(/' "$invalid/$library" ;;
            wrong-gain-kind)
                sed -i 's/ORIGINAL_GAIN_B real_gain(/ORIGINAL_GAIN_B real_to_v(/' "$invalid/$library" ;;
            duplicate-gain-parameter)
                sed -i 's/gain=-1.5 out_offset/gain=-1.5 gain=-1 out_offset/' "$invalid/$library" ;;
            wrong-digital-input)
                sed -i 's/ArealA Q0 NULL REAL_SUM/ArealA BPRE NULL REAL_SUM/' "$invalid/$library"
                jq '(.code_model_contracts[] | select(.instance=="ArealA") | .ports.in)=["BPRE"]' \
                    "$config" >"$invalid/poisoned.json"
                bad_config="$invalid/poisoned.json" ;;
            null-real-output)
                sed -i 's/ArealA Q0 NULL REAL_SUM/ArealA Q0 NULL NULL/' "$invalid/$library"
                jq '(.code_model_contracts[] | select(.instance=="ArealA") | .ports.out)=["NULL"]' \
                    "$config" >"$invalid/poisoned.json"
                bad_config="$invalid/poisoned.json" ;;
        esac
        if "$bins/alb-boundary-discover" "$invalid/original.cir" "$bad_config" \
            "$invalid/discovered.json" >"$invalid/rejected.log" 2>&1; then
            printf 'Invalid original REAL UDN contract escaped: %s %s\n' "$kind" "$poison" >&2
            exit 1
        fi
        rg -i -q 'IFS|port|binding|model|parameter|shared|NULL' "$invalid/rejected.log" || {
            tail -75 "$invalid/rejected.log" >&2; exit 1;
        }
        test ! -e "$invalid/discovered.json"
        printf 'Original REAL UDN %s IFS rejection PASS %s\n' "$kind" "$poison"
    done

    "$bins/alb-boundary-discover" "$dir/native/original.cir" "$config" "$dir/native/discovered.json"
    "$bins/alb-logical-expand" "$dir/native/original.cir" "$dir/native/discovered.json" \
        "$dir/native/lowered.cir" "$dir/native/generated.inc"
    "$bins/alb-family-autowire" "$dir/native/lowered.cir" "$dir/native/generated.inc" \
        "$dir/native/discovered.json" "$fixtures/auto-models.json" "$dir/native/circuit.cir"
    rg -F -x -q \
      'Xexternal LEN LEN_SR_SINK LEN_SR_SOURCE ANALOG_SUM ANALOG_POST ANALOG_ENABLED ANALOG_PROBE SR_ORIGINAL_REAL_SUM' \
      "$dir/native/circuit.cir" || { echo 'Original REAL physical subckt ABI changed' >&2; exit 1; }
    [[ $(rg -F -c 'sr_family_adc(' "$dir/native/circuit.cir") == 1 ]] || {
        echo 'Expected one original source compiled physical ADC' >&2; exit 1;
    }
    expected_drivers=2
    if [[ "$kind" != pair ]]; then expected_drivers=3; fi
    [[ $(rg -c '^(ArealA Q0 NULL REAL_SUM|AgainB BPRE REAL_SUM|ArealC Q2 NULL REAL_SUM)' "$dir/native/$library") == "$expected_drivers" ]] || {
        echo "Original $kind real UDN expected $expected_drivers independent drivers" >&2; exit 1;
    }

    for route in stock native; do
        (
            cd "$dir/$route"
            deck=circuit.cir
            if [[ "$route" == stock ]]; then deck=original.cir; fi
            ngspice -b "$deck" >run.log 2>&1 || {
                tail -95 run.log >&2
                exit 1
            }
            if rg -i 'error:|fatal error|timestep too small|no such model' run.log; then
                printf 'Original REAL %s simulation failed %s\n' "$kind" "$route" >&2
                exit 1
            fi
            test -s result.dat && test -s original-events.txt
        )
    done
    check_events() {
        awk -f "$here/verify-original-real-sum-events.awk" "$kind" \
            "$dir/stock/original-events.txt" "$1"
    }
    check_physical() {
        awk -f "$here/family-dc-equilibrium.awk" -f "$here/compare-original-real-sum-physical.awk" "$kind" \
            "$dir/stock/result.dat" "$1"
    }
    check_events "$dir/native/original-events.txt"
    check_physical "$dir/native/result.dat"

    mutations="wrong-source-a wrong-source-b wrong-real-a wrong-real-b wrong-b-gain wrong-post-gain wrong-a-delay wrong-b-delay bypassed-enable missing-enable-receiver swapped-physical-outputs orphan-a-driver orphan-b-driver"
    if [[ "$kind" != pair ]]; then
        mutations="$mutations wrong-source-c wrong-c-delay orphan-c-driver"
    fi
    for poison in $mutations; do
        invalid="$dir/corrupt-$poison"
        mkdir -p "$invalid"
        cp -- "$dir/native/circuit.cir" "$dir/native/generated.inc" \
              "$dir/native/$library" "$dir/native/.spiceinit" \
              "$dir/native/original-real-sum-driver-"*.txt "$invalid/"
        case "$poison" in
            wrong-source-a)
                sed -i 's/0s/1s/g' "$invalid/original-real-sum-driver-0.txt" ;;
            wrong-source-b)
                sed -i 's/1r/0r/g' "$invalid/original-real-sum-driver-1.txt" ;;
            wrong-source-c)
                sed -i 's/0r/1r/g' "$invalid/original-real-sum-driver-2.txt" ;;
            wrong-real-a)
                sed -i 's/zero=-0.5 one=1.5/zero=-0.25 one=1.5/' "$invalid/$library" ;;
            wrong-real-b)
                sed -i 's/zero=1.25 one=-0.75/zero=1.25 one=-0.5/' "$invalid/$library" ;;
            wrong-b-gain)
                sed -i 's/gain=-1.5 out_offset/gain=-1.25 out_offset/' "$invalid/$library" ;;
            wrong-post-gain)
                sed -i 's/gain=-0.5 out_offset/gain=-0.75 out_offset/' "$invalid/$library" ;;
            wrong-a-delay)
                sed -i 's/one=1.5 delay=200p/one=1.5 delay=250p/' "$invalid/$library" ;;
            wrong-b-delay)
                sed -i 's/delay=240p ic=0.375/delay=400p ic=0.375/' "$invalid/$library" ;;
            wrong-c-delay)
                sed -i 's/one=1.875 delay=700p/one=1.875 delay=900p/' "$invalid/$library" ;;
            bypassed-enable)
                sed -i 's/Ad2enable Q0 EN R_ENABLED/Ad2enable Q0 NULL R_ENABLED/' "$invalid/$library" ;;
            missing-enable-receiver)
                sed -i 's/capacitance=2p/capacitance=0/' "$invalid/$library" ;;
            swapped-physical-outputs)
                sed -i \
                    -e 's/Areadout REAL_SUM ANALOG_SUM/Areadout REAL_SUM ANALOG_POST/' \
                    -e 's/Apost RPOST ANALOG_POST/Apost RPOST ANALOG_SUM/' "$invalid/$library" ;;
            orphan-a-driver)
                sed -i 's/ArealA Q0 NULL REAL_SUM/ArealA Q0 NULL ORPHAN_A/' "$invalid/$library" ;;
            orphan-b-driver)
                sed -i 's/AgainB BPRE REAL_SUM/AgainB BPRE ORPHAN_B/' "$invalid/$library" ;;
            orphan-c-driver)
                sed -i 's/ArealC Q2 NULL REAL_SUM/ArealC Q2 NULL ORPHAN_C/' "$invalid/$library" ;;
        esac
        (
            cd "$invalid"
            ngspice -b circuit.cir >run.log 2>&1 || {
                tail -95 run.log >&2
                exit 1
            }
            if rg -i 'error:|fatal error|timestep too small|no such model' run.log; then
                printf 'Original REAL C mutant failed simulator: %s %s\n' "$kind" "$poison" >&2
                exit 1
            fi
            test -s result.dat && test -s original-events.txt
        )
        if check_events "$invalid/original-events.txt" >"$invalid/event-rejected.log" 2>&1 &&
            check_physical "$invalid/result.dat" >"$invalid/physical-rejected.log" 2>&1; then
            printf 'Original REAL UDN C-model mutation escaped: %s %s\n' "$kind" "$poison" >&2
            exit 1
        fi
        printf 'Original REAL UDN %s original C-model physical corruption PASS %s\n' "$kind" "$poison"
    done
    printf 'AUTO PHYSICAL PASS original-real-sum-%s (original 1991 real UDN summation and exact true event/physical oracle)\n' "$kind"
done
for route in stock native; do
    cmp -s "$work/triple/$route/original-events.txt" \
           "$work/triple-reordered/$route/original-events.txt" || {
        echo 'Original REAL UDN sums differ after permuting binary-exact model drivers' >&2
        exit 1
    }
done
echo 'Original REAL UDN source-order invariance PASS: original and compiled true real event traces identical after driver reordering'
