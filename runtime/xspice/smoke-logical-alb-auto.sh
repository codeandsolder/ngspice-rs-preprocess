#!/usr/bin/env bash
# Automated original-source ALB-to-physical ngspice route. No manual
# analog/digital boundary nodes, directions or families in any input mapping.
set -euo pipefail
if (( $# != 3 )); then
    printf 'usage: %s CODEMODEL_CM RUST_BIN_DIR SCRATCH_ROOT\n' "$0" >&2
    exit 2
fi
here="$(cd -- "$(dirname -- "$0")" && pwd)"
cm="$(realpath -- "$1")"
bins="$(realpath -- "$2")"
scratch="$(realpath -- "$3")"
for cmd in ngspice rg awk; do command -v "$cmd" >/dev/null; done
for executable in alb-boundary-discover alb-logical-expand alb-family-autowire; do
    test -x "$bins/$executable" || { echo "Missing compiler $executable" >&2; exit 1; }
done
work="$(mktemp -d "$scratch/ngspice_preprocess_auto_XXXXXXXX")"
trap 'rm -rf -- "$work"' EXIT
printf 'codemodel %s\n' "$cm" >"$work/.spiceinit"
# Verify exact original subcircuit ABI and counted physical ADC/DAC bridges.
# Fixed-line matching is important: in a regex, von=2.4 would also match
# an invalid von=2x4 parameter.
verify_ti_native_contract() {
    if (( $# < 5 )); then
        echo 'verify_ti_native_contract requires name, directory, ADC count, DAC count and instances' >&2
        return 2
    fi
    local variant="$1" workdir="$2" expected_adc="$3" expected_dac="$4"
    shift 4
    local instance bridge expected actual
    for instance in "$@"; do
        rg -F -x -q -- "$instance" "$workdir/circuit.cir" || {
            printf 'Original %s native instance ABI or parameter string changed: %s\n' \
                "$variant" "$instance" >&2
            return 1
        }
    done
    for bridge in adc dac; do
        expected="$expected_adc"
        [[ $bridge == dac ]] && expected="$expected_dac"
        actual="$(rg -F -c -- "sr_family_$bridge(" "$workdir/circuit.cir" || true)"
        [[ $actual == "$expected" ]] || {
            printf 'Original %s native %s count changed: wanted %s, observed %s\n' \
                "$variant" "$bridge" "$expected" "$actual" >&2
            return 1
        }
    done
}

# Verify original vendor analog models against two independent measured
# waveforms and a negative reference/parameter-pair corruption.
verify_ti_analog_pair() {
    local variant="$1" workdir="$2"
    local first_instance second_instance first_reference second_reference
    local abs_tol rms_tol poison_log
    case "$variant" in
        vendor-ti-inverter-x)
            first_instance='Xvendor_fast VENDOR_OUT ANALOG_INPUT SR_TI_INVERTER PARAMS: td=0 tt=2n'
            second_instance='Xvendor_slow VENDOR_SLOW ANALOG_INPUT SR_TI_INVERTER PARAMS: td=1n tt=4n'
            first_reference=ti-tps22986-inverter-default-reference.dat
            second_reference=ti-tps22986-inverter-slow-reference.dat
            abs_tol=.02 rms_tol=.005
            poison_log=incorrect-vendor-comparison.log
            ;;
        vendor-ti-comparator-x)
            first_instance='Xvendor_base COMPARATOR_BASE ANALOG_INPUT COMPARATOR_REFERENCE SR_TI_COMP_HYS PARAMS: vhys=.05 td=0 tt=2n'
            second_instance='Xvendor_wide COMPARATOR_WIDE ANALOG_INPUT COMPARATOR_REFERENCE SR_TI_COMP_HYS PARAMS: vhys=.12 td=1n tt=4n'
            first_reference=ti-comparator-base-wide-reference.dat
            second_reference=ti-comparator-slow-wide-reference.dat
            abs_tol=.015 rms_tol=.002
            poison_log=incorrect-hysteresis-comparison.log
            ;;
        *)
            printf 'Unknown TI analog-pair model: %s\n' "$variant" >&2
            return 1
            ;;
    esac
    verify_ti_native_contract "$variant" "$workdir" 1 1 \
        "$first_instance" "$second_instance"
    local references="$here/fixtures/vendor-external"
    awk -v native_column=2 -v abs_tol="$abs_tol" -v rms_tol="$rms_tol" \
        -f "$here/compare-vendor-waveform.awk" \
        "$references/$first_reference" "$workdir/result.dat" || {
            printf 'Original %s first vendor waveform differs\n' "$variant" >&2
            return 1
        }
    awk -v native_column=4 -v abs_tol="$abs_tol" -v rms_tol="$rms_tol" \
        -f "$here/compare-vendor-waveform.awk" \
        "$references/$second_reference" "$workdir/result.dat" || {
            printf 'Original %s second vendor waveform differs\n' "$variant" >&2
            return 1
        }
    # An accidental parameter drop or column mix must fail this real oracle.
    if awk -v native_column=4 -v abs_tol="$abs_tol" -v rms_tol="$rms_tol" \
        -f "$here/compare-vendor-waveform.awk" \
        "$references/$first_reference" "$workdir/result.dat" \
        >"$workdir/$poison_log" 2>&1; then
        printf 'Original %s accepted an incorrect vendor reference pair\n' "$variant" >&2
        return 1
    fi
}
# Reject a physically corrupted original external library at discovery time.
# Each caller still supplies its independently chosen pin-order mutation.
verify_rejected_native_formals() {
    local original="$1" electrical="$2" workdir="$3"
    local library="$4" rejected_name="$5" mutation="$6" diagnostic="$7"
    local rejected="$workdir/$rejected_name"
    local published="$here/fixtures/family/$library"
    mkdir -p -- "$rejected"
    cp -- "$original" "$rejected/original.cir"
    sed -e "$mutation" "$published" >"$rejected/$library"
    if cmp -s -- "$published" "$rejected/$library"; then
        printf 'Original native library poison was a no-op: %s\n' "$library" >&2
        return 1
    fi
    if "$bins/alb-boundary-discover" \
        "$rejected/original.cir" "$electrical" "$rejected/discovered.json" \
        >"$rejected/error.log" 2>&1; then
        printf '%s\n' "$diagnostic" >&2
        return 1
    fi
    rg -q 'in that exact order' "$rejected/error.log" || return 1
    if [[ -e "$rejected/discovered.json" ]]; then
        printf 'Rejected original native library left a boundary output: %s\n' "$library" >&2
        return 1
    fi
}
# Compare every independent electrical V/I column of a manufacturer
# waveform, retaining each model's own reference and tolerance set.
verify_ti_physical_columns() {
    local description="$1" reference="$2" trace="$3"
    shift 3
    local columns ref_col native_col value_unit max_error rms_error
    for columns in "$@"; do
        read -r ref_col native_col value_unit max_error rms_error <<<"$columns"
        awk -v reference_column="$ref_col" -v native_column="$native_col" \
            -v value_unit="$value_unit" -v abs_tol="$max_error" -v rms_tol="$rms_error" \
            -f "$here/compare-vendor-waveform.awk" "$reference" "$trace" || {
                printf 'Original TI %s physical differential failed for reference column %s\n' \
                    "$description" "$ref_col" >&2
                return 1
            }
    done
}
count=0
for case in original nested shared device-variants device-comb stateless-user inner-contact shared-producers observe-print observe-control bidirectional-port opaque-x native-digital-x native-digital-lib-x native-digital-history-x native-digital-mixed-x native-digital-hiz-x vendor-ti-inverter-x vendor-ti-comparator-x vendor-ti-nmos-x vendor-ti-gatedriver-x georgiatech-dlatch-x original-5v; do
    dir="$work/$case"
    mkdir -p "$dir"
    cp -- "$work/.spiceinit" "$dir/"
    variant="$case"
    supply=3v3
    if [[ $case == original-5v ]]; then
        variant=original
        supply=5v
    fi
    if [[ $case == bidirectional-port ]]; then
        source="$here/fixtures/family/bidirectional-port.cir"
    else
        source="$here/fixtures/family/logical-$variant.cir"
    fi
    electrical="$here/fixtures/family/auto-discovery-$supply.json"
    if [[ $case == georgiatech-dlatch-x ]]; then
        electrical="$here/fixtures/family/auto-discovery-georgiatech-dlatch-x.json"
        cp -- "$here/fixtures/family/native-georgiatech-dlatch.lib" "$dir/"
        cp -- "$here/fixtures/family/georgiatech-dlatch-original.ifs" "$dir/"
        mkdir -p "$dir/invalid-georgiatech-abi"
        cp -- "$source" "$dir/invalid-georgiatech-abi/original.cir"
        sed 's/DATA DATA_SINK DATA_SOURCE/DATA DATA_SOURCE DATA_SINK/' \
            "$here/fixtures/family/native-georgiatech-dlatch.lib" \
            >"$dir/invalid-georgiatech-abi/native-georgiatech-dlatch.lib"
        if "$bins/alb-boundary-discover" \
            "$dir/invalid-georgiatech-abi/original.cir" "$electrical" \
            "$dir/invalid-georgiatech-abi/discovered.json" \
            >"$dir/invalid-georgiatech-abi/error.log" 2>&1; then
            echo 'Wrong event load/source order of original Georgia Tech XSPICE latch wrapper accepted' >&2
            exit 1
        fi
        rg -q 'in that exact order' "$dir/invalid-georgiatech-abi/error.log"

        # The original IFS is authoritative even if the native outer
        # .subckt header remains perfectly correct. Verify the raw
        # manufacturer IFS, actual inner A-instance and .model kind at
        # preflight; never silently skip an original vector/port contract.
        for sabotage in swap-original-A-ports swap-ifspec-port-columns wrong-original-code-kind missing-original-ifspec; do
            invalid="$dir/ifspec-rejected-$sabotage"
            mkdir -p "$invalid"
            cp -- "$source" "$invalid/original.cir"
            cp -- "$dir/native-georgiatech-dlatch.lib" "$invalid/"
            cp -- "$dir/georgiatech-dlatch-original.ifs" "$invalid/"
            case "$sabotage" in
                swap-original-A-ports)
                    sed -i 's/Alatch DATA EN SET RESET Q NQ/Alatch EN DATA SET RESET Q NQ/' \
                        "$invalid/native-georgiatech-dlatch.lib"
                    ;;
                swap-ifspec-port-columns)
                    sed -i 's/Port_Name: *data *enable/Port_Name: enable data/' \
                        "$invalid/georgiatech-dlatch-original.ifs"
                    ;;
                wrong-original-code-kind)
                    sed -i 's/\.model GT_ORIGINAL_LATCH d_dlatch(/.model GT_ORIGINAL_LATCH d_ram(/' \
                        "$invalid/native-georgiatech-dlatch.lib"
                    ;;
                missing-original-ifspec)
                    rm -- "$invalid/georgiatech-dlatch-original.ifs"
                    ;;
            esac
            if "$bins/alb-boundary-discover" \
                "$invalid/original.cir" "$electrical" "$invalid/invalid-boundaries.json" \
                >"$invalid/preflight-error.log" 2>&1; then
                echo "Original IFS semantic poison incorrectly accepted before discovery: $sabotage" >&2
                exit 1
            fi
            rg -i -q 'IFS|code.model' "$invalid/preflight-error.log" || \
                { cat "$invalid/preflight-error.log" >&2; exit 1; }
            test ! -e "$invalid/invalid-boundaries.json" || \
                { echo "Original IFS failure left a boundary output: $sabotage" >&2; exit 1; }
        done
    fi
    if [[ $case == vendor-ti-gatedriver-x ]]; then
        electrical="$here/fixtures/family/auto-discovery-vendor-ti-gatedriver-x.json"
        cp -- "$here/fixtures/family/native-ti-gatedriver.lib" "$dir/"
        verify_rejected_native_formals "$source" "$electrical" "$dir" \
            "native-ti-gatedriver.lib" "invalid-gatedriver-library" \
            's/SR_TI_GATE_DRIVER_EVENT OUT CONTROL CONTROL_SINK CONTROL_SOURCE/SR_TI_GATE_DRIVER_EVENT OUT CONTROL CONTROL_SOURCE CONTROL_SINK/' \
            'Wrong order of real external native event/load formals was accepted'
    fi
    if [[ $case == vendor-ti-nmos-x ]]; then
        electrical="$here/fixtures/family/auto-discovery-vendor-ti-nmos-x.json"
        cp -- "$here/fixtures/family/native-ti-nmos.lib" "$dir/"
        verify_rejected_native_formals "$source" "$electrical" "$dir" \
            "native-ti-nmos.lib" "invalid-nmos-library" \
            's/SR_TI_NMOS D G S/SR_TI_NMOS D S G/' \
            'Wrong order of actual TI NMOS D/G/S pins must fail'
    fi
    if [[ $case == vendor-ti-comparator-x ]]; then
        electrical="$here/fixtures/family/auto-discovery-vendor-ti-comparator-x.json"
        cp -- "$here/fixtures/family/native-ti-comparator.lib" "$dir/"
        verify_rejected_native_formals "$source" "$electrical" "$dir" \
            "native-ti-comparator.lib" "invalid-comparator-library" \
            's/SR_TI_COMP_HYS OUT INP INN/SR_TI_COMP_HYS OUT INN INP/' \
            'Swapped real TI comparator analog X formals incorrectly accepted'
    fi
    if [[ $case == vendor-ti-inverter-x ]]; then
        electrical="$here/fixtures/family/auto-discovery-vendor-ti-inverter-x.json"
        cp -- "$here/fixtures/family/native-ti-inverter.lib" "$dir/"
        verify_rejected_native_formals "$source" "$electrical" "$dir" \
            "native-ti-inverter.lib" "invalid-analog-library" \
            's/SR_TI_INVERTER OUT IN/SR_TI_INVERTER IN OUT/' \
            'Wrong order of actual external analog X formals must fail'
    fi
    if [[ $case == native-digital-hiz-x ]]; then
        electrical="$here/fixtures/family/auto-discovery-native-hiz-x.json"
        cp -- "$here/fixtures/family/native-external-tristate.lib" "$dir/"
    fi
    if [[ $case == native-digital-mixed-x ]]; then
        electrical="$here/fixtures/family/auto-discovery-native-mixed-x.json"
        cp -- "$here/fixtures/family/native-external-mixed-buffer.lib" "$dir/"
    fi
    if [[ $case == native-digital-history-x ]]; then
        electrical="$here/fixtures/family/auto-discovery-native-digital-x.json"
        cp -- "$here/fixtures/family/native-external-buffer.lib" "$dir/"
    fi
    if [[ $case == native-digital-lib-x ]]; then
        electrical="$here/fixtures/family/auto-discovery-native-digital-x.json"
        cp -- "$here/fixtures/family/native-external-buffer-sections.lib" "$dir/"
        sed 's/ VALID$/ INVALID/' "$source" > "$dir/wrong-section.cir"
        if "$bins/alb-boundary-discover" \
            "$dir/wrong-section.cir" "$electrical" \
            "$dir/wrong-section.json" > "$dir/wrong-section.log" 2>&1; then
            echo 'Inactive correct native .lib section must not validate wrong selection' >&2
            exit 1
        fi
        rg -q 'in that exact order' "$dir/wrong-section.log"
        sed 's/ VALID$/ ABSENT/' "$source" > "$dir/absent-section.cir"
        if "$bins/alb-boundary-discover" \
            "$dir/absent-section.cir" "$electrical" \
            "$dir/absent-section.json" > "$dir/absent-section.log" 2>&1; then
            echo 'Missing native .lib section must fail before emitting output' >&2
            exit 1
        fi
        rg -q '\.lib section ABSENT not found' "$dir/absent-section.log"
    fi
    if [[ $case == native-digital-x ]]; then
        electrical="$here/fixtures/family/auto-discovery-native-digital-x.json"
        cp -- "$here/fixtures/family/native-external-buffer.lib" "$dir/"
        # The real library, not the invented topology stub, must match the
        # verified native event-state/load/source port order.
        mkdir -p "$dir/invalid-library"
        cp -- "$source" "$dir/invalid-library/source.cir"
        sed 's/D D_SINK D_SOURCE/D D_SOURCE D_SINK/' \
            "$here/fixtures/family/native-external-buffer.lib" \
            >"$dir/invalid-library/native-external-buffer.lib"
        if "$bins/alb-boundary-discover" \
            "$dir/invalid-library/source.cir" "$electrical" \
            "$dir/invalid-library/discovered.json" \
            >"$dir/invalid-library/error.log" 2>&1; then
            echo 'Swapped external native event formals must fail before wiring' >&2
            exit 1
        fi
        rg -q 'in that exact order' "$dir/invalid-library/error.log"
        rm -- "$dir/invalid-library/native-external-buffer.lib"
        if "$bins/alb-boundary-discover" \
            "$dir/invalid-library/source.cir" "$electrical" \
            "$dir/invalid-library/discovered.json" \
            >"$dir/invalid-library/error.log" 2>&1; then
            echo 'Missing actual native external .subckt must fail before wiring' >&2
            exit 1
        fi
        rg -q 'cannot resolve original external X include' "$dir/invalid-library/error.log"
    fi
    if [[ $case == opaque-x ]]; then
        # The external library's verified model-level *pin signature*
        # identifies analog contacts without handwritten per-net cuts.
        electrical="$here/fixtures/family/auto-discovery-opaque-x.json"
        cp -- "$here/fixtures/family/opaque-analog-link.lib" "$dir/"
        if "$bins/alb-boundary-discover" \
            "$source" "$here/fixtures/family/auto-discovery-3v3.json" \
            "$dir/unverified.json" >"$dir/unverified.log" 2>&1; then
            echo 'Undeclared external X should not be guessed analog' >&2
            exit 1
        fi
    fi
    "$bins/alb-boundary-discover" \
        "$source" "$electrical" \
        "$dir/discovered.json"
    "$bins/alb-logical-expand" \
        "$source" "$dir/discovered.json" \
        "$dir/lowered.cir" "$dir/generated.inc"
    "$bins/alb-family-autowire" \
        "$dir/lowered.cir" "$dir/generated.inc" \
        "$dir/discovered.json" \
        "$here/fixtures/family/auto-models.json" "$dir/circuit.cir"
    (
        cd "$dir"
        if ! ngspice -b circuit.cir >run.log 2>&1; then
            printf 'Native auto boundary smoke failed: %s\n' "$case" >&2
            tail -n 85 run.log >&2
            exit 1
        fi
        if rg -i 'error:|fatal error|timestep too small|no such model' run.log; then
            printf 'Native auto boundary smoke emitted errors: %s\n' "$case" >&2
            exit 1
        fi
        test -s result.dat
    )
    if [[ $case == original-5v ]]; then
        awk '$1>10e-9 && $1<11e-9 && $2>=-.01 && $2<.1 {found=1}
             END {exit !found}' "$dir/result.dat" ||
            { echo 'Automatic 5V family input threshold failed' >&2; exit 1; }
    elif [[ $case == device-variants || $case == device-comb ]]; then
        awk '$1>10e-9 && $1<11e-9 && $2>3.1 && $2<3.31 && $4>=-.01 && $4<.1 && $6>3.1 && $6<3.31 {found=1}
             END {exit !found}' "$dir/result.dat" ||
            { echo "Automatic physical DEVICE model routing failed: $case" >&2; exit 1; }
        [[ $(rg -c '^\.subckt SRALB' "$dir/generated.inc") == 2 ]] ||
            { echo "Original DEVICE variants were not deduplicated: $case" >&2; exit 1; }
    elif [[ $case != shared && $case != bidirectional-port && $case != native-digital-hiz-x && $case != vendor-ti-inverter-x && $case != vendor-ti-comparator-x && $case != vendor-ti-nmos-x && $case != vendor-ti-gatedriver-x && $case != georgiatech-dlatch-x ]]; then
        awk '$1>10e-9 && $1<11e-9 && $2>3.1 && $2<3.31 {found=1}
             END {exit !found}' "$dir/result.dat" ||
            { echo "Automatic physical ALB HIGH failed: $case" >&2; exit 1; }
    fi
    if [[ $case == shared ]]; then
        [[ $(rg -c 'sr_family_adc\(' "$dir/circuit.cir") == 1 ]] ||
            { echo 'Automatic source boundary duplicated one shared ADC' >&2; exit 1; }
        [[ $(rg -c '^XSR_A[12] INPUT INPUT_SR_SINK INPUT_SR_SOURCE ' "$dir/circuit.cir") == 2 ]] ||
            { echo 'Automatic shared input did not reuse one physical state triple' >&2; exit 1; }
    fi
    if [[ $case == bidirectional-port ]]; then
        [[ $(rg -F -c 'sr_family_adc(' "$dir/circuit.cir") == 2 ]] ||
            { echo 'Bidir pin requires independent physical PIN and OE ADCs' >&2; exit 1; }
        [[ $(rg -F -c 'sr_family_dac(' "$dir/circuit.cir") == 2 ]] ||
            { echo 'Bidir pin requires independent physical PIN and MONITOR DACs' >&2; exit 1; }
        awk '$1>26.5e-9 && $1<27e-9 && $2>3.13 && $2<3.23 && $4>3.1 && $6>3.2 && $8<.1 {ok=1}
             END {exit !ok}' "$dir/result.dat" ||
            { echo 'Automatic bidirectional ALB physical drive and sensing failed' >&2; exit 1; }
    fi
    if [[ $case == native-digital-x || $case == native-digital-lib-x || $case == native-digital-history-x ]]; then
        rg -q '^Xexternal FIRST FIRST_SR_SINK FIRST_SR_SOURCE SECOND SECOND_SR_SINK SECOND_SR_SOURCE SR_NATIVE_DIGITAL_BUFFER$' "$dir/circuit.cir" ||
            { echo 'External verified digital ABI did not receive both complete triples' >&2; exit 1; }
        [[ $(rg -F -c 'sr_family_adc(' "$dir/circuit.cir") == 1 ]] ||
            { echo 'Native external digital input duplicated physical ADC' >&2; exit 1; }
        expected_dacs=1
        if [[ $case == native-digital-history-x ]]; then
            expected_dacs=2 # the external X output and its separate 3-stage reference
        fi
        [[ $(rg -F -c 'sr_family_dac(' "$dir/circuit.cir") == "$expected_dacs" ]] ||
            { echo 'Native external digital output or reference has incorrect DAC count' >&2; exit 1; }
        if [[ $case != native-digital-history-x ]]; then
            awk '$1>1e-9 && $1<1.5e-9 && $2<.1 {initial_low=1}
                 $1>10e-9 && $1<11e-9 && $2>3.1 {pulse_high=1}
                 $1>14.5e-9 && $1<15e-9 && $2<.1 {released_low=1}
                 END {exit !(initial_low && pulse_high && released_low)}' "$dir/result.dat" ||
                { echo 'Native external digital ABI failed LOW/HIGH/LOW event propagation' >&2; exit 1; }
        fi
    fi
    if [[ $case == native-digital-history-x ]]; then
        # The threshold-band UNKNOWN must retain the preceding HIGH through
        # the external X event triple. At startup and after recovery it is LOW.
        # The same original ALB model traverses three direct native stages
        # as an independent wiring/physical-timing reference in this circuit.
        awk '
          $1>1e-9 && $1<1.5e-9 && $2<.1 && $4<.1 {start_low=1}
          $1>10e-9 && $1<11e-9 && $2>3.1 && $4>3.1 {first_high=1}
          $1>20e-9 && $1<21e-9 && $2>3.1 && $4>3.1 && $6>2.14 && $6<2.16 {retained=1}
          $1>28e-9 && $1<29e-9 && $2<.1 && $4<.1 {known_low=1}
          $1>37e-9 && $1<38e-9 && $2>3.1 && $4>3.1 {second_high=1}
          $1>45e-9 && $1<46e-9 && $2<.1 && $4<.1 {final_low=1}
          END {exit !(start_low && first_high && retained && known_low && second_high && final_low)}
        ' "$dir/result.dat" ||
            { echo 'Native external X lost vendor-established ALB UNKNOWN history' >&2; exit 1; }
        awk '
          {
            delta=$2-$4; if (delta<0) delta=-delta
            if (delta>worst) worst=delta
            samples++
          }
          END {
            printf "Native X vs direct ALB full-waveform: %d samples, max |dV|=%.6g V\n", samples, worst
            exit !(samples>100 && worst<=.02)
          }
        ' "$dir/result.dat" ||
            { echo 'External X voltage or timing diverged from direct 3-stage ALB' >&2; exit 1; }
        rg -q 'unknown_as_last=true' "$dir/generated.inc"
    fi
    if [[ $case == vendor-ti-inverter-x ]]; then
        verify_ti_analog_pair "$case" "$dir"
    fi
    if [[ $case == georgiatech-dlatch-x ]]; then
        # Direct Georgia Tech original 1991 public-domain d_dlatch event
        # model is the oracle, not an authored reimplementation or an
        # analog PSpice logic approximation. Both paths use the same
        # time-dependent input PWL and independently configured ADCs.
        expected_x='Xexternal DATA DATA_SR_SINK DATA_SR_SOURCE ENABLE ENABLE_SR_SINK ENABLE_SR_SOURCE SET SET_SR_SINK SET_SR_SOURCE RESET RESET_SR_SINK RESET_SR_SOURCE ANALOG_Q ANALOG_NQ SR_GT_DLATCH'
        rg -F -x -q "$expected_x" "$dir/circuit.cir" ||
            { echo 'Real Georgia Tech event latch external-X ordered ABI was corrupted' >&2; exit 1; }
        rg -F -x -q 'Alatch DATA EN SET RESET Q NQ GT_ORIGINAL_LATCH' "$dir/native-georgiatech-dlatch.lib" ||
            { echo 'Georgia Tech published d_dlatch event port order is not implemented' >&2; exit 1; }
        [[ $(rg -F -c 'sr_family_adc(' "$dir/circuit.cir") == 4 ]] ||
            { echo 'Four independent external XSPICE event inputs require four physical ADCs' >&2; exit 1; }
        [[ $(rg -F -c 'sr_bundle_load(capacitance=2p' "$dir/native-georgiatech-dlatch.lib") == 1 ]] ||
            { echo 'Original stateful latch lacks explicit native receiver load' >&2; exit 1; }
        mkdir -p "$dir/stock-original"
        cp -- "$here/fixtures/family/georgiatech-dlatch-original-reference.cir" \
            "$dir/stock-original/original.cir"
        (
            cd "$dir/stock-original"
            ngspice -b original.cir >run.log 2>&1 ||
                { tail -n 85 run.log >&2; exit 1; }
            if rg -i 'error:|fatal error|timestep too small|no such model' run.log; then
                echo 'Original stock Georgia Tech XSPICE reference simulator failed' >&2
                exit 1
            fi
            test -s result.dat
        )
        # All 18 state steps of Q+NQ (not just final values), including
        # asynchronous SET/RESET and both UNKNOWN outputs, must match.
        # The native 2p receiver on the existing 100-ohm family source
        # adds a directly measured ~200ps to each of the 16 transitions.
        stock="$dir/stock-original/result.dat"
        states_q='0,1,0,1,0,1,X,0,1'
        states_nq='1,0,1,0,1,0,X,1,0'
        awk -v expected_q="$states_q" -v expected_nq="$states_nq" \
            -v min_delta_ps=150 -v max_delta_ps=250 \
            -f "$here/compare-event-traces.awk" "$stock" "$dir/result.dat" ||
            { echo 'Original public-domain XSPICE digital latch event history or physical timing diverged' >&2; exit 1; }

        # Behavioral corruption must be detected even when the external
        # wrapper header remains formally correct. Swap the original code
        # model's DATA/ENABLE and SET/RESET port assignments independently,
        # then remove only the physical receiver load or miswire the inverted
        # output of the genuine stock vector DAC to catch silent Q-only tests.
        for sabotage in swap-data-enable swap-set-reset remove-input-loading miswire-inverted-output; do
            poisoned="$dir/poisoned-$sabotage"
            mkdir -p "$poisoned"
            cp -- "$dir/circuit.cir" "$dir/generated.inc" "$dir/.spiceinit" "$poisoned/"
            case "$sabotage" in
                swap-data-enable)
                    sed 's/Alatch DATA EN SET RESET Q NQ/Alatch EN DATA SET RESET Q NQ/' \
                        "$dir/native-georgiatech-dlatch.lib" >"$poisoned/native-georgiatech-dlatch.lib"
                    ;;
                swap-set-reset)
                    sed 's/Alatch DATA EN SET RESET Q NQ/Alatch DATA EN RESET SET Q NQ/' \
                        "$dir/native-georgiatech-dlatch.lib" >"$poisoned/native-georgiatech-dlatch.lib"
                    ;;
                remove-input-loading)
                    sed 's/capacitance=2p/capacitance=0/' \
                        "$dir/native-georgiatech-dlatch.lib" >"$poisoned/native-georgiatech-dlatch.lib"
                    ;;
                miswire-inverted-output)
                    # dac_bridge has vector type v: [OUT NOUT] means two
                    # independent single-ended nodes. [OUT 0 NOUT 0] is
                    # invalid differential-pair thinking and leaves NOUT
                    # effectively unobserved while Q still looks correct.
                    sed 's/\[OUT NOUT\]/[OUT 0 NOUT 0]/' \
                        "$dir/native-georgiatech-dlatch.lib" >"$poisoned/native-georgiatech-dlatch.lib"
                    ;;
            esac
            (
                cd "$poisoned"
                ngspice -b circuit.cir >run.log 2>&1 ||
                    { tail -n 85 run.log >&2; exit 1; }
                if rg -i 'error:|fatal error|timestep too small|no such model' run.log; then
                    echo "Poisoned stock XSPICE model failed to simulate: $sabotage" >&2
                    exit 1
                fi
            )
            if awk -v expected_q="$states_q" -v expected_nq="$states_nq" \
                -v min_delta_ps=150 -v max_delta_ps=250 \
                -f "$here/compare-event-traces.awk" "$stock" "$poisoned/result.dat" \
                >"$poisoned/expected-rejection.log" 2>&1; then
                echo "Poisoned original XSPICE event semantics/load falsely accepted: $sabotage" >&2
                exit 1
            fi
            printf 'Expected native digital event negative PASS: %s\n' "$sabotage"
        done
    fi
    if [[ $case == vendor-ti-gatedriver-x ]]; then
        verify_ti_native_contract "gate driver" "$dir" 2 1 \
            'Xvendor_a OUT_A EVENT_CONTROL EVENT_CONTROL_SR_SINK EVENT_CONTROL_SR_SOURCE RAIL 0 SR_TI_GATE_DRIVER_EVENT PARAMS: tdr=4n tdf=4n ttr=2n ttf=2n rsrc=20 isrc=4m rsnk=30 isnk=6m gmin=1e-10' \
            'Xvendor_b OUT_B EVENT_CONTROL EVENT_CONTROL_SR_SINK EVENT_CONTROL_SR_SOURCE RAIL 0 SR_TI_GATE_DRIVER_EVENT PARAMS: tdr=7n tdf=6n ttr=4n ttf=3n rsrc=50 isrc=8m rsnk=70 isnk=3m gmin=1e-10'
        rg -F -q 'sr_bundle_load(capacitance=2p' "$dir/native-ti-gatedriver.lib" ||
            { echo 'Native event input lacks physical sink/source bundle load' >&2; exit 1; }
        rg -F -q 'dac_bridge(out_low=0 out_high=1' "$dir/native-ti-gatedriver.lib" ||
            { echo 'Explicit original 0/1V analog logic level adaptation missing' >&2; exit 1; }
        # The original vendor INP was 0..1V analog-coded logic, not a native
        # event pin. The independently characterized ALB+DAC adapter is
        # pinned to 14 PWL kinks, and must not silently change its timing.
        awk -v min_samples=2 -v native_column=2 \
            -v abs_tol=.002 -v rms_tol=.0008 \
            -f "$here/compare-vendor-waveform.awk" \
            "$here/fixtures/vendor-external/ti-gatedriver-adapter-control.pwl" \
            "$dir/control-result.dat" ||
            { echo 'ALB physical event to original 0..1V control interface timing changed' >&2; exit 1; }
        # The original TI physical stage is run independently under the
        # measured adapter's *same* 0..1V control signal, not given the
        # wrong unadapted source waveform with a permissive error budget.
        reference="$here/fixtures/vendor-external/ti-gatedriver-event-reference.dat"
        verify_ti_physical_columns "gate driver" "$reference" "$dir/result.dat" \
            '2 2 V .06 .004' '3 4 A .0003 .00003' \
            '4 6 V .06 .004' '5 8 A .0003 .00003'
        awk '
          function near(value, wanted, tol) {
            difference=value-wanted
            return difference<tol && difference> -tol
          }
          # Source-limited and sink-limited current/voltage constraints
          # are manufacturer-specific, but both phases recur twice.
          # Keep all four *independent time windows* as mandatory witnesses.
          function source_limited() {
            return near($2,.8,.02) && near($4,-.004,.0001) &&
                   near($6,1.6,.03) && near($8,-.008,.0001)
          }
          function sink_limited() {
            return near($2,2.1,.03) && near($4,.006,.0001) &&
                   near($6,2.7,.03) && near($8,.003,.0001)
          }
          $1>35e-9 && $1<45e-9 && source_limited() {source_caps=1}
          $1>83e-9 && $1<95e-9 && sink_limited() {sink_caps=1}
          $1>145e-9 && $1<165e-9 && source_limited() {source_again=1}
          $1>214e-9 && $1<228e-9 && sink_limited() {sink_again=1}
          END {exit !(source_caps && sink_caps && source_again && sink_again)}
        ' "$dir/result.dat" ||
            { echo 'Original TI asymmetric source/sink current limit or timed gate switching failed' >&2; exit 1; }
        if awk -v reference_column=3 -v native_column=8 -v value_unit=A \
            -v abs_tol=.0003 -v rms_tol=.00003 \
            -f "$here/compare-vendor-waveform.awk" "$reference" "$dir/result.dat" \
            >"$dir/swapped-gatedriver-parameters.log" 2>&1; then
            echo 'Swapped source/sink vendor instance currents were incorrectly accepted' >&2
            exit 1
        fi
    fi
    if [[ $case == vendor-ti-nmos-x ]]; then
        ti_nmos_base='Xfet_base DRAIN_BASE ANALOG_INPUT 0 SR_TI_NMOS'
        ti_nmos_strong='Xfet_strong DRAIN_STRONG ANALOG_INPUT 0 SR_TI_NMOS PARAMS: von=2.4 vth=.4 ron=120 roff=5e8 imax=4m'
        verify_ti_native_contract "native NMOS" "$dir" 1 1 \
            "$ti_nmos_base" "$ti_nmos_strong"
        # An earlier regex-based ABI oracle falsely accepted decimal-point
        # substitutions such as von=2x4. Preserve an adversarial fixture:
        # the simulator need not run because the ABI itself must reject it.
        decimal_poison="$dir/invalid-decimal-parameter"
        mkdir -p -- "$decimal_poison"
        sed 's/von=2[.]4/von=2x4/' "$dir/circuit.cir" >"$decimal_poison/circuit.cir"
        if cmp -s -- "$dir/circuit.cir" "$decimal_poison/circuit.cir"; then
            echo 'Original TI NMOS native parameter poison was a no-op' >&2
            exit 1
        fi
        if verify_ti_native_contract "native NMOS" "$decimal_poison" 1 1 \
            "$ti_nmos_base" "$ti_nmos_strong" >"$decimal_poison/negative.log" 2>&1; then
            echo 'Malformed original TI NMOS decimal parameter passed exact ABI validation' >&2
            exit 1
        fi
        reference="$here/fixtures/vendor-external/ti-nmos-reference.dat"
        verify_ti_physical_columns "native NMOS" "$reference" "$dir/result.dat" \
            '2 2 V .001 .0001' '3 4 A .00001 .000001' \
            '4 6 V .001 .0001' '5 8 A .00001 .000001'
        awk '
          function near(value, wanted, tol) {
            difference=value-wanted
            return difference<tol && difference> -tol
          }
          $1>17e-9 && $1<19e-9 && near($4,0,.0000001) &&
                near($8,.000131275,.000005) {early_gate=1}
          $1>32e-9 && $1<34e-9 && near($4,.001659135,.00001) &&
                near($8,.002108494,.00001) {partial_gate=1}
          $1>61e-9 && $1<63e-9 && near($4,.01,.00001) &&
                near($8,.004,.00001) && near($2,1.8,.005) && near($6,2.7,.005) {limited=1}
          $1>84e-9 && $1<86e-9 && near($4,-.0075,.00001) &&
                near($8,-.005555556,.00001) &&
                near($2,-.375,.005) && near($6,-.666667,.005) {unlimited_reverse=1}
          $1>112e-9 && $1<115e-9 && near($4,0,.0000001) &&
                near($8,0,.0000001) {recovered_off=1}
          $1>131e-9 && $1<133e-9 && near($4,.01,.00001) &&
                near($8,.004,.00001) && near($2,2.5,.005) && near($6,3.4,.005) {high_bias=1}
          END {
            if (!(early_gate && partial_gate && limited && unlimited_reverse &&
                  recovered_off && high_bias)) {
              print "Incomplete actual TI NMOS OFF/partial/limit/reverse/high-bias behavior" >"/dev/stderr"
              exit 1
            }
          }
        ' "$dir/result.dat"
        # An intuitively plausible but wrong symmetric current limiter
        # clips the negative branch of the real original TI model.
        mkdir -p "$dir/symmetric-limit"
        cp -- "$dir/circuit.cir" "$dir/generated.inc" "$dir/.spiceinit" "$dir/symmetric-limit/"
        sed -e 's/I=min(imax,/I=max(-imax,min(imax,/' \
            -e 's@/ron))$@/ron)))@' \
            "$dir/native-ti-nmos.lib" >"$dir/symmetric-limit/native-ti-nmos.lib"
        (
            cd "$dir/symmetric-limit"
            ngspice -b circuit.cir >run.log 2>&1 || { tail -n 75 run.log >&2; exit 1; }
            if rg -i 'error:|fatal error|timestep too small|no such model' run.log; then
                echo 'Poisoned NMOS negative-limit experiment failed to execute' >&2
                exit 1
            fi
        )
        if awk -v reference_column=5 -v native_column=8 -v value_unit=A \
            -v abs_tol=.00001 -v rms_tol=.000001 \
            -f "$here/compare-vendor-waveform.awk" "$reference" \
            "$dir/symmetric-limit/result.dat" >"$dir/symmetric-negative-error.log" 2>&1; then
            echo 'Symmetric current clamp was falsely accepted against asymmetric vendor limit' >&2
            exit 1
        fi
        if awk -v reference_column=3 -v native_column=8 -v value_unit=A \
            -v abs_tol=.00001 -v rms_tol=.000001 \
            -f "$here/compare-vendor-waveform.awk" "$reference" \
            "$dir/result.dat" >"$dir/swapped-nmos-parameters-error.log" 2>&1; then
            echo 'Swapped NMOS voltage/current and parameter pair was falsely accepted' >&2
            exit 1
        fi
    fi
    if [[ $case == vendor-ti-comparator-x ]]; then
        verify_ti_analog_pair "$case" "$dir"
        awk '
          BEGIN {
            expected[1,1]=25.925; expected[1,2]=62.975
            expected[1,3]=113.505; expected[1,4]=140.235
            expected[2,1]=27.935; expected[2,2]=85.495
            expected[2,3]=115.485; expected[2,4]=164.635
          }
          {
            for (which=1; which<=2; which++) {
              level=(which==1 ? $2 : $4)
              if (NR>1 && ((last[which]<.5 && level>=.5) ||
                           (last[which]>=.5 && level<.5))) {
                transitions[which]++
                edge_no=transitions[which]
                if (edge_no>4) bad=1
                else {
                  tns=$1*1e9
                  d=tns-expected[which,edge_no]; if (d<0) d=-d
                  if (d>1) bad=1
                  printf "TI vendor feedback comparator variant %d edge %d: %.3f ns\n",which,edge_no,tns
                }
              }
              last[which]=level
            }
          }
          END {exit !(transitions[1]==4 && transitions[2]==4 && !bad)}
        ' "$dir/result.dat" ||
            { echo 'TI feedback comparator lost hysteresis edge history or timing' >&2; exit 1; }
    fi
    if [[ $case == native-digital-hiz-x ]]; then
        rg -q '^Xexternal DATA DATA_SR_SINK DATA_SR_SOURCE ENABLE ENABLE_SR_SINK ENABLE_SR_SOURCE ANALOG_OUTPUT SR_NATIVE_TRI_DRIVER$' "$dir/circuit.cir" ||
            { echo 'Independent native tri-state X ABI lost its two complete event bundles' >&2; exit 1; }
        [[ $(rg -F -c 'sr_family_adc(' "$dir/circuit.cir") == 2 ]] ||
            { echo 'Tri-state native X did not receive two independent physical ADCs' >&2; exit 1; }
        if rg -q 'sr_family_dac\(' "$dir/circuit.cir"; then
            echo 'Tri-state analog external output must be driven by its real native library, not invented autowire DAC' >&2
            exit 1
        fi
        # The independently supplied native library contains ngspice's
        # stock d_tristate, explicit event bundle loads and nonlinear DAC.
        # High-Z is an electrical state: 1V bias through 10k competing with
        # a 100k shunt gives ~0.909 V, not a fictitious digital 0 or 1.
        awk '
          $1>5e-9 && $1<6e-9 && $2>.89 && $2<.93 && $4< -8e-6 && $4> -10e-6 {initial_hiz=1}
          $1>15e-9 && $1<16e-9 && $2>3.27 && $2<3.31 && $4>2.2e-4 && $4<2.4e-4 {first_high=1}
          $1>25e-9 && $1<26e-9 && $2>.89 && $2<.93 && $4< -8e-6 && $4> -10e-6 {released=1}
          $1>35e-9 && $1<36e-9 && $2>3.27 && $2<3.31 && $4>2.2e-4 && $4<2.4e-4 {second_high=1}
          $1>45e-9 && $1<46e-9 && $2>.89 && $2<.93 && $4< -8e-6 && $4> -10e-6 {final_hiz=1}
          END {exit !(initial_hiz && first_high && released && second_high && final_hiz)}
        ' "$dir/result.dat" ||
            { echo 'Native external X HI_Z release or loaded analog current is incorrect' >&2; exit 1; }
        # A real external digital receiver advertises 2p on the event
        # state/sink/source triple. Removing only that load from the
        # independently supplied native library must advance both output
        # crossings by out_res(100 ohm) * 2p = 200 ps.
        mkdir -p "$dir/no-digital-load"
        cp -- "$dir/circuit.cir" "$dir/generated.inc" "$dir/.spiceinit" "$dir/no-digital-load/"
        sed 's/capacitance=2p/capacitance=0/' \
            "$dir/native-external-tristate.lib" \
            >"$dir/no-digital-load/native-external-tristate.lib"
        (
            cd "$dir/no-digital-load"
            ngspice -b circuit.cir >run.log 2>&1 || { tail -n 70 run.log >&2; exit 1; }
            if rg -i 'error:|fatal error|timestep too small|no such model' run.log; then
                echo 'No-load native X timing comparator failed' >&2
                exit 1
            fi
        )
        awk '
          FNR==1 {series++}
          FNR>1 {
            if (!rise[series] && t<2.5 && $2>=2.5 && $1>8e-9 && $1<18e-9)
              rise[series]=old_time+($1-old_time)*(2.5-t)/($2-t)
            if (!fall[series] && t>=2.5 && $2<2.5 && $1>18e-9 && $1<28e-9)
              fall[series]=old_time+($1-old_time)*(t-2.5)/(t-$2)
          }
          {old_time=$1; t=$2}
          END {
            rise_ps=(rise[1]-rise[2])*1e12
            fall_ps=(fall[1]-fall[2])*1e12
            printf "External native X 2p receiver-load timing: rise +%.3f ps, fall +%.3f ps\n",rise_ps,fall_ps
            exit !(rise[1] && rise[2] && fall[1] && fall[2] &&
                   rise_ps>150 && rise_ps<250 && fall_ps>150 && fall_ps<250)
          }
        ' "$dir/result.dat" "$dir/no-digital-load/result.dat" ||
            { echo 'Native external X digital load did not propagate 200ps through ALB' >&2; exit 1; }
    fi
    if [[ $case == native-digital-mixed-x ]]; then
        rg -q '^Xexternal FIRST FIRST_SR_SINK FIRST_SR_SOURCE SECOND SECOND_SR_SINK SECOND_SR_SOURCE ANALOG_TAP SR_NATIVE_MIXED_BUFFER$' "$dir/circuit.cir" ||
            { echo 'Mixed external native X analog TAP incorrectly expanded or misplaced' >&2; exit 1; }
        [[ $(rg -F -c 'sr_family_adc(' "$dir/circuit.cir") == 1 ]] ||
            { echo 'Mixed external X input did not share one real ADC' >&2; exit 1; }
        [[ $(rg -F -c 'sr_family_dac(' "$dir/circuit.cir") == 1 ]] ||
            { echo 'Mixed external X output did not produce one real DAC' >&2; exit 1; }
        awk '
            $1>1e-9 && $1<1.5e-9 && $2<.1 {initial_low=1}
            $1>10e-9 && $1<11e-9 && $2>3.1 {pulse_high=1}
            $1>14.5e-9 && $1<15e-9 && $2<.1 {released_low=1}
            {if ($4<.99 || $4>1.01) bad_load=1; samples++}
            END {exit !(initial_low && pulse_high && released_low && !bad_load && samples>100)}
        ' "$dir/result.dat" ||
            { echo 'Mixed native X lost event propagation or real 1k analog load' >&2; exit 1; }
    fi
    if [[ $case == opaque-x ]]; then
        [[ $(rg -F -c 'sr_family_adc(' "$dir/circuit.cir") == 1 ]] ||
            { echo 'Opaque X analog input witness did not insert ADC' >&2; exit 1; }
        [[ $(rg -F -c 'sr_family_dac(' "$dir/circuit.cir") == 1 ]] ||
            { echo 'Opaque X analog output witness did not insert DAC' >&2; exit 1; }
        rg -q '^Xinput analog_input inner_signal ANALOG_LINK$' "$dir/circuit.cir" ||
            { echo 'External analog input X was incorrectly lowered' >&2; exit 1; }
        rg -q '^Xoutput inner_output analog_output ANALOG_LINK$' "$dir/circuit.cir" ||
            { echo 'External analog output X was incorrectly lowered' >&2; exit 1; }
    fi
    if [[ $case == inner-contact ]]; then
        rg -q '^\.subckt TOP DIN DOUT ' "$dir/lowered.cir" ||
            { echo 'Analog input was incorrectly exported as digital' >&2; exit 1; }
        rg -q '^Rin din 0 10k$' "$dir/lowered.cir"
        rg -q 'SR_AUTO_ANALOG_INPUT_ADC' "$dir/circuit.cir"
        [[ $(rg -F -c 'sr_family_adc(' "$dir/circuit.cir") == 1 ]] ||
            { echo 'Scalar inner contact duplicated its physical ADC' >&2; exit 1; }
    fi
    if [[ $case == shared-producers ]]; then
        [[ $(rg -c 'sr_family_dac\(' "$dir/circuit.cir") == 2 ]] ||
            { echo 'Independent output drivers collapsed into one DAC' >&2; exit 1; }
        rg -q 'SR_AUTO_ANALOG_OUTPUT_DAC_1' "$dir/circuit.cir"
    fi
    count=$((count+1))
    printf 'AUTO PHYSICAL PASS %s\n' "$case"
done
"$here/smoke-georgiatech-ram.sh" "$cm" "$bins" "$scratch"
count=$((count+1))
"$here/smoke-georgiatech-ram-wide.sh" "$cm" "$bins" "$scratch"
count=$((count+1))
"$here/smoke-original-genlut-fanout.sh" "$cm" "$bins" "$scratch"
count=$((count+1))
"$here/smoke-original-mixed-bridge.sh" "$cm" "$bins" "$scratch"
count=$((count+1))
"$here/smoke-original-real-event-chain.sh" "$cm" "$bins" "$scratch"
count=$((count+1))
"$here/smoke-original-clocked-real.sh" "$cm" "$bins" "$scratch"
count=$((count+1))
"$here/smoke-original-real-strength-fanout.sh" "$cm" "$bins" "$scratch"
count=$((count+1))
"$here/smoke-original-full-strength-cascade.sh" "$cm" "$bins" "$scratch"
count=$((count+1))
"$here/smoke-original-contention.sh" "$cm" "$bins" "$scratch"
count=$((count+3))
"$here/smoke-original-real-sum.sh" "$cm" "$bins" "$scratch"
count=$((count+3))
"$here/smoke-original-real-cancellation.sh" "$cm" "$bins" "$scratch"
count=$((count+2))
"$here/smoke-original-real-nonfinite.sh" "$cm" "$bins" "$scratch"
count=$((count+4))
"$here/smoke-original-real-midpoint.sh" "$cm" "$bins" "$scratch"
count=$((count+1))
printf '%s/%s zero-hand-annotated original-source physical ALB circuits passed\n' "$count" "$count"
