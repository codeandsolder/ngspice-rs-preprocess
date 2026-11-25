#!/usr/bin/env bash
# Direct native ngspice replay of the same original three-instance
# Elements 9.40d COMB model: two distinct DEVICE arrays and one duplicate.
set -euo pipefail
if (( $# != 2 )); then
    printf 'Usage: %s STATEFUL_CODEMODEL_CM SCRATCH_ROOT\n' "$0" >&2
    exit 2
fi
here="$(cd -- "$(dirname -- "$0")" && pwd)"
model="$(realpath -- "$1")"
root="$(realpath -- "$2")"
test -s "$model"
for cmd in awk ngspice od rg tr; do command -v "$cmd" >/dev/null; done
work="$(mktemp -d "$root/stateful-device-multi-XXXXXXXX")"
trap 'rm -rf -- "$work"' EXIT
printf 'codemodel %s\n' "$model" > "$work/.spiceinit"
vendor="$here/fixtures/vendor-device-multi"
fixture="$vendor/comb-three-instances.net"
source="$(awk '
    { sub(/\r$/, "", $0) }
    toupper($1)==".FILE" {inside=1;next}
    toupper($1)==".ENDF" {inside=0;next}
    inside && NF {printf "%s ",$0}
' "$fixture")"
test -n "$source"
hex="$(printf %s "$source" | od -An -v -tx1 | tr -d ' \n')"
{
    echo '* Three original-source SIMetrix ALB A devices, distinct DEVICE values'
    rg '^Vdata ' "$fixture"
    echo 'Aadc0 %gd(d 0) din0 dins0 dint0 ADC'
    echo '.model ADC sr_family_adc(in_low=2.1 in_high=2.2 rise_delay=1p fall_delay=1p out_low=0 out_high=5 clamp_low=0 clamp_high=5 clamp_res=10 clamp_bias=.5 schmitt=true time_tol=20p)'
    echo 'Aalb1 [din0] [dout1] 0 [sink1] [source1] STATEON'
    echo 'Aalb2 [din0] [dout2] 0 [sink2] [source2] STATEOFF'
    echo 'Aalb3 [din0] [dout3] 0 [sink3] [source3] STATEON'
    printf '.model STATEON sr_alb_stateful(source_hex="%s" reg_delay=1n port_delay=1n user_values=[0.75] user_scale=3 device_values=[1 1n])\n' "$hex"
    printf '.model STATEOFF sr_alb_stateful(source_hex="%s" reg_delay=1n port_delay=1n user_values=[0.75] user_scale=3 device_values=[0 2n])\n' "$hex"
    for n in 1 2 3; do
        printf 'Adac%s dout%s sink%s source%s %%gd(q%s 0) DAC\n' "$n" "$n" "$n" "$n" "$n"
        printf 'Rout%s q%s 0 100k\n' "$n" "$n"
    done
    echo '.model DAC sr_family_dac(out_low=0 out_high=5 g_pullup=.024 g_pulldown=.034 g_resistive=.001 g_hiz=1e-9 knee_low=2 knee_high=2.75 v_smooth=.5 input_load=-31p t_rise=2n t_fall=2n sink_current=0 source_current=0)'
    rg '^\.tran ' "$fixture"
    printf '.control\nrun\n'
    for n in 1 2 3; do
        printf 'wrdata q%s.dat v(q%s)\n' "$n" "$n"
    done
    printf 'quit\n.endc\n.end\n'
} > "$work/circuit.cir"
(cd "$work" && ngspice -b circuit.cir >sim.log 2>&1) || {
    tail -n 75 "$work/sim.log" >&2
    exit 1
}
if rg -i 'error:|fatal error|timestep too small|no such model' "$work/sim.log"; then
    echo 'Native three-device COMB replay returned simulator errors' >&2
    exit 1
fi
for n in 1 2 3; do
    awk -v name="device-multi-q$n" -v pins=1 \
        -f "$here/check-stateful-extended-edges.awk" \
        "$vendor/vendor-q-2p5v-events.csv" \
        "$vendor/vendor-q-initial-voltages.csv" "$work/q$n.dat"
done
echo '3/3 simultaneous proprietary three-DEVICE source outputs, 4 analog crossings and 3 startup voltages passed'
