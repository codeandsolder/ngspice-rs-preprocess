#!/usr/bin/env bash
# Original 1991 XSPICE REAL UDN IEEE-754 cancellation: unlike the previous
# exact-fraction sum experiment, the original C resolver is observably
# order-dependent for +1e16, +1, -1e16. Both physical original simulator
# routes and their 1k/2p loaded outputs are independently verified.
set -euo pipefail
here="$(cd -- "$(dirname -- "$0")" && pwd)"
# shellcheck source=runtime/xspice/prepare-original-smoke.sh
source "$here/prepare-original-smoke.sh"
prepare_original_smoke "real_cancellation" "$@"
config="$fixtures/auto-discovery-original-real-sum-triple-x.json"
for order in left grouped; do
    dir="$work/$order"
    mkdir -p "$dir/stock" "$dir/native"
    cp -- "$fixtures/original-real-sum-cancellation-$order-reference.cir" "$dir/stock/original.cir"
    cp -- "$fixtures/logical-original-real-sum-cancellation-$order-x.cir" "$dir/native/original.cir"
    cp -- "$fixtures/native-original-real-sum-cancellation-$order.lib" "$dir/native/"
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
    library="native-original-real-sum-cancellation-$order.lib"
    "$bins/alb-boundary-discover" "$dir/native/original.cir" "$config" \
        "$dir/native/discovered.json"
    "$bins/alb-logical-expand" "$dir/native/original.cir" \
        "$dir/native/discovered.json" \
        "$dir/native/lowered.cir" "$dir/native/generated.inc"
    "$bins/alb-family-autowire" \
        "$dir/native/lowered.cir" "$dir/native/generated.inc" \
        "$dir/native/discovered.json" "$fixtures/auto-models.json" \
        "$dir/native/circuit.cir"
    rg -F -x -q \
        'Xexternal LEN LEN_SR_SINK LEN_SR_SOURCE ANALOG_SUM ANALOG_POST ANALOG_ENABLED ANALOG_PROBE SR_ORIGINAL_REAL_SUM' \
        "$dir/native/circuit.cir" || {
        echo "Original cancellation $order source/native X ABI changed" >&2
        exit 1
    }
    [[ $(rg -c '^(ArealA Q0 NULL REAL_SUM|AgainB BPRE REAL_SUM|ArealC Q2 NULL REAL_SUM)' "$dir/native/$library") == 3 ]] || {
        echo "Missing one of three real original output drivers: $order" >&2
        exit 1
    }
    for route in stock native; do
        (
            cd "$dir/$route"
            deck=circuit.cir
            if [[ $route == stock ]]; then deck=original.cir; fi
            ngspice -b "$deck" >run.log 2>&1 || { tail -95 run.log >&2; exit 1; }
            if rg -i 'error:|fatal error|timestep too small|no such model' run.log; then
                printf 'Original real cancellation ngspice %s/%s failed\n' "$order" "$route" >&2
                exit 1
            fi
            test -s original-events.txt
            test -s result.dat
        )
    done
    oracle() {
        awk -v ORDER="$order" -f "$here/family-dc-equilibrium.awk" -f "$here/verify-original-real-cancellation.awk" \
            "$dir/stock/original-events.txt" "$1" \
            "$dir/stock/result.dat" "$2"
    }
    oracle "$dir/native/original-events.txt" "$dir/native/result.dat"

    for poison in change-a change-b-runtime change-b-initial change-c \
                  reverse-real-driver-order change-post-gain \
                  remove-enable-receiver swap-loaded-analog-outputs; do
        bad="$dir/mutation-$poison"
        mkdir -p "$bad"
        cp -- "$dir/native/circuit.cir" "$dir/native/generated.inc" \
              "$dir/native/$library" "$dir/native/.spiceinit" \
              "$dir/native/original-real-sum-driver-"*.txt "$bad/"
        case "$poison" in
            change-a)
                sed -i 's/zero=1e16 one=1e16/zero=9999999999999996 one=9999999999999996/' \
                    "$bad/$library" ;;
            change-b-runtime)
                sed -i 's/gain=0 out_offset=1 delay=240p/gain=0 out_offset=2 delay=240p/' \
                    "$bad/$library" ;;
            change-b-initial)
                sed -i 's/delay=240p ic=1)/delay=240p ic=2)/' "$bad/$library" ;;
            change-c)
                sed -i 's/zero=-1e16 one=-1e16/zero=-9999999999999998 one=-9999999999999998/' \
                    "$bad/$library" ;;
            reverse-real-driver-order)
                opposite=left
                if [[ "$order" == left ]]; then opposite=grouped; fi
                cp -- "$fixtures/native-original-real-sum-cancellation-$opposite.lib" \
                    "$bad/$library" ;;
            change-post-gain)
                sed -i 's/gain=-0.5 out_offset/gain=-0.75 out_offset/' \
                    "$bad/$library" ;;
            remove-enable-receiver)
                sed -i 's/capacitance=2p/capacitance=0/' "$bad/$library" ;;
            swap-loaded-analog-outputs)
                sed -i \
                    -e 's/Areadout REAL_SUM ANALOG_SUM/Areadout REAL_SUM ANALOG_POST/' \
                    -e 's/Apost RPOST ANALOG_POST/Apost RPOST ANALOG_SUM/' \
                    "$bad/$library" ;;
        esac
        (
            cd "$bad"
            ngspice -b circuit.cir >run.log 2>&1 || { tail -95 run.log >&2; exit 1; }
            if rg -i 'error:|fatal error|timestep too small|no such model' run.log; then
                printf 'Original cancellation C-model corruption did not simulate: %s/%s\n' \
                    "$order" "$poison" >&2
                exit 1
            fi
            test -s original-events.txt
            test -s result.dat
        )
        if oracle "$bad/original-events.txt" "$bad/result.dat" >"$bad/rejected.log" 2>&1; then
            printf 'Original cancellation actually simulated corruption escaped: %s/%s\n' \
                "$order" "$poison" >&2
            exit 1
        fi
        printf 'Original real UDN IEEE-754 C-model simulated mutation rejection PASS %s/%s\n' "$order" "$poison"
    done
    printf 'AUTO PHYSICAL PASS original-real-cancellation-%s (original sequential binary64 UDN sum plus physical outputs)\n' "$order"
done
# Strength, input source, original C models and parameters are identical.
# Only driver A-instance order changes. It *must* alter the original real
# resolved numeric event and the downstream physical voltage.
for route in stock native; do
    cmp -s "$work/left/$route/original-events.txt" \
           "$work/grouped/$route/original-events.txt" && {
        echo 'Original REAL UDN nonassociative cancellation lost its source-order dependence' >&2
        exit 1
    }
done
echo 'Original REAL UDN +1e16,+1,-1e16 versus +1e16,-1e16,+1 source-order nonassociativity PASS (two original real events 0 vs 1, both simulators)'
