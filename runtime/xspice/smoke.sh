#!/usr/bin/env bash
set -euo pipefail
if (( $# != 2 )); then
    printf 'Usage: %s ABSOLUTE_MODEL_CM SMOKE_WORK_ROOT\n' "$0" >&2
    exit 2
fi
source_dir="$(cd -- "$(dirname -- "$0")" && pwd)"
model="$(realpath -- "$1")"
work_root="$(realpath -- "$2")"
if [[ ! -f "$model" ]]; then echo "Missing compiled model: $model" >&2; exit 1; fi
command -v ngspice >/dev/null || { echo "ngspice required" >&2; exit 1; }
command -v awk >/dev/null || { echo "awk required" >&2; exit 1; }
command -v rg >/dev/null || { echo "ripgrep required" >&2; exit 1; }

work="$(mktemp -d "$work_root/ngspice_preprocess_alb_smoke_XXXXXXXX")"
cleanup() { rm -rf -- "$work"; }
trap cleanup EXIT
cp -- "$source_dir"/smokes/*.cir "$work/"
printf 'codemodel %s\n' "$model" > "$work/.spiceinit"
cd "$work"
for name in startup short long loaded bundle-underlimit bundle-overload lut-and lut-xor lut-chain lut-asymmetric lut-tristate lut-startup lut-consensus lut-inertial-contention lut-vendor-unknown; do
    if ! ngspice -b "$name.cir" > "$name.log" 2>&1; then
        printf 'ngspice failed on %s:\n' "$name" >&2
        tail -n 60 "$name.log" >&2
        exit 1
    fi
    if rg -i 'fatal error|timestep too small|error:|no such model' "$name.log"; then
        printf 'Unexpected simulator error in %s\n' "$name" >&2
        exit 1
    fi
    if [[ -f out.dat ]]; then mv -- out.dat "$name.dat"; fi
    if [[ ! -s "$name.dat" ]]; then
        printf 'Missing transient data for %s\n' "$name" >&2
        exit 1
    fi
done

# ngspice wrdata emits two columns: time and voltage.
# These are prototype runtime invariants, not SIMetrix-equivalence claims.
awk '$1 > 5e-9 && $2 > .75 {pass=1} END {exit !pass}' startup.dat ||
    { echo "Startup high state did not arrive" >&2; exit 1; }
awk '$2 > .75 {bad=1} END {exit bad}' short.dat ||
    { echo "Inertial model passed a 2ns pulse" >&2; exit 1; }
awk '$2 > .75 {pass=1} END {exit !pass}' long.dat ||
    { echo "Inertial model swallowed an 8ns pulse" >&2; exit 1; }
awk '$1 > 10e-9 && $2 > .75 {pass=1} END {exit !pass}' loaded.dat ||
    { echo "Loaded transition did not arrive" >&2; exit 1; }
awk '$1 > 8e-9 && $1 < 12e-9 && $2 > .75 {pass=1} END {exit !pass}' bundle-underlimit.dat ||
    { echo "Under-limit driver failed to propagate high" >&2; exit 1; }
awk '$1 > 8e-9 && $1 < 12e-9 && $2 > .45 && $2 < .55 {pass=1} END {exit !pass}' bundle-overload.dat ||
    { echo "Overloaded driver did not emit UNKNOWN" >&2; exit 1; }

# Generated per-bit LUTs: exact known-input rows, inertial stage timing,
# and cascaded propagation (2ns + 3ns), not UNKNOWN-state equivalence.
awk '$1 > 4e-9 && $1 < 6e-9 && $2 > .75 {pass=1} END {exit !pass}' lut-and.dat ||
    { echo "Known-input AND LUT did not drive high" >&2; exit 1; }
awk '$1 > 4e-9 && $1 < 6e-9 && $2 < .25 {pass=1} END {exit !pass}' lut-xor.dat ||
    { echo "XOR LUT did not go low when both inputs were high" >&2; exit 1; }
awk '$1 > 8e-9 && $1 < 9e-9 && $2 > .75 {pass=1} END {exit !pass}' lut-xor.dat ||
    { echo "XOR LUT did not recover high after one input fell" >&2; exit 1; }
awk '$1 > 4e-9 && $1 < 5e-9 && $2 > .75 {pass=1} END {exit !pass}' lut-chain.dat ||
    { echo "Cascaded LUT output changed too early" >&2; exit 1; }
awk '$1 > 7e-9 && $1 < 8e-9 && $2 < .25 {pass=1} END {exit !pass}' lut-chain.dat ||
    { echo "Cascaded LUT 2ns+3ns propagation did not complete" >&2; exit 1; }
awk '$1 > 3e-9 && $1 < 4e-9 && $2 > .75 {pass=1} END {exit !pass}' lut-asymmetric.dat ||
    { echo "LUT source-bit row order is wrong" >&2; exit 1; }
awk '$1 > 7e-9 && $1 < 8e-9 && $2 < .25 {pass=1} END {exit !pass}' lut-asymmetric.dat ||
    { echo "Asymmetric LUT failed on both-inputs-high row" >&2; exit 1; }
awk '$1 > 4e-9 && $1 < 5e-9 && $2 > .45 && $2 < .55 {pass=1} END {exit !pass}' lut-tristate.dat ||
    { echo "Tri-state row did not become unknown at DAC" >&2; exit 1; }
awk '$1 > 10e-9 && $1 < 11e-9 && $2 < .25 {pass=1} END {exit !pass}' lut-tristate.dat ||
    { echo "Tri-state output did not resume driven-low" >&2; exit 1; }
awk '$1 > 4e-9 && $1 < 5e-9 && $2 > .75 {pass=1} END {exit !pass}' lut-startup.dat ||
    { echo "LUT startup did not evaluate constant DC input" >&2; exit 1; }
# SIMetrix 9.40d ALB expressions convert every UNKNOWN level to a binary
# zero regardless of drive strength (12-state physical vendor oracle).
# Check that 1 XOR X=1 and !X=1 rather than generic UNKNOWN, while output
# enable X/0 releases to the external pull-up and 1 drives LOW. The generic
# Boolean-consensus LUT must continue to retain UNKNOWN for 1 XOR X.
awk '$1 >= 4e-9 && $1 <= 5e-9 &&
     $2 < .2 && $4 > 4.8 && $6 > 4.8 &&
     $8 < .2 && $10 > 4.8 &&
     $12 > 4.95 && $14 > 4.95 && $16 < .2 &&
     $18 > 1.5 && $18 < 3.5 {pass=1}
     END {exit !pass}' lut-vendor-unknown.dat ||
    { echo "Vendor ALB UNKNOWN level coercion or output enable does not match" >&2; exit 1; }
# Native ternary consensus must keep known values when every compatible
# binary LUT row agrees. Disagreement remains UNKNOWN. The 0/Z mixture
# remains undetermined; unanimous Z releases the digital bus so a separate
# STRONG 1 driver wins. ngspice dac_bridge renders UNKNOWN near .5V.
awk '$1 > 4e-9 && $1 < 5e-9 &&
     $2 < .05 && $4 > .45 && $4 < .55 &&
     $6 > .45 && $6 < .55 && $8 < .05 &&
     $10 > .95 && $12 > .95 &&
     $14 > .45 && $14 < .55 && $16 > .95 {pass=1}
     END {exit !pass}' lut-consensus.dat ||
    { echo "UNKNOWN-input Boolean/drive consensus at a=0 is wrong" >&2; exit 1; }
awk '$1 > 11e-9 && $1 < 12e-9 &&
     $2 > .45 && $2 < .55 && $4 > .95 &&
     $6 > .45 && $6 < .55 && $8 < .05 &&
     $10 > .95 && $12 > .95 &&
     $14 > .45 && $14 < .55 && $16 > .95 {pass=1}
     END {exit !pass}' lut-consensus.dat ||
    { echo "UNKNOWN-input Boolean/drive consensus at a=1 is wrong" >&2; exit 1; }
awk -f "$source_dir/check-inertial-contention.awk" lut-inertial-contention.dat ||
    { echo "Inertial contention/transport transitions failed" >&2; exit 1; }
printf 'All 15 ngspice 47 ALB XSPICE smokes passed.\n'
