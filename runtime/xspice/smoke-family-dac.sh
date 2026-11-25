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
work="$(mktemp -d "$scratch/ngspice_preprocess_family_dac_XXXXXXXX")"
trap 'rm -rf -- "$work"' EXIT
cp -- "$here"/fixtures/family-dac/*.cir "$work/"
printf 'codemodel %s\n' "$model" > "$work/.spiceinit"
cd "$work"
for circuit in hc-low hc-high hc-resistive hc-hiz hc-ac-linear hc-ac-knee hc-ac-plateau hc-transient hc-interrupted ls-low ls-high bundle-overload bundle-underlimit; do
    if ! ngspice -b "$circuit.cir" >"$circuit.log" 2>&1; then
        tail -n 70 "$circuit.log" >&2
        exit 1
    fi
    if rg -i 'fatal error|timestep too small|error:|no such model' "$circuit.log"; then
        echo "Native family-DAC simulation error in $circuit" >&2
        exit 1
    fi
    [[ -s out.dat ]] || { echo "Missing $circuit output" >&2; exit 1; }
    mv -- out.dat "$circuit.dat"
done

# The .DC traces contain Vtest in column one and i(Vtest) in column two;
# the latter is negative when the device sinks current at the output node.
check_dc() {
    local fixture="$1" sample="$2" expected="$3" tolerance="$4"
    awk -v sample="$sample" -v expected="$expected" -v tolerance="$tolerance" '
      ($1-sample < 1e-9 && $1-sample > -1e-9) {
          deviation=$2-expected; if (deviation<0) deviation=-deviation
          if (deviation<=tolerance) pass=1
      }
      END {exit !pass}
    ' "$fixture.dat" || {
        printf 'Unexpected family DAC I-V: %s at V=%s expected I(Vtest)=%s\n' "$fixture" "$sample" "$expected" >&2
        exit 1
    }
}
# HC: 34 mS pulldown, 24 mS pullup, 2 V and 2.75 V knees.
# In the quadratic smoothing band around K, I(K)=g*(K-Vsmooth/4).
check_dc hc-low 1.5 -.051 1e-6
check_dc hc-low 2 -.06375 1e-6
check_dc hc-low 2.5 -.068 1e-6
check_dc hc-low 5 -.068 1e-6
check_dc hc-high 0 .054 1e-6
check_dc hc-high 2.75 .051 1e-6
check_dc hc-high 3.25 .042 1e-6
check_dc hc-high 4 .024 1e-6
check_dc hc-high 5 0 1e-7
# LS: 50 mS pull-down, 5.5 mS pull-up, no smoothing band.
check_dc ls-low .5 -.025 1e-6
check_dc ls-low 1.5 -.0325 1e-6
check_dc ls-high 0 .01925 1e-6
check_dc ls-high 3 .00275 1e-6
check_dc ls-high 3.5 0 1e-7
# Resistive drive is a plain 1 kΩ path to the nominal logic voltage,
# independent of the STRONG output-stage nonlinearity.
check_dc hc-resistive 0 .005 1e-6
check_dc hc-resistive 3 .002 1e-6
check_dc hc-resistive 5 0 1e-7

# A native AC sweep of the same differential conductance checks the
# analytic Jacobian on the ohmic section, inside the quadratic knee
# and on the flat current-limited plateau. wrdata columns are f, re(I), im(I).
awk '$1>999 && $1<1001 && $2>-.034001 && $2<-.033999 && $3<1e-8 && $3> -1e-8 {pass=1} END {exit !pass}' hc-ac-linear.dat ||
    { echo 'HC AC linear-region derivative is not 34mS' >&2; exit 1; }
awk '$1>999 && $1<1001 && $2>-.017001 && $2<-.016999 && $3<1e-8 && $3> -1e-8 {pass=1} END {exit !pass}' hc-ac-knee.dat ||
    { echo 'HC AC quadratic-knee derivative is not 17mS' >&2; exit 1; }
awk '$1>999 && $1<1001 && $2<1e-6 && $2> -1e-6 && $3<1e-8 && $3> -1e-8 {pass=1} END {exit !pass}' hc-ac-plateau.dat ||
    { echo 'HC AC saturated-current derivative is not zero' >&2; exit 1; }

# HI-Z is 100 MΩ to the midpoint of out_low and out_high. A d_tristate
# source must present HI_IMPEDANCE strength, not UNKNOWN/UNDETERMINED.
awk '$1>3e-9 && $1<4e-9 && $2>2.49e-8 && $2<2.51e-8 {pass=1} END {exit !pass}' hc-hiz.dat ||
    { echo 'HI-Z state did not behave as a 100 MOhm midpoint resistor' >&2; exit 1; }

# Mixed-signal time behavior: non-instantaneous 2 ns rails, 10 kΩ load.
awk '$1>4.45e-9 && $1<4.55e-9 && $2>.3 && $2<1.2 {pass=1} END {exit !pass}' hc-transient.dat ||
    { echo 'HC analog rise does not observe configured slew' >&2; exit 1; }
awk '$1>6.5e-9 && $1<7e-9 && $2>4.9 {pass=1} END {exit !pass}' hc-transient.dat ||
    { echo 'HC output did not reach high rail' >&2; exit 1; }
awk '$1>13.5e-9 && $1<14e-9 && $2<.1 {pass=1} END {exit !pass}' hc-transient.dat ||
    { echo 'HC output did not return to low rail' >&2; exit 1; }

# Two interruptions before a slew completes: the transition uses a
# weighted physical I-V basis, rather than a false jump to either rail.
awk '
  $1>4.7e-9 && $1<4.9e-9 && $2>.5 && $2<1.3 {mid=1}
  $1>5.7e-9 && $1<5.9e-9 && $2<.1 {low=1}
  $1>8.5e-9 && $1<9.5e-9 && $2>4.9 {high=1}
  $1>4.7e-9 && $1<6.2e-9 && prev_t>0 &&
  ($2-prev_v>.1 || prev_v-$2>.1) && $1-prev_t<1e-11 {jump=1}
  {prev_t=$1; prev_v=$2}
  END {exit !(mid && low && high && !jump)}
' hc-interrupted.dat ||
    { echo 'Interrupted HC slew lost continuity or correct recovery' >&2; exit 1; }

# The DAC itself is a *physical digital input load* on all three nets;
# high input current > the upstream driver's max_source resolves UNKNOWN.
awk '$1>10e-9 && $1<11e-9 && $2>4.9 {pass=1} END {exit !pass}' bundle-underlimit.dat ||
    { echo 'Under-limit DAC bundle did not preserve HIGH' >&2; exit 1; }
awk '$1>10e-9 && $1<11e-9 && $2>1 && $2<3 {pass=1} END {exit !pass}' bundle-overload.dat ||
    { echo 'Overloaded physical DAC bundle did not resolve UNKNOWN' >&2; exit 1; }

printf 'All 13 experimental nonlinear family-DAC native smokes passed.\n'
