#!/usr/bin/env bash
set -euo pipefail
if (( $# < 3 || $# > 4 )); then
    printf 'usage: %s CODEMODEL_CM RUST_BIN_DIR SCRATCH_ROOT [producer-identity-drivers]\n' "$0" >&2
    exit 2
fi
here="$(cd -- "$(dirname -- "$0")" && pwd)"
cm="$(realpath -- "$1")"
bins="$(realpath -- "$2")"
scratch="$(realpath -- "$3")"
fixture="${4:-multi-physical-drivers}"
case "$fixture" in
    multi-physical-drivers|producer-identity-drivers|promoted-x-formal-drivers) ;;
    *) echo "Unknown multi-driver fixture: $fixture" >&2; exit 2 ;;
esac
for tool in ngspice rg awk; do command -v "$tool" >/dev/null; done
for tool in alb-logical-expand alb-family-autowire; do
    test -x "$bins/$tool" || { echo "Missing $tool" >&2; exit 1; }
done
work="$(mktemp -d "$scratch/ngspice_preprocess_multi_driver_XXXXXXXX")"
trap 'rm -rf -- "$work"' EXIT
config="$here/fixtures/family/$fixture.json"
"$bins/alb-logical-expand"     "$here/fixtures/family/$fixture.cir" "$config"     "$work/lowered.cir" "$work/generated.inc"
"$bins/alb-family-autowire"     "$work/lowered.cir" "$work/generated.inc" "$config"     "$here/fixtures/family/auto-models.json" "$work/circuit.cir"

rg -q '^\.subckt TOP PIN A B$' "$work/circuit.cir"
rg -q '^Xone PIN A B TOP$' "$work/circuit.cir"
if [[ $fixture == promoted-x-formal-drivers ]]; then
    rg -q '^\.subckt GATE .*SR_PROMOTED_OUT_0.*SR_PROMOTED_OUT_1' "$work/circuit.cir"
    rg -q '^\.subckt GROUP .*SR_PROMOTED_OUT_0.*SR_PROMOTED_OUT_1' "$work/circuit.cir"
    rg -q '^Xgateway .*D_LEFT D_LEFT_SR_SINK D_LEFT_SR_SOURCE.*D_RIGHT D_RIGHT_SR_SINK D_RIGHT_SR_SOURCE GATE$' "$work/circuit.cir"
    rg -q '^Xidle .*SPARE SPARE_SR_SINK SPARE_SR_SOURCE GROUP$' "$work/circuit.cir" ||
        rg -q '^Xidle .*SPARE SPARE_SR_SINK SPARE_SR_SOURCE.*GROUP$' "$work/circuit.cir"
else
    rg -q '^Xleft D_A D_A_SR_SINK D_A_SR_SOURCE D_LEFT D_LEFT_SR_SINK D_LEFT_SR_SOURCE OUTER$' "$work/circuit.cir"
    rg -q '^Xright D_B D_B_SR_SINK D_B_SR_SOURCE D_RIGHT D_RIGHT_SR_SINK D_RIGHT_SR_SOURCE OUTER$' "$work/circuit.cir"
fi
rg -q '^AsrFamily[[:digit:]]+ D_LEFT D_LEFT_SR_SINK D_LEFT_SR_SOURCE %gd\(PIN 0\)' "$work/circuit.cir"
rg -q '^AsrFamily[[:digit:]]+ D_RIGHT D_RIGHT_SR_SINK D_RIGHT_SR_SOURCE %gd\(PIN 0\)' "$work/circuit.cir"
[[ $(rg -c 'sr_family_adc\(' "$work/circuit.cir") == 2 ]] ||
    { echo 'Expected two independent ADC inputs' >&2; exit 1; }
[[ $(rg -c 'sr_family_dac\(' "$work/circuit.cir") == 2 ]] ||
    { echo 'Expected two independent physical DAC drivers' >&2; exit 1; }
[[ $(rg -c 'sr_bundle_load\(' "$work/generated.inc") == 1 ]] ||
    { echo 'Expected one shared ALB load model definition' >&2; exit 1; }
printf 'codemodel %s\n' "$cm" > "$work/.spiceinit"
(
    cd "$work"
    ngspice -b circuit.cir >sim.log 2>&1 || { tail -n 120 sim.log >&2; exit 1; }
    if rg -i 'fatal error|timestep too small|error:|no such model' sim.log; then
        echo 'Independent output-driver physical simulation failed' >&2; exit 1
    fi
    test -s result.dat
)
# Columns: TIME PIN, TIME A, TIME B. Both drivers start LOW.
awk '$1>6e-9 && $1<7e-9 && $2<.20 {seen=1} END {exit !seen}' "$work/result.dat" ||
    { echo 'Both native DACs did not drive LOW together' >&2; exit 1; }
# With 40 ohm low drive, 0.55 V high-driver current knee and 100 kOhm
# load, KCL predicts 0.54978 V (not an ideal symmetric divider).
awk 'BEGIN {expect=(3.3-2.75)*(.025/(.025+1/100000)); tol=.025}
     $1>12e-9 && $1<13e-9 && $2>expect-tol && $2<expect+tol &&
     $4>3.2 && $6<.1 {seen=1} END {exit !seen}' "$work/result.dat" ||
    { echo 'A HIGH/B LOW must produce analog contention voltage, not digital resolution' >&2; exit 1; }
awk '$1>18e-9 && $1<19e-9 && $2>3.05 &&
     $4>3.2 && $6>3.2 {seen=1} END {exit !seen}' "$work/result.dat" ||
    { echo 'Both HIGH drivers failed to raise the physical bus' >&2; exit 1; }
awk 'BEGIN {expect=(3.3-2.75)*(.025/(.025+1/100000)); tol=.025}
     $1>26e-9 && $1<27e-9 && $2>expect-tol && $2<expect+tol &&
     $4<.1 && $6>3.2 {seen=1} END {exit !seen}' "$work/result.dat" ||
    { echo 'A LOW/B HIGH contention did not resolve electrically' >&2; exit 1; }
awk '$1>35e-9 && $1<36e-9 && $2<.2 &&
     $4<.1 && $6<.1 {seen=1} END {exit !seen}' "$work/result.dat" ||
    { echo 'Bus did not return LOW under two low DAC drivers' >&2; exit 1; }
echo '1/1 ngspice independent dual-DAC physical contention smoke passes'
