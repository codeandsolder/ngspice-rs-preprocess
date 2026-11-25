#!/usr/bin/env bash
# Construct and simulate one original Elements ALB netlist via native
# Rust-backed XSPICE ADC + scheduler + DACs, preserving all output pins.
set -euo pipefail
if (( $# != 3 )); then
    printf 'Usage: %s ORIGINAL_VENDOR_NETLIST WORK_DIRECTORY ADC_TIME_TOL\n' "$0" >&2
    exit 2
fi
source="$(realpath -- "$1")"
work="$(realpath -- "$2")"
time_tol="$3"
name="$(basename "$source" .net)"
test -s "$source"
test -s "$work/.spiceinit"
for tool in awk od sed rg ngspice; do command -v "$tool" >/dev/null; done
body="$(awk '
    tolower($1)==".file" {active=1;next}
    tolower($1)==".endf" {active=0;next}
    active && NF {printf "%s ",$0}
' "$source")"
[[ -n "$body" ]] || { echo "Missing original ALB source: $name" >&2; exit 1; }
hex="$(printf %s "$body" | od -An -v -tx1 | tr -d ' \n')"
instance="$(rg -m 1 '^A[^ ]+ \[' "$source")"
fields="$(printf '%s\n' "$instance" | sed -nE 's/^A[^ ]+ \[([^]]+)\] \[([^]]+)\] ([^ ]+)$/\1|\2|\3/p')"
[[ -n "$fields" ]] || { echo "Invalid original ALB instance: $name" >&2; exit 1; }
IFS='|' read -r input_nodes output_nodes _ <<< "$fields"
in_digital=""
out_digital=""
out_sink=""
out_source=""
circuit="$work/circuit.cir"
{
    echo '* Complete original SIMetrix ALB body running via Rust/XSPICE'
    rg '^V[^ ]+ ' "$source"
    i=0
    for node in $input_nodes; do
        printf 'Aadc%d %%gd(%s 0) din%d dins%d dint%d ADC\n' "$i" "$node" "$i" "$i" "$i"
        in_digital="$in_digital din$i"
        i=$((i+1))
    done
    printf '.model ADC sr_family_adc(in_low=2.1 in_high=2.2 rise_delay=1p fall_delay=1p out_low=0 out_high=5 clamp_low=0 clamp_high=5 clamp_res=10 clamp_bias=.5 schmitt=true time_tol=%s)\n' "$time_tol"
    i=0
    for node in $output_nodes; do
        out_digital="$out_digital dout$i"
        out_sink="$out_sink sink$i"
        out_source="$out_source source$i"
        i=$((i+1))
    done
    printf '%s\n' "$i" > "$work/output-pin-count"
    printf 'Aalb [%s] [%s] 0 [%s] [%s] STATEMODEL\n' "$in_digital" "$out_digital" "$out_sink" "$out_source"
    printf '.model STATEMODEL sr_alb_stateful(source_hex="%s" reg_delay=1n port_delay=1n)\n' "$hex"
    i=0
    for node in $output_nodes; do
        printf 'Adac%d dout%d sink%d source%d %%gd(%s 0) DAC\n' "$i" "$i" "$i" "$i" "$node"
        i=$((i+1))
    done
    printf '.model DAC sr_family_dac(out_low=0 out_high=5 g_pullup=.024 g_pulldown=.034 g_resistive=.001 g_hiz=1e-9 knee_low=2 knee_high=2.75 v_smooth=.5 input_load=-31p t_rise=2n t_fall=2n sink_current=0 source_current=0)\n'
    rg '^Rout' "$source"
    rg '^\.tran ' "$source"
    printf '.control\nrun\nwrdata native.dat'
    for node in $output_nodes; do printf ' v(%s)' "$node"; done
    printf '\nquit\n.endc\n.end\n'
} > "$circuit"
(cd "$work" && ngspice -b circuit.cir > sim.log 2>&1) || {
    printf 'ngspice failed for %s:\n' "$name" >&2
    tail -n 70 "$work/sim.log" >&2
    exit 1
}
if rg -i 'error:|fatal error|timestep too small|no such model' "$work/sim.log"; then
    printf 'Unexpected XSPICE error for %s\n' "$name" >&2
    exit 1
fi
test -s "$work/native.dat" || { echo "Missing native waveform for $name" >&2; exit 1; }
