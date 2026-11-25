#!/usr/bin/env bash
# Original proprietary SIMetrix Elements 9.40d USER/DEVICE expression oracle:
# fractional truncation, negative unsigned wrap, dynamic nested indexing,
# model USER_SCALE and instance DEVICE independent parameter values.
set -euo pipefail
if (( $# != 2 )); then
    printf 'Usage: %s STATEFUL_CODEMODEL_CM SCRATCH_ROOT\n' "$0" >&2
    exit 2
fi
here="$(cd -- "$(dirname -- "$0")" && pwd)"
model="$(realpath -- "$1")"
root="$(realpath -- "$2")"
test -s "$model"
for tool in awk ngspice od rg tr; do command -v "$tool" >/dev/null; done
work="$(mktemp -d "$root/stateful-expressions-XXXXXXXX")"
trap 'rm -rf -- "$work"' EXIT
printf 'codemodel %s\n' "$model" > "$work/.spiceinit"
vendor="$here/fixtures/vendor-expr"
count=0
for fixture in "$vendor"/*.net; do
    name="$(basename "$fixture" .net)"
    case "$name" in
        user-fractional-predicate|user-fractional-direct) extras="user_values=[0.5]" ;;
        device-fractional-predicate) extras="device_values=[0.5]" ;;
        user-1p5-gt1) extras="user_values=[1.5]" ;;
        user-2p5-gt1) extras="user_values=[2.5]" ;;
        user-negative-eq|user-negative-unsigned|user-negative-less) extras="user_values=[-1.5]" ;;
        user-large-unsigned) extras="user_values=[4294967295]" ;;
        user-dynamic-index) extras="user_values=[1.5 2]" ;;
        user-scale-fraction) extras="user_values=[0.75] user_scale=3" ;;
        user-device-cross) extras="user_values=[1] device_values=[2]" ;;
        *) printf 'Unknown vendor expression case: %s\n' "$name" >&2; exit 2 ;;
    esac
    source="$(awk '
        { sub(/\r$/, "", $0) }
        toupper($1)==".FILE" {inside=1;next}
        toupper($1)==".ENDF" {inside=0;next}
        inside && NF {printf "%s ",$0}
    ' "$fixture")"
    test -n "$source"
    hex="$(printf %s "$source" | od -An -v -tx1 | tr -d ' \n')"
    {
        printf '* Vendor original-source ALB expression oracle %s\n' "$name"
        rg '^Vdata ' "$fixture"
        echo 'Aadc0 %gd(d 0) din0 dins0 dint0 ADC'
        echo '.model ADC sr_family_adc(in_low=2.1 in_high=2.2 rise_delay=1p fall_delay=1p out_low=0 out_high=5 clamp_low=0 clamp_high=5 clamp_res=10 clamp_bias=.5 schmitt=true time_tol=20p)'
        echo 'Aalb [din0] [dout0] 0 [sink0] [source0] STATE'
        printf '.model STATE sr_alb_stateful(source_hex="%s" reg_delay=1n port_delay=1n %s)\n' "$hex" "$extras"
        echo 'Adac0 dout0 sink0 source0 %gd(q 0) DAC'
        echo '.model DAC sr_family_dac(out_low=0 out_high=5 g_pullup=.024 g_pulldown=.034 g_resistive=.001 g_hiz=1e-9 knee_low=2 knee_high=2.75 v_smooth=.5 input_load=-31p t_rise=2n t_fall=2n sink_current=0 source_current=0)'
        echo 'Rout q 0 100k'
        rg '^\.tran ' "$fixture"
        printf '.control\nrun\nwrdata native.dat v(q)\nquit\n.endc\n.end\n'
    } > "$work/circuit.cir"
    if ! (cd "$work" && ngspice -b circuit.cir > sim.log 2>&1); then
        printf 'Failed source expression %s\n' "$name" >&2
        tail -n 70 "$work/sim.log" >&2
        exit 1
    fi
    if rg -i 'error:|fatal error|timestep too small|no such model' "$work/sim.log"; then
        printf 'Native XSPICE source expression error: %s\n' "$name" >&2
        exit 1
    fi
    awk -v name="$name" -v pins=1 -f "$here/check-stateful-extended-edges.awk" \
        "$vendor/vendor-q-2p5v-events.csv" \
        "$vendor/vendor-q-initial-voltages.csv" "$work/native.dat"
    count=$((count + 1))
done
printf '%s/%s original-source SIMetrix USER/DEVICE expression differential scenarios passed\n' "$count" "$count"
