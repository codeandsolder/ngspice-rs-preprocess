#!/usr/bin/env bash
# Differential replay of full original-source SIMetrix ALB USER/DEVICE decks,
# with original model-level USER and instance-level DEVICE parameter values.
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
work="$(mktemp -d "$root/stateful-parameter-XXXXXXXX")"
trap 'rm -rf -- "$work"' EXIT
printf 'codemodel %s\n' "$model" > "$work/.spiceinit"

vendor="$here/fixtures/vendor-params"
for name in user-scaled device-indexed user-device-combined device-delay-two-ns; do
    case "$name" in
        user-scaled)
            extra="user_values=[500p] user_scale=2"
            ;;
        device-indexed)
            extra="device_values=[1n 1]"
            ;;
        user-device-combined)
            extra="user_values=[250p] device_values=[1 1] user_scale=4"
            ;;
        device-delay-two-ns)
            extra="device_values=[2n 1]"
            ;;
    esac
    fixture="$vendor/$name.net"
    test -s "$fixture"
    source="$(awk '
        toupper($1)==".FILE" {inside=1;next}
        toupper($1)==".ENDF" {inside=0;next}
        inside && NF {printf "%s ",$0}
    ' "$fixture")"
    test -n "$source"
    hex="$(printf %s "$source" | od -An -v -tx1 | tr -d ' \n')"
    {
        printf '* Original-source SIMetrix USER/DEVICE differential probe: %s\n' "$name"
        rg '^Vdata ' "$fixture"
        echo 'Aadc0 %gd(d 0) din0 dins0 dint0 ADC'
        echo '.model ADC sr_family_adc(in_low=2.1 in_high=2.2 rise_delay=1p fall_delay=1p out_low=0 out_high=5 clamp_low=0 clamp_high=5 clamp_res=10 clamp_bias=.5 schmitt=true time_tol=20p)'
        echo 'Aalb [din0] [dout0] 0 [sink0] [source0] STATE'
        # ALB DEVICE is a per-instance vector. Native XSPICE parameters
        # belong to .MODEL, so a netlist generator must emit a unique model
        # for each distinct source/USER/DEVICE environment.
        printf '.model STATE sr_alb_stateful(source_hex="%s" reg_delay=1n port_delay=1n %s)\n' "$hex" "$extra"
        echo 'Adac0 dout0 sink0 source0 %gd(q 0) DAC'
        echo '.model DAC sr_family_dac(out_low=0 out_high=5 g_pullup=.024 g_pulldown=.034 g_resistive=.001 g_hiz=1e-9 knee_low=2 knee_high=2.75 v_smooth=.5 input_load=-31p t_rise=2n t_fall=2n sink_current=0 source_current=0)'
        echo 'Rout q 0 100k'
        rg '^\.tran ' "$fixture"
        printf '.control\nrun\nwrdata native.dat v(q)\nquit\n.endc\n.end\n'
    } > "$work/circuit.cir"
    (cd "$work" && ngspice -b circuit.cir > sim.log 2>&1) || {
        printf 'ngspice parameter model %s failed\n' "$name" >&2
        tail -n 80 "$work/sim.log" >&2
        exit 1
    }
    if rg -i 'error:|fatal error|timestep too small|no such model' "$work/sim.log"; then
        printf 'XSPICE parameter model %s returned a diagnostic\n' "$name" >&2
        exit 1
    fi
    awk -v name="$name" -v pins=1 \
        -f "$here/check-stateful-extended-edges.awk" \
        "$vendor/vendor-q-2p5v-events.csv" \
        "$vendor/vendor-q-initial-voltages.csv" "$work/native.dat"
done

# An absent instance-specific DEVICE parameter must fail closed rather than
# silently accepting XSPICE's optional vector default as DEVICE[0]=0.
source="$(awk '
    toupper($1)==".FILE" {inside=1;next}
    toupper($1)==".ENDF" {inside=0;next}
    inside && NF {printf "%s ",$0}
' "$vendor/device-indexed.net")"
hex="$(printf %s "$source" | od -An -v -tx1 | tr -d ' \n')"
{
    echo '* Missing DEVICE[0] must be diagnosed'
    echo 'Vdata d 0 0'
    echo 'Aadc0 %gd(d 0) din0 dins0 dint0 ADC'
    echo '.model ADC sr_family_adc(in_low=2.1 in_high=2.2 rise_delay=1p fall_delay=1p out_low=0 out_high=5 clamp_low=0 clamp_high=5 clamp_res=10 clamp_bias=.5 schmitt=true time_tol=20p)'
    echo 'Aalb [din0] [dout0] 0 [sink0] [source0] STATE'
    printf '.model STATE sr_alb_stateful(source_hex="%s")\n' "$hex"
    echo 'Adac0 dout0 sink0 source0 %gd(q 0) DAC'
    echo '.model DAC sr_family_dac(out_low=0 out_high=5 g_pullup=.024 g_pulldown=.034 g_resistive=.001 g_hiz=1e-9 knee_low=2 knee_high=2.75 v_smooth=.5 input_load=-31p t_rise=2n t_fall=2n sink_current=0 source_current=0)'
    echo 'Rout q 0 100k'
    printf '.tran 20p 2n 0 20p\n.control\nrun\nquit\n.endc\n.end\n'
} > "$work/missing.cir"
(cd "$work" && ngspice -b missing.cir > missing.log 2>&1) || true
if ! rg -q 'DEVICE\[0\] requires an instance parameter environment' "$work/missing.log"; then
    printf 'Missing DEVICE vector did not fail closed:\n' >&2
    tail -n 65 "$work/missing.log" >&2
    exit 1
fi
# The same vendor DEVICE source must maintain per-instance parameter
# isolation when two XSPICE models use different DEVICE values concurrently.
source="$(awk '
    toupper($1)==".FILE" {inside=1;next}
    toupper($1)==".ENDF" {inside=0;next}
    inside && NF {printf "%s ",$0}
' "$vendor/device-indexed.net")"
hex="$(printf %s "$source" | od -An -v -tx1 | tr -d ' \n')"
{
    echo '* Two original-source ALB DEVICE instances with independent XSPICE parameter environments'
    rg '^Vdata ' "$vendor/device-indexed.net"
    echo 'Aadc0 %gd(d 0) din0 dins0 dint0 ADC'
    echo '.model ADC sr_family_adc(in_low=2.1 in_high=2.2 rise_delay=1p fall_delay=1p out_low=0 out_high=5 clamp_low=0 clamp_high=5 clamp_res=10 clamp_bias=.5 schmitt=true time_tol=20p)'
    echo 'Aalb1 [din0] [dout1] 0 [sink1] [source1] STATE1'
    echo 'Aalb2 [din0] [dout2] 0 [sink2] [source2] STATE2'
    printf '.model STATE1 sr_alb_stateful(source_hex="%s" device_values=[1n 1])\n' "$hex"
    printf '.model STATE2 sr_alb_stateful(source_hex="%s" device_values=[2n 1])\n' "$hex"
    echo 'Adac1 dout1 sink1 source1 %gd(q1 0) DAC'
    echo 'Adac2 dout2 sink2 source2 %gd(q2 0) DAC'
    echo '.model DAC sr_family_dac(out_low=0 out_high=5 g_pullup=.024 g_pulldown=.034 g_resistive=.001 g_hiz=1e-9 knee_low=2 knee_high=2.75 v_smooth=.5 input_load=-31p t_rise=2n t_fall=2n sink_current=0 source_current=0)'
    echo 'Rout1 q1 0 100k'
    echo 'Rout2 q2 0 100k'
    rg '^\.tran ' "$vendor/device-indexed.net"
    printf '.control\nrun\nwrdata fast.dat v(q1)\nwrdata slow.dat v(q2)\nquit\n.endc\n.end\n'
} > "$work/isolated.cir"
(cd "$work" && ngspice -b isolated.cir > isolated.log 2>&1) || {
    tail -n 80 "$work/isolated.log" >&2
    exit 1
}
if rg -i 'error:|fatal error|timestep too small|no such model' "$work/isolated.log"; then
    printf 'Isolated parameter model emitted error\n' >&2
    exit 1
fi
awk -v name=device-indexed -v pins=1 -f "$here/check-stateful-extended-edges.awk" \
    "$vendor/vendor-q-2p5v-events.csv" "$vendor/vendor-q-initial-voltages.csv" "$work/fast.dat"
awk -v name=device-delay-two-ns -v pins=1 -f "$here/check-stateful-extended-edges.awk" \
    "$vendor/vendor-q-2p5v-events.csv" "$vendor/vendor-q-initial-voltages.csv" "$work/slow.dat"

# The optional USER numeric vector must also fail closed when omitted.
source="$(awk '
    toupper($1)==".FILE" {inside=1;next}
    toupper($1)==".ENDF" {inside=0;next}
    inside && NF {printf "%s ",$0}
' "$vendor/user-scaled.net")"
hex="$(printf %s "$source" | od -An -v -tx1 | tr -d ' \n')"
{
    echo '* Missing USER[0] must be diagnosed'
    echo 'Vdata d 0 0'
    echo 'Aadc0 %gd(d 0) din0 dins0 dint0 ADC'
    echo '.model ADC sr_family_adc(in_low=2.1 in_high=2.2 rise_delay=1p fall_delay=1p out_low=0 out_high=5 clamp_low=0 clamp_high=5 clamp_res=10 clamp_bias=.5 schmitt=true time_tol=20p)'
    echo 'Aalb [din0] [dout0] 0 [sink0] [source0] STATE'
    printf '.model STATE sr_alb_stateful(source_hex="%s")\n' "$hex"
    echo 'Adac0 dout0 sink0 source0 %gd(q 0) DAC'
    echo '.model DAC sr_family_dac(out_low=0 out_high=5 g_pullup=.024 g_pulldown=.034 g_resistive=.001 g_hiz=1e-9 knee_low=2 knee_high=2.75 v_smooth=.5 input_load=-31p t_rise=2n t_fall=2n sink_current=0 source_current=0)'
    echo 'Rout q 0 100k'
    printf '.tran 20p 2n 0 20p\n.control\nrun\nquit\n.endc\n.end\n'
} > "$work/missing-user.cir"
(cd "$work" && ngspice -b missing-user.cir > missing-user.log 2>&1) || true
if ! rg -q 'USER\[0\] is out of range' "$work/missing-user.log"; then
    printf 'Missing USER vector did not fail closed:\n' >&2
    tail -n 65 "$work/missing-user.log" >&2
    exit 1
fi
printf '4/4 original-source parameterized SIMetrix circuits, 2 simultaneous independent DEVICE models and absent USER/DEVICE fail-closed checks passed\n'
