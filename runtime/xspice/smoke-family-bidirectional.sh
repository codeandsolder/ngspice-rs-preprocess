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
work="$(mktemp -d "$scratch/ngspice_preprocess_bidir_XXXXXXXX")"
trap 'rm -rf -- "$work"' EXIT
"$bins/alb-logical-expand" \
    "$here/fixtures/family/bidirectional-port.cir" \
    "$here/fixtures/family/bidirectional-port.json" \
    "$work/lowered.cir" "$work/generated.inc"
"$bins/alb-family-autowire" \
    "$work/lowered.cir" "$work/generated.inc" \
    "$here/fixtures/family/bidirectional-port.json" \
    "$here/fixtures/family/auto-models.json" "$work/circuit.cir"

rg -q '^\.subckt TOP PIN OE MONITOR$' "$work/circuit.cir"
rg -q '^Xone PIN OE OBSERVE TOP$' "$work/circuit.cir"
rg -q '^XSR_A1 D_SENSE D_SENSE_SR_SINK D_SENSE_SR_SOURCE D_OE .* D_DRIVE D_DRIVE_SR_SINK D_DRIVE_SR_SOURCE D_MON ' "$work/circuit.cir"
rg -q '^AsrFamily[[:digit:]]+ %gd\(PIN 0\) D_SENSE D_SENSE_SR_SINK D_SENSE_SR_SOURCE' "$work/circuit.cir"
rg -q '^AsrFamily[[:digit:]]+ D_DRIVE D_DRIVE_SR_SINK D_DRIVE_SR_SOURCE %gd\(PIN 0\)' "$work/circuit.cir"
[[ $(rg -F -c 'sr_family_adc(' "$work/circuit.cir") == 2 ]] ||
    { echo 'Expected one PIN ADC and one OE ADC' >&2; exit 1; }
[[ $(rg -F -c 'sr_family_dac(' "$work/circuit.cir") == 2 ]] ||
    { echo 'Expected one PIN DAC and one monitor DAC' >&2; exit 1; }
[[ $(rg -F -c 'sr_bundle_load(' "$work/generated.inc") == 1 ]] ||
    { echo 'The two ALB input pins must share one native load model' >&2; exit 1; }
[[ $(rg -c '^Asr_load[[:digit:]]+' "$work/generated.inc") == 2 ]] ||
    { echo 'Every physical ALB input contributes exactly one load' >&2; exit 1; }

printf 'codemodel %s\n' "$cm" > "$work/.spiceinit"
(
    cd "$work"
    ngspice -b circuit.cir >sim.log 2>&1 || { tail -n 115 sim.log >&2; exit 1; }
    if rg -i 'fatal error|timestep too small|error:|no such model' sim.log; then
        echo 'Bidirectional mixed-pin native simulation failed' >&2; exit 1
    fi
    test -s result.dat
)
# Source and result columns: TIME, PIN, TIME, MONITOR, TIME, OE, TIME, EXT.
# The physical ADC/DAC and compiled ALB both have real dynamic delays.
awk '$1>5e-9 && $1<5.2e-9 && $2<.16 && $4<.1 && $6<.1 {ok=1}
     END {exit !ok}' "$work/result.dat" ||
    { echo 'Released bus with external source LOW is not LOW' >&2; exit 1; }
awk '$1>13.5e-9 && $1<13.8e-9 && $2>3.1 && $4>3.1 &&
     $6<.1 && $8>3.2 {ok=1} END {exit !ok}' "$work/result.dat" ||
    { echo 'Released bus failed to sense external HIGH' >&2; exit 1; }
awk '$1>26.5e-9 && $1<27e-9 &&
     $2>3.13 && $2<3.23 && $4>3.1 && $6>3.2 && $8<.1 {ok=1}
     END {exit !ok}' "$work/result.dat" ||
    { echo 'Enabled native DAC failed to overpower 1k external LOW' >&2; exit 1; }
awk '$1>38.5e-9 && $1<39e-9 &&
     $2<.09 && $4<.1 && $6<.1 && $8<.1 {ok=1}
     END {exit !ok}' "$work/result.dat" ||
    { echo 'Released bus did not recover external LOW after drive' >&2; exit 1; }
echo '1/1 native ALB bidirectional pin (external sensing, strong drive, release) passes'
