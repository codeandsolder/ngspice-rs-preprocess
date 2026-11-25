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
work="$(mktemp -d "$scratch/ngspice_preprocess_nested_bidir_XXXXXXXX")"
trap 'rm -rf -- "$work"' EXIT
config="$here/fixtures/family/bidirectional-port.json"
"$bins/alb-logical-expand"     "$here/fixtures/family/nested-bidirectional-port.cir" "$config"     "$work/lowered.cir" "$work/generated.inc"
"$bins/alb-family-autowire"     "$work/lowered.cir" "$work/generated.inc" "$config"     "$here/fixtures/family/auto-models.json" "$work/circuit.cir"

rg -q '^\.subckt TOP PIN OE MONITOR$' "$work/circuit.cir"
rg -q '^Xone PIN OE OBSERVE TOP$' "$work/circuit.cir"
rg -q '^Xsense D_SENSE D_SENSE_SR_SINK D_SENSE_SR_SOURCE D_MON ' "$work/circuit.cir"
rg -q '^Xdriver D_OE D_OE_SR_SINK D_OE_SR_SOURCE D_DRIVE ' "$work/circuit.cir"
rg -q '^AsrFamily[[:digit:]]+ %gd\(PIN 0\) D_SENSE D_SENSE_SR_SINK D_SENSE_SR_SOURCE' "$work/circuit.cir"
rg -q '^AsrFamily[[:digit:]]+ D_DRIVE D_DRIVE_SR_SINK D_DRIVE_SR_SOURCE %gd\(PIN 0\)' "$work/circuit.cir"
[[ $(rg -c 'sr_family_adc\(' "$work/circuit.cir") == 2 ]] ||
    { echo 'Expected PIN/OE ADC models' >&2; exit 1; }
[[ $(rg -c 'sr_family_dac\(' "$work/circuit.cir") == 2 ]] ||
    { echo 'Expected PIN/MONITOR DAC models' >&2; exit 1; }
[[ $(rg -c 'sr_bundle_load\(' "$work/generated.inc") == 2 ]] ||
    { echo 'Each remote ALB input needs exactly one input-load model' >&2; exit 1; }

printf 'codemodel %s\n' "$cm" > "$work/.spiceinit"
(
    cd "$work"
    ngspice -b circuit.cir > sim.log 2>&1 || { tail -n 120 sim.log >&2; exit 1; }
    if rg -i 'fatal error|timestep too small|error:|no such model' sim.log; then
        echo 'Nested bidirectional ALB simulation failed' >&2; exit 1
    fi
    test -s result.dat
)
# TIME, PIN, TIME, MONITOR, TIME, OE, TIME, EXT.
awk '$1>5e-9 && $1<5.2e-9 && $2<.15 && $4<.15 && $6<.1 {ok=1}
     END {exit !ok}' "$work/result.dat" ||
    { echo 'Initial driven-low bus was not sensed LOW' >&2; exit 1; }
awk '$1>13.5e-9 && $1<13.8e-9 && $2<.25 && $4<.25 &&
     $6<.1 && $8>3.2 {ok=1} END {exit !ok}' "$work/result.dat" ||
    { echo 'Strong nested LOW drive failed to oppose external HIGH' >&2; exit 1; }
awk '$1>26.5e-9 && $1<27e-9 &&
     $2>3.05 && $4>3.05 && $6>3.2 && $8<.1 {ok=1}
     END {exit !ok}' "$work/result.dat" ||
    { echo 'Nested HIGH drive failed to overpower external LOW or reach sensor' >&2; exit 1; }
awk '$1>38.5e-9 && $1<39e-9 &&
     $2<.15 && $4<.15 && $6<.1 && $8<.1 {ok=1}
     END {exit !ok}' "$work/result.dat" ||
    { echo 'Nested driver/sensor failed to return LOW' >&2; exit 1; }
echo '1/1 ngspice nested directional ALB boundary simulation passes'
