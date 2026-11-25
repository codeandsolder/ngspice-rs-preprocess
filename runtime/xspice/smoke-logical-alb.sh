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
for name in alb-logical-expand alb-family-autowire; do
    test -x "$bins/$name" || { echo "Missing compiler $name" >&2; exit 1; }
done
work="$(mktemp -d "$scratch/ngspice_preprocess_logical_XXXXXXXX")"
trap 'rm -rf -- "$work"' EXIT
printf 'codemodel %s\n' "$cm" >"$work/.spiceinit"
for variant in original-3v3 original-5v nested shared; do
    dir="$work/$variant"
    mkdir -p "$dir"
    cp -- "$work/.spiceinit" "$dir/"
    case "$variant" in
        original-3v3) source=logical-original.cir; binding=auto-bindings-3v3.json ;;
        original-5v) source=logical-original.cir; binding=auto-bindings-5v.json ;;
        nested) source=logical-nested.cir; binding=auto-bindings-3v3.json ;;
        shared) source=logical-shared.cir; binding=auto-bindings-shared.json ;;
    esac
    "$bins/alb-logical-expand" \
        "$here/fixtures/family/$source" \
        "$here/fixtures/family/$binding" \
        "$dir/lowered.cir" "$dir/generated.inc"
    "$bins/alb-family-autowire" \
        "$dir/lowered.cir" "$dir/generated.inc" \
        "$here/fixtures/family/$binding" \
        "$here/fixtures/family/auto-models.json" "$dir/circuit.cir"
    if [[ $variant == shared ]]; then
        [[ $(rg -c 'sr_family_adc\(' "$dir/circuit.cir") == 1 ]] ||
            { echo 'Original shared digital input split into several ADCs' >&2; exit 1; }
        [[ $(rg -c '^XSR_A[12] INPUT INPUT_SR_SINK INPUT_SR_SOURCE' "$dir/circuit.cir") == 2 ]] ||
            { echo 'Two original ALB receivers do not share a physical triple' >&2; exit 1; }
    else
        rg -q 'sr_family_adc\(' "$dir/circuit.cir"
        rg -q 'sr_family_dac\(' "$dir/circuit.cir"
        rg -q 'g_pullup=2\.5e-2 g_pulldown=2\.5e-2' "$dir/circuit.cir"
    fi
    (
        cd "$dir"
        if ! ngspice -b circuit.cir >run.log 2>&1; then
            tail -n 95 run.log >&2
            exit 1
        fi
        if rg -i 'fatal error|timestep too small|error:|no such model' run.log; then
            echo "Original ALB end-to-end circuit failed: $variant" >&2
            exit 1
        fi
        test -s result.dat || { echo "Missing waveform for $variant" >&2; exit 1; }
    )
done
for variant in original-3v3 nested; do
    awk '$1>10e-9 && $1<11e-9 && $2>3.1 && $2<3.31 {found=1}
         END {exit !found}' "$work/$variant/result.dat" ||
        { echo "Original ALB logical source did not drive HIGH: $variant" >&2; exit 1; }
done
awk '$1>10e-9 && $1<11e-9 && $2>=-.01 && $2<.1 {found=1}
     END {exit !found}' "$work/original-5v/result.dat" ||
    { echo 'Original ALB nominal HC input incorrectly became HIGH' >&2; exit 1; }
awk '$1>4.99e-9 && $1<5.01e-9 && $2>3.04 && $2<3.29 {found=1}
     END {exit !found}' "$work/shared/result.dat" ||
    { echo 'Original ALB two-receiver 4pF analog input load failed' >&2; exit 1; }
echo '4/4 original SIMetrix A + embedded D_LOGIC_BLOCK native ngspice circuits passed'
