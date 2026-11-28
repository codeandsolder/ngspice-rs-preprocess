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
test -x "$bins/alb-family-compile" || { echo "Missing alb-family-compile" >&2; exit 1; }
work="$(mktemp -d "$scratch/ngspice_preprocess_occurrences_XXXXXXXX")"
trap 'rm -rf -- "$work"' EXIT
"$bins/alb-family-compile" \
    "$here/fixtures/family/occurrence-3calls.cir" \
    "$here/fixtures/family/occurrence-3calls.json" \
    "$here/fixtures/family/auto-models.json" "$work/circuit.cir" "$work/generated.inc"
[[ $(rg -c '^\.subckt SRALB' "$work/generated.inc") == 1 ]] ||
    { echo 'Identical specialized ALB kernels must be emitted once' >&2; exit 1; }
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

# The same logical definition has different analog/digital port widths.
dir="$work/heterogeneous"
mkdir -p "$dir"
"$bins/alb-family-compile" \
    "$here/fixtures/family/occurrence-heterogeneous.cir" \
    "$here/fixtures/family/occurrence-heterogeneous.json" \
    "$here/fixtures/family/auto-models.json" "$dir/circuit.cir" "$dir/generated.inc"
rg -q '^Xone IN_A OUT_A TOP_SRV0$' "$dir/circuit.cir"
rg -q '^Xtwo BUS BUS_SR_SINK BUS_SR_SOURCE OUT_B TOP_SRV1$' "$dir/circuit.cir"
[[ $(rg -F -c 'sr_family_adc(' "$dir/circuit.cir") == 2 ]]
[[ $(rg -F -c 'sr_family_dac(' "$dir/circuit.cir") == 2 ]]
printf 'codemodel %s\n' "$cm" > "$dir/.spiceinit"
(
    cd "$dir"
    ngspice -b circuit.cir >sim.log 2>&1 || { tail -n 100 sim.log >&2; exit 1; }
    if rg -i 'fatal error|timestep too small|error:|no such model' sim.log; then
        exit 1
    fi
    test -s result.dat
)
# The independent analog-input occurrence remains high; the digitized
# pulse traverses the direct digital interstage connection in the other.
awk '
    $1>4e-9 && $1<5e-9 && $4>=-.01 && $4<.1 {before=1}
    $1>14e-9 && $1<16e-9 && $2>3.1 && $2<3.31 && $4>3.1 && $4<3.31 {high=1}
    $1>27e-9 && $1<29e-9 && $2>3.1 && $2<3.31 && $4>=-.01 && $4<.1 {after=1}
    END {exit !(before && high && after)}
' "$dir/result.dat" || {
    echo 'Heterogeneous analog/digital occurrence transient mismatch' >&2
    exit 1
}
echo 'Native heterogeneous same-definition analog/digital port transient: PASS'
