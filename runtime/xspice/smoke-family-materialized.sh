#!/usr/bin/env bash
set -euo pipefail
if (( $# != 5 )); then
    printf 'Usage: %s CODEMODEL_CM RUST_BINARY_DIR ALB_BUFFER_INC ALB_ZERO_CURRENT_INC SCRATCH_ROOT\n' "$0" >&2
    exit 2
fi
here="$(cd -- "$(dirname -- "$0")" && pwd)"
model="$(realpath -- "$1")"
bins="$(realpath -- "$2")"
alb="$(realpath -- "$3")"
alb_zero="$(realpath -- "$4")"
scratch="$(realpath -- "$5")"
for path in "$model" "$alb" "$alb_zero" "$bins/alb-family-resolve" "$bins/alb-family-materialize"; do
    [[ -f $path ]] || { echo "Missing family materialization input: $path" >&2; exit 1; }
done
for tool in ngspice awk rg; do command -v "$tool" >/dev/null; done
work="$(mktemp -d "$scratch/ngspice_preprocess_native_family_XXXXXXXX")"
trap 'rm -rf -- "$work"' EXIT
printf 'codemodel %s\n' "$model" >"$work/.spiceinit"
for variant in 3v3 5v; do
    dir="$work/$variant"
    mkdir -p "$dir"
    cp -- "$here/fixtures/family/materialized-buffer.cir" "$dir/"
    cp -- "$alb" "$dir/generated.inc"
    cp -- "$work/.spiceinit" "$dir/"
    "$bins/alb-family-resolve" \
        "$here/fixtures/family/compat-synthetic.txt" \
        "$here/fixtures/family/buffer-plan-$variant.json" \
        "$dir/resolved.json"
    "$bins/alb-family-materialize" \
        "$dir/resolved.json" \
        "$here/fixtures/family/buffer-native.json" \
        "$dir/bridge.inc"
    if [[ $variant == 3v3 ]]; then
        rg -q 'in_low=1\.386e0 in_high=1\.452e0' "$dir/bridge.inc"
        rg -q 'out_high=3\.3e0' "$dir/bridge.inc"
    else
        rg -q 'in_low=2\.1e0 in_high=2\.2e0' "$dir/bridge.inc"
        rg -q 'out_high=5e0' "$dir/bridge.inc"
    fi
    rg -q 'g_pullup=2\.5e-2 g_pulldown=2\.5e-2' "$dir/bridge.inc"
    (
        cd "$dir"
        if ! ngspice -b materialized-buffer.cir >native.log 2>&1; then
            tail -n 80 native.log >&2
            exit 1
        fi
        if rg -i 'fatal error|timestep too small|error:|no such model' native.log; then
            echo "Native generated family simulation failed for $variant" >&2
            exit 1
        fi
        [[ -s result.dat ]] || { echo "Missing generated waveform for $variant" >&2; exit 1; }
    )
done
awk '$1>10e-9 && $1<11e-9 && $2>3.1 && $2<3.31 {high=1}
     END {exit !high}' "$work/3v3/result.dat" ||
    { echo '3.3 V supply failed to scale ADC threshold and DAC output high' >&2; exit 1; }
awk '$1>10e-9 && $1<11e-9 && $2>=-.01 && $2<.1 {low=1}
     END {exit !low}' "$work/5v/result.dat" ||
    { echo '5 V nominal supply incorrectly treated 1.5 V as HC high' >&2; exit 1; }
# Two separate compiled ALB physical inputs contribute 2pF each to
# one shared family ADC, for a total C=4pF. With a 1k source its analog
# RC time constant is ~4ns; after 4ns the expected input is ~3.16V.
# No sr_bundle_load is emitted by the bridge planner.
shared="$work/shared"
mkdir -p "$shared"
cp -- "$here/fixtures/family/materialized-shared-rc.cir" "$shared/"
cp -- "$alb_zero" "$shared/generated.inc"
cp -- "$work/.spiceinit" "$shared/"
"$bins/alb-family-resolve" \
    "$here/fixtures/family/compat-synthetic.txt" \
    "$here/fixtures/family/shared-adc-plan.json" \
    "$shared/resolved.json"
"$bins/alb-family-materialize" \
    "$shared/resolved.json" \
    "$here/fixtures/family/shared-adc-native.json" \
    "$shared/bridge.inc"
[[ $(rg -c 'sr_family_adc\(' "$shared/bridge.inc") == 1 ]] ||
    { echo 'Shared ADC was duplicated instead of merged' >&2; exit 1; }
if rg 'sr_bundle_load' "$shared/bridge.inc"; then
    echo 'Native ADC emitter duplicated physical ALB input loads' >&2
    exit 1
fi
(
    cd "$shared"
    if ! ngspice -b materialized-shared-rc.cir >native.log 2>&1; then
        tail -n 80 native.log >&2
        exit 1
    fi
    if rg -i 'fatal error|timestep too small|error:|no such model' native.log; then
        echo 'Shared family ADC ngspice simulation errored' >&2
        exit 1
    fi
    [[ -s result.dat ]] || { echo 'Missing shared ADC waveform' >&2; exit 1; }
)
awk '$1>4.99e-9 && $1<5.01e-9 && $2>3.04 && $2<3.29 {rc=1}
     END {exit !rc}' "$shared/result.dat" ||
    { echo 'Shared ADC did not reflect exactly two physical 2pF ALB input loads' >&2; exit 1; }
echo '3/3 source-only native family materialization simulations passed.'
