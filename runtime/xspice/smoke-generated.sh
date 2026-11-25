#!/usr/bin/env bash
set -euo pipefail
if (( $# != 3 )); then
    printf 'usage: %s XSPICE_CODE_MODEL GENERATED_INCLUDE_DIRECTORY SCRATCH_ROOT\n' "$0" >&2
    exit 2
fi
here="$(cd -- "$(dirname -- "$0")" && pwd)"
model="$(realpath -- "$1")"
generated="$(realpath -- "$2")"
root="$(realpath -- "$3")"
command -v ngspice >/dev/null
command -v awk >/dev/null
command -v rg >/dev/null
work="$(mktemp -d "$root/ngspice_preprocess_generated_XXXXXXXX")"
trap 'rm -rf -- "$work"' EXIT
printf 'codemodel %s\n' "$model" > "$work/.spiceinit"
for example in timed-enable timed-enable-family timed-enable-both-family constant bundle-unloaded bundle-underlimit bundle-overload bundle-overload-low bundle-multidriver; do
    cp -- "$here/fixtures/$example.cir" "$work/"
    if [[ $example == timed-enable ]]; then
        # A stock DAC sees the retained logic level, not the drive strength.
        # Check the actual event states without modifying the archived deck.
        sed -i '/^wrdata /a eprint dout' "$work/$example.cir"
    fi
    generated_name="$example"
    if [[ $example == timed-enable-family || $example == timed-enable-both-family ]]; then
        generated_name=timed-enable
    elif [[ $example == bundle-overload-low ]]; then
        generated_name=bundle-buffer-low
    elif [[ $example == bundle-* ]]; then
        generated_name=bundle-buffer
    fi
    cp -- "$generated/$generated_name.inc" "$work/generated.inc"
    (
        cd "$work"
        if ! ngspice -b "$example.cir" > "$example.log" 2>&1; then
            tail -n 60 "$example.log" >&2
            exit 1
        fi
        if rg -i 'fatal error|timestep too small|error:|no such model' "$example.log"; then
            echo "Simulator error in $example" >&2
            exit 1
        fi
        if [[ ! -s generated.dat ]]; then
            echo "Missing generated waveform in $example" >&2
            exit 1
        fi
        mv -- generated.dat "$example.dat"
    )
done
awk '$1 > 4.1e-9 && $1 < 6e-9 && $2 > .75 {high=1}
     END {exit !high}' "$work/timed-enable.dat" ||
    { echo "Generated stage output did not propagate" >&2; exit 1; }
# The compiler retains the last logical polarity during release. Require
# the complete startup/enable/delayed-AND/release event sequence, including
# the strength character; a stuck strongly driven high must fail.
awk '
    $2 ~ /^[01Uu][szru]$/ && $1 ~ /^[0-9.+-]+[eE][+-][0-9]+$/ {
        n++
        if (n==1 && !($1==0 && $2=="0z")) bad=1
        if (n==2 && !($1>1.49e-9 && $1<1.52e-9 && $2=="0s")) bad=1
        if (n==3 && !($1>3.99e-9 && $1<4.02e-9 && $2=="1s")) bad=1
        if (n==4 && !($1>7.99e-9 && $1<8.02e-9 && $2=="1z")) bad=1
    }
    END {exit !(n==4 && !bad)}
' "$work/timed-enable.log" ||
    { echo "Generated stage output event timing or drive strength is wrong" >&2; exit 1; }
awk '$1 > 4e-9 && $1 < 5e-9 && $2 > .75 {high=1} END {exit !high}' "$work/constant.dat" ||
    { echo "Generated constant-fanin stage failed to start or drive" >&2; exit 1; }
# The same Rust-generated ALB model is instantiated through a nested
# subcircuit boundary. Each child declares exactly one 2pF physical input.
# At 1k output resistance, 2 children should shift the first high edge
# by ~4ns; 3 children should exceed the +/-20mA limit and emit UNKNOWN.
awk '$1 > 4.5e-9 && $1 < 5e-9 && $2 > .75 {pass=1} END {exit !pass}' "$work/bundle-unloaded.dat" ||
    { echo "Unloaded nested bundle did not propagate by 5ns" >&2; exit 1; }
awk '$1 > 4.5e-9 && $1 < 7.5e-9 && $2 > .75 {bad=1} END {exit bad}' "$work/bundle-underlimit.dat" ||
    { echo "Nested bundle high transition arrived before load-dependent delay" >&2; exit 1; }
awk '$1 > 9e-9 && $1 < 10e-9 && $2 > .75 {pass=1} END {exit !pass}' "$work/bundle-underlimit.dat" ||
    { echo "Under-limit nested bundle did not propagate after load-dependent delay" >&2; exit 1; }
awk '$1 > 12e-9 && $1 < 13e-9 && $2 > .45 && $2 < .55 {pass=1} END {exit !pass}' "$work/bundle-overload.dat" ||
    { echo "Three physical input loads did not force UNKNOWN through hierarchy" >&2; exit 1; }
awk '$1 > 5e-9 && $1 < 7e-9 && $2 > .45 && $2 < .55 {pass=1} END {exit !pass}' "$work/bundle-multidriver.dat" ||
    { echo "Opposing strong ALB drivers did not resolve to UNKNOWN" >&2; exit 1; }
awk '$1 > 13e-9 && $1 < 14e-9 && $2 > .75 {pass=1} END {exit !pass}' "$work/bundle-multidriver.dat" ||
    { echo "ALB shared digital bundle did not recover HIGH when drivers agreed" >&2; exit 1; }
awk '$1 > 11e-9 && $1 < 13e-9 && $2 > .75 {pass=1} END {exit !pass}' "$work/bundle-overload-low.dat" ||
    { echo "High state incorrectly tripped the disabled source-current limit" >&2; exit 1; }
awk '$1 > 19e-9 && $1 < 20e-9 && $2 > .45 && $2 < .55 {pass=1} END {exit !pass}' "$work/bundle-overload-low.dat" ||
    { echo "Sink-current overload did not force UNKNOWN when low" >&2; exit 1; }
awk '$1 > 5e-9 && $1 < 6e-9 && $2 > 4.6 {high=1}
     $1 > 10e-9 && $1 < 11e-9 && $2 > 2.35 && $2 < 2.65 {z=1}
     END {exit !(high && z)}' "$work/timed-enable-family.dat" ||
    { echo "Compiled ALB driving nonlinear HC output did not respect STRONG/HI-Z analog loading" >&2; exit 1; }
awk '$1 > 5e-9 && $1 < 6e-9 && $2 > 4.6 {high=1}
     $1 > 10e-9 && $1 < 11e-9 && $2 > 2.35 && $2 < 2.65 {z=1}
     END {exit !(high && z)}' "$work/timed-enable-both-family.dat" ||
    { echo "Physical family ADC -> compiled ALB -> nonlinear HC DAC did not preserve timed logic" >&2; exit 1; }
echo "9/9 compiler-generated ngspice transient circuits passed"
