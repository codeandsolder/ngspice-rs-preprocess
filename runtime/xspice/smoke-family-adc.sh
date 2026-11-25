#!/usr/bin/env bash
set -euo pipefail
if (( $# != 2 )); then
    printf 'Usage: %s ABSOLUTE_MODEL_CM SCRATCH_ROOT\n' "$0" >&2
    exit 2
fi
here="$(cd -- "$(dirname -- "$0")" && pwd)"
model="$(realpath -- "$1")"
scratch="$(realpath -- "$2")"
[[ -f $model ]] || { echo "Missing native model: $model" >&2; exit 1; }
command -v ngspice >/dev/null
command -v awk >/dev/null
command -v rg >/dev/null
work="$(mktemp -d "$scratch/ngspice_preprocess_family_adc_XXXXXXXX")"
trap 'rm -rf -- "$work"' EXIT
cp -- "$here"/fixtures/family-adc/*.cir "$work/"
printf 'codemodel %s\n' "$model" > "$work/.spiceinit"
cd "$work"
for circuit in adc-hc-ac adc-hc-dc adc-hc-rc adc-hc-threshold adc-hc-schmitt adc-ls-clamp adc-time-tol-coarse adc-time-tol-refined adc-time-tol-schmitt; do
    if ! ngspice -b "$circuit.cir" > "$circuit.log" 2>&1; then
        tail -n 70 "$circuit.log" >&2
        exit 1
    fi
    if rg -i 'fatal error|timestep too small|error:|no such model' "$circuit.log"; then
        printf 'ngspice ADC error in %s\n' "$circuit" >&2
        exit 1
    fi
    [[ -s out.dat ]] || { echo "Missing output for $circuit" >&2; exit 1; }
    mv -- out.dat "$circuit.dat"
done

# Clamped ADC with 0..5V rails; low/high static digital input currents
# are -10mA and +20mA, so G = (20m-(-10m))/5V = 6mS.
# Source measures negative of analog-bridge input current.
awk '
  ($1>-.000001 && $1<.000001) && $2>.00999 && $2<.01001 {low=1}
  ($1>4.999999 && $1<5.000001) && $2>-.02001 && $2<-.01999 {high=1}
  ($1>-.500001 && $1<-.499999) && $2>.0179 && $2<.0181 {lowclamp=1}
  ($1>5.499999 && $1<5.500001) && $2>-.0281 && $2<-.0279 {highclamp=1}
  END {exit !(low && high && lowclamp && highclamp)}
' adc-hc-dc.dat ||
    { echo 'ADC reflected current or nonlinear clamp I-V incorrect' >&2; exit 1; }
# LS upper clamp is at 15V (no usual upper rail diode).
awk '
  $1>3.49999 && $1<3.50001 && $2>-.0000001 && $2<.0000001 {logic_high=1}
  $1>14.99999 && $1<15.00001 && $2>-.0000001 && $2<.0000001 {unclamped=1}
  $1>16.99999 && $1<17.00001 && $2<-.439 && $2>-.441 {clamped=1}
  END {exit !(logic_high && unclamped && clamped)}
' adc-ls-clamp.dat ||
    { echo 'LS high clamp incorrectly tied to 3.5V logic rail' >&2; exit 1; }
# AC: G = 6mS and C = 2nF on the same port, at f=1 kHz.
# i(Vtest) is negative of the device current, hence negative values.
awk '$1>999 && $1<1001 && $2>-.006001 && $2<-.005999 &&
     $3>-.000012568 && $3<-.000012564 {pass=1} END {exit !pass}' adc-hc-ac.dat ||
    { echo 'ADC AC static or dynamic load admittance wrong' >&2; exit 1; }
# 1kΩ * 2pF physical digital load = 2ns RC; after 2ns (t=3ns),
# expected 5*(1-exp(-1))=3.16V. Test response plus rail settling.
awk '$1>2.995e-9 && $1<3.005e-9 && $2>3.10 && $2<3.22 {rc=1}
     $1>7.95e-9 && $1<8.05e-9 && $2>4.80 && $2<4.90 {settled=1}
     END {exit !(rc && settled)}' adc-hc-rc.dat ||
    { echo 'Reflected physical input capacitor failed RC transient' >&2; exit 1; }
# HC non-hysteretic thresholds with separate 2ns/3ns output delays.
awk '$1>49e-9 && $1<51e-9 && $2<.1 {low=1}
     $1>55e-9 && $1<56e-9 && $2>.45 && $2<.55 {unknown=1}
     $1>58e-9 && $1<60e-9 && $2>.9 {high=1}
     $1>179e-9 && $1<181e-9 && $2>.45 && $2<.55 {fall_unknown=1}
     $1>188e-9 && $1<190e-9 && $2<.1 {fall=1}
     END {exit !(low && unknown && high && fall_unknown && fall)}' adc-hc-threshold.dat ||
    { echo 'ADC thresholds/delays/UNKNOWN band incorrect' >&2; exit 1; }
# Hysteresis holds previous digital state in the threshold band;
# startup is controlled explicitly by IC rather than inferred.
awk '$1>1e-9 && $1<2e-9 && $2<.1 {start=1}
     $1>11e-9 && $1<12e-9 && $2>.9 {high=1}
     $1>15e-9 && $1<16e-9 && $2>.9 {heldhigh=1}
     $1>21e-9 && $1<22e-9 && $2<.1 {low=1}
     $1>26e-9 && $1<27e-9 && $2<.1 {heldlow=1}
     END {exit !(start && high && heldhigh && low && heldlow)}' adc-hc-schmitt.dat ||
    { echo 'ADC Schmitt hysteresis/startup state incorrect' >&2; exit 1; }
# A coarse analog timestep (200ns) misses two narrow UNKNOWN-band
# crossings unless the native bridge requests rollback to the threshold.
# The ramp crosses 2.1/2.2 V at .42/.44us ascending and
# 2.2/2.1 V at 2.56/2.58us descending. 1ps digital/analog delays
# and simulator timepoints can account for up to tens of picoseconds.
awk '
  $1>4.1999e-7 && $1<4.2002e-7 && $2>.45 && $2<.55 {rising_unknown=1}
  $1>4.3999e-7 && $1<4.4002e-7 && $2>.95 {rising_high=1}
  $1>2.55999e-6 && $1<2.56002e-6 && $2>.45 && $2<.55 {falling_unknown=1}
  $1>2.57999e-6 && $1<2.58002e-6 && $2<.05 {falling_low=1}
  END {exit !(rising_unknown && rising_high && falling_unknown && falling_low)}
' adc-time-tol-refined.dat ||
    { echo 'ADC 10ps threshold refinement missed coarse-step edges' >&2; exit 1; }
# 10us tolerance intentionally leaves the same poorly sampled source
# unrefined; verify no spurious early edge due to threshold snapping.
awk '
  $2>.95 && !high {high=$1}
  $1>2e-6 && $2<.05 && !low {low=$1}
  END {exit !(high>5.5e-7 && high<7.5e-7 && low>2.65e-6)}
' adc-time-tol-coarse.dat ||
    { echo 'Large TIME_TOL unexpectedly changed coarse ADC behavior' >&2; exit 1; }
# Schmitt bridge tracks only its active hysteresis threshold; it never
# emits UNKNOWN while crossing the dead band.
awk '
  $1>4.3999e-7 && $1<4.4002e-7 && $2>.95 {rise=1}
  $1>2.57999e-6 && $1<2.58002e-6 && $2<.05 {fall=1}
  $2>.05 && $2<.95 {unknown=1}
  END {exit !(rise && fall && !unknown)}
' adc-time-tol-schmitt.dat ||
    { echo 'Schmitt threshold rollback or hold-state correctness failed' >&2; exit 1; }
echo 'All 9 experimental family-ADC native smokes passed.'
