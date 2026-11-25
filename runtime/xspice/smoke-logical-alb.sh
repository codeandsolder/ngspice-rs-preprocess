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
for variant in original-3v3 original-5v nested shared device-variants device-variants-comb stateless-user-on stateless-user-off; do
    dir="$work/$variant"
    mkdir -p "$dir"
    cp -- "$work/.spiceinit" "$dir/"
    case "$variant" in
        original-3v3) source=logical-original.cir; binding=auto-bindings-3v3.json ;;
        original-5v) source=logical-original.cir; binding=auto-bindings-5v.json ;;
        nested) source=logical-nested.cir; binding=auto-bindings-3v3.json ;;
        shared) source=logical-shared.cir; binding=auto-bindings-shared.json ;;
        stateless-user-on|stateless-user-off) source=logical-stateless-user.cir; binding=auto-bindings-3v3.json ;;
        device-variants) source=logical-device-variants.cir; binding=auto-bindings-device-variants.json ;;
        device-variants-comb) source=logical-device-comb.cir; binding=auto-bindings-device-variants.json ;;
    esac
    source_path="$here/fixtures/family/$source"
    if [[ $variant == stateless-user-off ]]; then
        sed 's/user_scale=3/user_scale=1/' "$source_path" >"$dir/off.cir"
        source_path="$dir/off.cir"
    fi
    "$bins/alb-logical-expand" \
        "$source_path" \
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
        rg -q 'g_pullup=2\.4e-2 g_pulldown=3\.4e-2' "$dir/circuit.cir"
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
# The same ALB body with different MODEL USER_SCALE must compile to distinct
# truth tables and produce distinct physical outputs.
awk '$1>10e-9 && $1<11e-9 && $2>3.1 && $2<3.31 {found=1}
     END {exit !found}' "$work/stateless-user-on/result.dat" ||
    { echo 'Stateless model USER_SCALE=3 failed to enable output' >&2; exit 1; }
awk '$1>10e-9 && $1<11e-9 && $2>=-.01 && $2<.1 {found=1}
     END {exit !found}' "$work/stateless-user-off/result.dat" ||
    { echo 'Stateless model USER_SCALE=1 failed to disable output' >&2; exit 1; }
echo '2/2 additional stateless model USER/USER_SCALE native physical circuits passed'
# Three original ALB A devices share one original model, but DEVICE=[1]
# and DEVICE=[0] must become two, not three independent native code models.
awk '$1>10e-9 && $1<11e-9 && $2>3.1 && $2<3.31 && $4>=-.01 && $4<.1 && $6>3.1 && $6<3.31 {found=1}
     END {exit !found}' "$work/device-variants/result.dat" ||
    { echo 'Physical DEVICE per-instance variants lost independence' >&2; exit 1; }
[[ $(rg -c '^.subckt SRALB' "$work/device-variants/generated.inc") == 2 ]] ||
    { echo 'Identical DEVICE model environments were not deduplicated' >&2; exit 1; }
rg -q '^XSR_A1 .* SRALB0$' "$work/device-variants/lowered.cir"
rg -q '^XSR_A2 .* SRALB1$' "$work/device-variants/lowered.cir"
rg -q '^XSR_A3 .* SRALB0$' "$work/device-variants/lowered.cir"
echo '3/3 per-instance DEVICE vectors selected correctly (two deduplicated native model variants)'
# Registered COMB uses both DEVICE[0] for logic and DEVICE[1] for
# original per-instance timing. Identical vectors share one native model.
awk '$1>10e-9 && $1<11e-9 && $2>3.1 && $2<3.31 && $4>=-.01 && $4<.1 && $6>3.1 && $6<3.31 {found=1}
     END {exit !found}' "$work/device-variants-comb/result.dat" ||
    { echo 'Registered COMB DEVICE logic/timing specialization failed' >&2; exit 1; }
[[ $(rg -c '^\.subckt SRALB' "$work/device-variants-comb/generated.inc") == 2 ]] ||
    { echo 'Registered COMB DEVICE model environments were not deduplicated' >&2; exit 1; }
rg -q '^XSR_A1 .* SRALB0$' "$work/device-variants-comb/lowered.cir"
rg -q '^XSR_A2 .* SRALB1$' "$work/device-variants-comb/lowered.cir"
rg -q '^XSR_A3 .* SRALB0$' "$work/device-variants-comb/lowered.cir"
echo '3/3 original registered COMB per-instance DEVICE environments correctly specialized'
