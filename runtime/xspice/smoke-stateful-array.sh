#!/usr/bin/env bash
set -euo pipefail
if (( $# != 2 )); then
    printf 'Usage: %s STATEFUL_CODEMODEL_CM SCRATCH_ROOT\n' "$0" >&2
    exit 2
fi
here="$(cd -- "$(dirname -- "$0")" && pwd)"
model="$(realpath -- "$1")"
root="$(realpath -- "$2")"
test -s "$model"
for tool in awk od rg ngspice; do command -v "$tool" >/dev/null; done
work="$(mktemp -d "$root/stateful-array-XXXXXXXX")"
trap 'rm -rf -- "$work"' EXIT
printf 'codemodel %s\n' "$model" > "$work/.spiceinit"

for bitwise in 0 1; do
    hex="$(sed "s/BITWISE=1/BITWISE=$bitwise/" "$here/fixtures/stateful-array.alb" | od -An -v -tx1 | tr -d ' \n')"
    cat > "$work/circuit.cir" <<SPICE
* Native two-word, two-bit EDGE array, per-bit versus per-word HOLD cancellation
Vd0 d0 0 PWL(0 0 5n 0 5.1n 5 10.35n 5 10.45n 0 38n 0)
Vd1 d1 0 PWL(0 0 5n 0 5.1n 5 19.95n 5 20.05n 0 38n 0)
Vclock ck 0 PWL(0 0 9.95n 0 10.05n 5 14.95n 5 15.05n 0 23.95n 0 24.05n 5 28.95n 5 29.05n 0 38n 0)
Aadc0 %gd(d0 0) din0 sinkin0 sourcein0 ADC
Aadc1 %gd(d1 0) din1 sinkin1 sourcein1 ADC
Aadc2 %gd(ck 0) din2 sinkin2 sourcein2 ADC
.model ADC sr_family_adc(in_low=2.1 in_high=2.2 rise_delay=1p fall_delay=1p out_low=0 out_high=5 clamp_low=0 clamp_high=5 clamp_res=10 clamp_bias=.5 schmitt=true time_tol=20p)
Aalb [din0 din1 din2] [dout0 dout1 dout2 dout3] 0 [sink0 sink1 sink2 sink3] [source0 source1 source2 source3] STATEMODEL
.model STATEMODEL sr_alb_stateful(source_hex="$hex" reg_delay=1n port_delay=1n)
Adac0 dout0 sink0 source0 %gd(q0 0) DAC
Adac1 dout1 sink1 source1 %gd(q1 0) DAC
Adac2 dout2 sink2 source2 %gd(q2 0) DAC
Adac3 dout3 sink3 source3 %gd(q3 0) DAC
.model DAC sr_family_dac(out_low=0 out_high=5 g_pullup=.024 g_pulldown=.034 g_resistive=.001 g_hiz=1e-9 knee_low=2 knee_high=2.75 v_smooth=.5 input_load=-31p t_rise=2n t_fall=2n sink_current=0 source_current=0)
Rout0 q0 0 100k
Rout1 q1 0 100k
Rout2 q2 0 100k
Rout3 q3 0 100k
.tran 20p 38n 0 20p
.control
run
wrdata native.dat v(q0) v(q1) v(q2) v(q3)
quit
.endc
.end
SPICE
    (cd "$work" && ngspice -b circuit.cir > "sim-$bitwise.log" 2>&1) || {
        tail -n 80 "$work/sim-$bitwise.log" >&2
        exit 1
    }
    if rg -i 'error:|fatal error|timestep too small|no such model' "$work/sim-$bitwise.log"; then
        printf 'Unexpected native XSPICE error (BITWISE=%s)\n' "$bitwise" >&2
        exit 1
    fi
    awk -v bitwise="$bitwise" '
        function abs(x) { return x < 0 ? -x : x }
        BEGIN {
            sample_time[1] = 4e-9;
            sample_time[2] = 14e-9;
            sample_time[3] = 32e-9;
            for (pin = 0; pin < 4; ++pin) {
                expected[1, pin] = pin >= 2 ? 5 : 0;
                expected[3, pin] = pin >= 2 ? 5 : 0;
                expected[2, pin] = pin >= 2 ? 5 : 0;
            }
            expected[2, 1] = 5;
            expected[2, 3] = 0;
            n = 3; sample = 1;
        }
        sample <= n && ($1 + 0) >= sample_time[sample] {
            for (pin = 0; pin < 4; ++pin) {
                actual = $(2 * (pin + 1));
                goal = expected[sample, pin];
                if (abs(actual - goal) > 0.75) {
                    printf "BITWISE=%d sample %d at %.3f ns Q%d=%.4f V, expected %.1f V\n",
                        bitwise, sample, ($1 + 0) * 1e9, pin, actual, goal > "/dev/stderr";
                    failed = 1;
                }
            }
            printf "BITWISE=%d sample %d checked at %.3f ns\n",
                bitwise, sample, ($1 + 0) * 1e9;
            sample++;
        }
        END { if (sample <= n || failed) exit 1 }
    ' "$work/native.dat"
done
echo 'Native two-word EDGE array: BITWISE=0/1, HOLD, all four physical DACs passed'
