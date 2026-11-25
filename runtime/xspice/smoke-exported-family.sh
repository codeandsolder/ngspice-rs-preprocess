#!/usr/bin/env bash
set -euo pipefail
if (( $# != 3 )); then
    printf 'usage: %s CODEMODEL_CM RUST_BIN_DIR SCRATCH_ROOT\n' "$0" >&2
    exit 2
fi
here="$(cd -- "$(dirname -- "$0")" && pwd)"
cm="$(realpath -- "$1")"
bins="$(realpath -- "$2")"
scratch="$(realpath -- "$3")"
for tool in ngspice rg awk; do command -v "$tool" >/dev/null; done
for bin in alb-logical-expand alb-family-autowire; do
    test -x "$bins/$bin" || { echo "Missing $bin" >&2; exit 1; }
done
work="$(mktemp -d "$scratch/ngspice_preprocess_exported_XXXXXXXX")"
trap 'rm -rf -- "$work"' EXIT
for variant in original-3v3 original-5v nested-3v3 shared mixed; do
    dir="$work/$variant"
    mkdir -p "$dir"
    case "$variant" in
        original-3v3) source=exported-original.cir; config=exported-original-3v3.json ;;
        original-5v) source=exported-original.cir; config=exported-original-5v.json ;;
        nested-3v3) source=exported-nested.cir; config=exported-nested-3v3.json ;;
        shared) source=exported-shared.cir; config=exported-shared.json ;;
        mixed) source=exported-mixed.cir; config=exported-mixed.json ;;
    esac
    "$bins/alb-logical-expand" \
        "$here/fixtures/family/$source" \
        "$here/fixtures/family/$config" \
        "$dir/lowered.cir" "$dir/generated.inc"
    "$bins/alb-family-autowire" \
        "$dir/lowered.cir" "$dir/generated.inc" \
        "$here/fixtures/family/$config" \
        "$here/fixtures/family/auto-models.json" "$dir/circuit.cir"
    if [[ "$variant" == original-* ]]; then
        rg -q '^Xone IN_A OUT_A TOP$' "$dir/circuit.cir"
        rg -q '^Xtwo IN_B OUT_B TOP$' "$dir/circuit.cir"
        rg -q '^\.subckt TOP ANALOG_IN ANALOG_OUT$' "$dir/circuit.cir"
    fi
    if [[ "$variant" == nested-3v3 ]]; then
        rg -q '^\.subckt INNER PIN_IN PIN_OUT$' "$dir/circuit.cir"
        rg -q '^Xinner PUBLIC_IN PUBLIC_OUT INNER$' "$dir/circuit.cir"
        rg -q '^Xtop ANALOG_IN ANALOG_OUT OUTER$' "$dir/circuit.cir"
    fi
    if [[ $variant == mixed ]]; then
        [[ $(rg -c '^AsrFamily0' "$dir/circuit.cir") == 2 ]] ||
            { echo 'Producer ADC and consumer DAC must be definition-local' >&2; exit 1; }
        [[ $(rg -F -c 'sr_family_adc(' "$dir/circuit.cir") == 1 ]] ||
            { echo 'Expected exactly one producer ADC' >&2; exit 1; }
        [[ $(rg -F -c 'sr_family_dac(' "$dir/circuit.cir") == 1 ]] ||
            { echo 'Expected exactly one consumer DAC definition' >&2; exit 1; }
        rg -q '^Xproducer ANALOG_IN BUS BUS_SR_SINK BUS_SR_SOURCE PRODUCER$' "$dir/circuit.cir"
        rg -q '^Xconsumer_a BUS BUS_SR_SINK BUS_SR_SOURCE OUT_A CONSUMER$' "$dir/circuit.cir"
        rg -q '^Xconsumer_b BUS BUS_SR_SINK BUS_SR_SOURCE OUT_B CONSUMER$' "$dir/circuit.cir"
    else
        [[ $(rg -c '^AsrFamily0' "$dir/circuit.cir") == 1 ]] ||
            { echo "Expected one definition-local ADC: $variant" >&2; exit 1; }
        if [[ $variant == shared ]]; then
            if rg -F -q 'sr_family_dac(' "$dir/circuit.cir"; then
                echo 'Shared exported ADC unexpectedly emitted DAC' >&2; exit 1
            fi
            [[ $(rg -c '^XSR_A[12] D_IN D_IN_SR_SINK D_IN_SR_SOURCE' "$dir/circuit.cir") == 2 ]] ||
                { echo 'Two ALB receivers do not share digital triple' >&2; exit 1; }
        else
            [[ $(rg -c '^AsrFamily1' "$dir/circuit.cir") == 1 ]] ||
                { echo "Expected definition-local DAC: $variant" >&2; exit 1; }
        fi
    fi
    printf 'codemodel %s\n' "$cm" >"$dir/.spiceinit"
    (
        cd "$dir"
        ngspice -b circuit.cir >sim.log 2>&1 || { tail -n 95 sim.log; exit 1; }
        if rg -i 'fatal error|timestep too small|error:|no such model' sim.log; then
            echo "Native exported-port simulation failed: $variant" >&2
            exit 1
        fi
        [[ -s result.dat ]] || { echo "Missing waveform: $variant" >&2; exit 1; }
    )
done
for variant in original-3v3 nested-3v3; do
    awk '$1>10e-9 && $1<11e-9 && $2>3.1 && $2<3.31 {ok=1}
         END {exit !ok}' "$work/$variant/result.dat" ||
        { echo "Exported $variant did not drive scaled HIGH" >&2; exit 1; }
done
awk '$1>10e-9 && $1<11e-9 && $2>3.1 && $2<3.31 && $4>3.1 && $4<3.31 {both=1}
     END {exit !both}' "$work/original-3v3/result.dat" ||
    { echo 'Two independently instantiated analog-interface ALBs did not both drive HIGH' >&2; exit 1; }
awk '$1>10e-9 && $1<11e-9 && $2>=-.01 && $2<.1 && $4>=-.01 && $4<.1 {both=1}
     END {exit !both}' "$work/original-5v/result.dat" ||
    { echo 'Analog-facing 5V HC nominal accepted 1.5V input incorrectly' >&2; exit 1; }
awk '$1>4.99e-9 && $1<5.01e-9 && $2>3.04 && $2<3.29 {yes=1}
     END {exit !yes}' "$work/shared/result.dat" ||
    { echo 'Exported shared ADC did not reflect two 2pF ALB loads' >&2; exit 1; }
awk '$1>15e-9 && $1<17e-9 && $2>3.1 && $2<3.31 && $4>3.1 && $4<3.31 {both=1}
     END {exit !both}' "$work/mixed/result.dat" ||
    { echo 'Producer digital bus did not drive both analog-facing consumers' >&2; exit 1; }
echo '5/5 exported analog port ALB→native ngspice simulations passed'
