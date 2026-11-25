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
for tool in alb-logical-expand alb-family-autowire; do
    test -x "$bins/$tool" || { echo "Missing $tool" >&2; exit 1; }
done
work="$(mktemp -d "$scratch/ngspice_preprocess_occurrences_XXXXXXXX")"
trap 'rm -rf -- "$work"' EXIT
"$bins/alb-logical-expand" \
    "$here/fixtures/family/occurrence-3calls.cir" \
    "$here/fixtures/family/occurrence-3calls.json" \
    "$work/lowered.cir" "$work/generated.inc"
"$bins/alb-family-autowire" \
    "$work/lowered.cir" "$work/generated.inc" \
    "$here/fixtures/family/occurrence-3calls.json" \
    "$here/fixtures/family/auto-models.json" "$work/circuit.cir"
[[ $(rg -c '^\.subckt TOP_SRV' "$work/circuit.cir") == 2 ]] ||
    { echo 'Different supplies did not create exactly two variant definitions' >&2; exit 1; }
rg -q '^Xone IN_A OUT_A TOP_SRV0$' "$work/circuit.cir"
rg -q '^Xtwo IN_B OUT_B TOP_SRV1$' "$work/circuit.cir"
rg -q '^Xthree IN_C OUT_C TOP_SRV0$' "$work/circuit.cir"
[[ $(rg -F -c 'sr_family_adc(' "$work/circuit.cir") == 2 ]] ||
    { echo 'Expected one ADC model per electrical variant' >&2; exit 1; }
[[ $(rg -F -c 'sr_family_dac(' "$work/circuit.cir") == 2 ]] ||
    { echo 'Expected one DAC model per electrical variant' >&2; exit 1; }
printf 'codemodel %s\n' "$cm" > "$work/.spiceinit"
(
    cd "$work"
    ngspice -b circuit.cir > sim.log 2>&1 || { tail -n 100 sim.log >&2; exit 1; }
    if rg -i 'fatal error|timestep too small|error:|no such model' sim.log; then
        echo 'Native occurrence simulation failed' >&2; exit 1
    fi
    test -s result.dat
)
awk '$1>10e-9 && $1<11e-9 && $2>3.1 && $2<3.31 &&
     $4>=-.01 && $4<.1 && $6>3.1 && $6<3.31 {good=1}
     END {exit !good}' "$work/result.dat" ||
    { echo 'Two 3.3V instances HIGH and 5V instance LOW not simultaneously observed' >&2; exit 1; }
echo '1/1 native mixed-supply occurrence transients pass (HIGH/LOW/HIGH simultaneously)'
