#!/usr/bin/env bash
# Original 1991 Georgia Tech XSPICE idn_digital_resolve:
# * all 144 pair resolutions using unmodified d_source C-model drivers;
# * all 48 original nonassociative triple inputs, with their actual
#   declaration order reversed in a THIRD independent physical circuit.
# Every original digital event is inspected with ngspice47 eprint, and
# analog state, electrical loading, bias current and 2pF input loading
# are checked against independently authored state+strength truth tables.
set -euo pipefail
here="$(cd -- "$(dirname -- "$0")" && pwd)"
# shellcheck source=runtime/xspice/prepare-original-smoke.sh
source "$here/prepare-original-smoke.sh"
# Deliberately misleading scratch variant for independently typed oracles.
prepare_original_smoke "pair_reordered" "$@"
for kind in pair triple triple-reordered; do
    stem="$kind"
    if [[ $kind == triple-reordered ]]; then stem=triple; fi
    case_dir="$work/$kind"
    mkdir -p "$case_dir/stock" "$case_dir/native"
    cp -- "$fixtures/original-contention-$kind-reference.cir" "$case_dir/stock/original.cir"
    cp -- "$fixtures/logical-original-contention-$kind-x.cir" "$case_dir/native/original.cir"
    cp -- "$fixtures/native-original-contention-$kind.lib" "$case_dir/native/"
    cp -- "$fixtures/original-contention-$stem-expected.txt" "$case_dir/native/"
    for i in 0 1; do
        cp -- "$fixtures/original-contention-$stem-source-$i.txt" "$case_dir/native/"
        cp -- "$fixtures/original-contention-$stem-source-$i.txt" "$case_dir/stock/"
    done
    if [[ $stem == triple ]]; then
        cp -- "$fixtures/original-contention-triple-source-2.txt" "$case_dir/native/"
        cp -- "$fixtures/original-contention-triple-source-2.txt" "$case_dir/stock/"
    fi
    cp -- "$fixtures/georgiatech-d_source-original.ifs" "$case_dir/native/"
    for m in d_to_real real_gain real_to_v; do
        cp -- "$fixtures/original-xtraevt-$m.ifs" "$case_dir/native/"
    done
    printf 'codemodel %s\n' "$cm" >"$case_dir/stock/.spiceinit"
    cp -- "$case_dir/stock/.spiceinit" "$case_dir/native/"
    config="$fixtures/auto-discovery-original-contention-$kind-x.json"
    native_name="native-original-contention-$kind.lib"

    # Invalid independent JSON witnesses cannot mask broken source-model
    # IFS port kind, digital vector shape, nonnullable input or bindings.
    for poison in missing-source scalar-source wrong-source-kind \
                  unexpected-real-source null-original-digital \
                  duplicated-gain-parameter; do
        dir="$case_dir/invalid-$poison"
        mkdir -p "$dir"
        cp -- "$case_dir/native/original.cir" \
              "$case_dir/native/$native_name" \
              "$case_dir/native/georgiatech-d_source-original.ifs" \
              "$case_dir/native/original-xtraevt-"*.ifs "$dir/"
        invalid_config="$config"
        case "$poison" in
            missing-source)
                sed -i 's/Asource0 \[QEV\] ORIGINAL_SOURCE0/Awrong [QEV] ORIGINAL_SOURCE0/' \
                    "$dir/$native_name" ;;
            scalar-source)
                sed -i 's/Asource0 \[QEV\] ORIGINAL_SOURCE0/Asource0 QEV ORIGINAL_SOURCE0/' \
                    "$dir/$native_name" ;;
            wrong-source-kind)
                sed -i 's/ORIGINAL_SOURCE0 d_source(/ORIGINAL_SOURCE0 d_ram(/' \
                    "$dir/$native_name" ;;
            unexpected-real-source)
                sed -i 's/Ad2real QEV NULL RRAW/Ad2real RRAW NULL RRAW/' \
                    "$dir/$native_name"
                jq '(.code_model_contracts[] | select(.instance=="Ad2real") | .ports.in)=["RRAW"]' \
                    "$config" >"$dir/poisoned.json"
                invalid_config="$dir/poisoned.json" ;;
            null-original-digital)
                sed -i 's/Ad2real QEV NULL RRAW/Ad2real NULL NULL RRAW/' \
                    "$dir/$native_name"
                jq '(.code_model_contracts[] | select(.instance=="Ad2real") | .ports.in)=["NULL"]' \
                    "$config" >"$dir/poisoned.json"
                invalid_config="$dir/poisoned.json" ;;
            duplicated-gain-parameter)
                sed -i 's/gain=1.6 out_offset/gain=1.6 gain=2 out_offset/' \
                    "$dir/$native_name" ;;
        esac
        if "$bins/alb-boundary-discover" \
            "$dir/original.cir" "$invalid_config" "$dir/discovered.json" \
            >"$dir/rejected.log" 2>&1; then
            printf 'Invalid original %s multi-driver contract escaped: %s\n' "$kind" "$poison" >&2
            exit 1
        fi
        rg -i -q 'IFS|port|parameter|model|binding|shared' "$dir/rejected.log" || {
            tail -85 "$dir/rejected.log" >&2; exit 1;
        }
        test ! -e "$dir/discovered.json"
        printf 'Original %s multi-driver original-IFS rejection PASS %s\n' "$kind" "$poison"
    done

    "$bins/alb-boundary-discover" "$case_dir/native/original.cir" "$config" \
        "$case_dir/native/discovered.json"
    "$bins/alb-logical-expand" \
        "$case_dir/native/original.cir" "$case_dir/native/discovered.json" \
        "$case_dir/native/lowered.cir" "$case_dir/native/generated.inc"
    "$bins/alb-family-autowire" \
        "$case_dir/native/lowered.cir" "$case_dir/native/generated.inc" \
        "$case_dir/native/discovered.json" "$fixtures/auto-models.json" \
        "$case_dir/native/circuit.cir"
    rg -F -x -q \
        'Xexternal LEN LEN_SR_SINK LEN_SR_SOURCE ANALOG_RAW ANALOG_ENABLED ANALOG_GAIN ANALOG_PROBE SR_ORIGINAL_CONTENTION' \
        "$case_dir/native/circuit.cir" || {
        printf 'Original %s multi-driver physical ABI changed\n' "$kind" >&2; exit 1;
    }
    [[ $(rg -F -c 'sr_family_adc(' "$case_dir/native/circuit.cir") == 1 ]] || {
        printf 'Original %s compiled enable receiver count wrong\n' "$kind" >&2; exit 1;
    }
    source_count=2
    if [[ $stem == triple ]]; then source_count=3; fi
    [[ $(rg -c '^Asource[0-9] \[QEV\] ORIGINAL_SOURCE[0-9]$' "$case_dir/native/$native_name") == "$source_count" ]] || {
        printf 'Original %s independent d_source driver count changed\n' "$kind" >&2; exit 1;
    }

    for route in stock native; do
        (
            cd "$case_dir/$route"
            deck=circuit.cir
            if [[ $route == stock ]]; then deck=original.cir; fi
            ngspice -b "$deck" >run.log 2>&1 || { tail -105 run.log >&2; exit 1; }
            if rg -i 'error:|fatal error|timestep too small|no such model' run.log; then
                printf 'Original %s %s simulator failed\n' "$kind" "$route" >&2
                exit 1
            fi
            test -s result.dat
            test -s original-events.txt
        )
    done
    expected="$case_dir/native/original-contention-$stem-expected.txt"
    check_events() {
        awk -v variant="$kind" -f "$here/verify-original-contention-events.awk" \
            "$expected" "$case_dir/stock/original-events.txt" "$1"
    }
    check_physical() {
        awk -v variant="$kind" -f "$here/family-dc-equilibrium.awk" -f "$here/compare-original-contention-physical.awk" \
            "$expected" "$case_dir/stock/result.dat" "$1"
    }
    check_events "$case_dir/native/original-events.txt"
    check_physical "$case_dir/native/result.dat"

    # Every poison is actually simulated by the genuine ngspice47 C
    # models; even when analog adapter hides a digital strength change,
    # the original eprint oracle must still detect it.
    for poison in invalid-left-input invalid-right-input \
                  wrong-digital-to-real wrong-real-gain \
                  bypassed-enable missing-physical-receiver \
                  swapped-output-probes; do
        dir="$case_dir/poisoned-$poison"
        mkdir -p "$dir"
        cp -- "$case_dir/native/circuit.cir" \
              "$case_dir/native/generated.inc" \
              "$case_dir/native/$native_name" \
              "$case_dir/native/.spiceinit" "$dir/"
        for i in 0 1; do
            cp -- "$case_dir/native/original-contention-$stem-source-$i.txt" "$dir/"
        done
        if [[ $stem == triple ]]; then
            cp -- "$case_dir/native/original-contention-triple-source-2.txt" "$dir/"
        fi
        case "$poison" in
            invalid-left-input)
                sed -i -E 's/([01U])[srzu]$/Us/' \
                    "$dir/original-contention-$stem-source-0.txt" ;;
            invalid-right-input)
                sed -i -E 's/([01U])[srzu]$/Us/' \
                    "$dir/original-contention-$stem-source-1.txt" ;;
            wrong-digital-to-real)
                sed -i 's/zero=-0.5 one=2/zero=0 one=2/' \
                    "$dir/$native_name" ;;
            wrong-real-gain)
                sed -i 's/gain=1.6 out_offset/gain=1.3 out_offset/' \
                    "$dir/$native_name" ;;
            bypassed-enable)
                sed -i 's/Ad2enabled QEV EN RGATED/Ad2enabled QEV NULL RGATED/' \
                    "$dir/$native_name" ;;
            missing-physical-receiver)
                sed -i 's/capacitance=2p/capacitance=0/' \
                    "$dir/$native_name" ;;
            swapped-output-probes)
                sed -i -e 's/Araw RRAW ANALOG_RAW/Araw RRAW ANALOG_GAIN/' \
                    -e 's/AgainAnalog RGAIN ANALOG_GAIN/AgainAnalog RGAIN ANALOG_RAW/' \
                    "$dir/$native_name" ;;
        esac
        (
            cd "$dir"
            ngspice -b circuit.cir >run.log 2>&1 || { tail -110 run.log >&2; exit 1; }
            if rg -i 'error:|fatal error|timestep too small|no such model' run.log; then
                printf 'Original %s poison cannot simulate: %s\n' "$kind" "$poison" >&2
                exit 1
            fi
            test -s result.dat
            test -s original-events.txt
        )
        if check_events "$dir/original-events.txt" >"$dir/event-rejected.log" 2>&1 &&
            check_physical "$dir/result.dat" >"$dir/physical-rejected.log" 2>&1; then
            printf 'Original %s physically simulated multi-driver corruption escaped: %s\n' "$kind" "$poison" >&2
            exit 1
        fi
        printf 'Original %s actual C-model mutation rejection PASS %s\n' "$kind" "$poison"
    done
    if [[ $stem == triple ]]; then
        dir="$case_dir/poisoned-driver-order"
        mkdir -p "$dir"
        cp -- "$case_dir/native/circuit.cir" \
              "$case_dir/native/generated.inc" \
              "$case_dir/native/$native_name" \
              "$case_dir/native/original-contention-triple-source-"*.txt \
              "$case_dir/native/.spiceinit" "$dir/"
        # Reorder *only* original A-instance lines, leaving all other
        # models, parameter values and drive text files byte-for-byte.
        case_kind="$kind" native_lib="$dir/$native_name" \
            uv run --no-project --no-managed-python python - <<'PY'
import os
from pathlib import Path
p=Path(os.environ["native_lib"])
s=p.read_text()
a="\n".join([
 "Asource0 [QEV] ORIGINAL_SOURCE0",
 "Asource1 [QEV] ORIGINAL_SOURCE1",
 "Asource2 [QEV] ORIGINAL_SOURCE2",
])
b="\n".join([
 "Asource1 [QEV] ORIGINAL_SOURCE1",
 "Asource2 [QEV] ORIGINAL_SOURCE2",
 "Asource0 [QEV] ORIGINAL_SOURCE0",
])
old=b if os.environ["case_kind"]=="triple-reordered" else a
new=a if os.environ["case_kind"]=="triple-reordered" else b
assert s.count(old)==1
p.write_text(s.replace(old,new))
PY
        (
            cd "$dir"
            ngspice -b circuit.cir >run.log 2>&1 || { tail -100 run.log >&2; exit 1; }
            test -s original-events.txt
            test -s result.dat
        )
        if check_events "$dir/original-events.txt" >"$dir/event-rejected.log" 2>&1 &&
            check_physical "$dir/result.dat" >"$dir/physical-rejected.log" 2>&1; then
            echo "Original non-associative digital resolver order-mutant escaped" >&2
            exit 1
        fi
        echo "Original $kind actual C-model netlist source order corruption PASS"
    fi
    echo "AUTO PHYSICAL PASS original-contention-$kind (source-faithful 12-state multi-driver resolution with real event, physical KCL and order-dependent oracles)"
done
