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
hex="$(od -An -v -tx1 "$here/fixtures/stateful-mixed.alb" | tr -d ' \n')"
work="$(mktemp -d "$root/stateful-mixed-XXXXXXXX")"
trap 'rm -rf -- "$work"' EXIT
printf 'codemodel %s\n' "$model" > "$work/.spiceinit"
cat > "$work/circuit.cir" <<SPICE
* Synthetic two-bit COMB -> EDGE -> LEVEL physical regression
Vd0 d0 0 PWL(0 0 5n 0 5.1n 5 20n 5 20.1n 0 42n 0)
Vd1 d1 0 PWL(0 0 20n 0 20.1n 5 42n 5)
Vclock ck 0 PWL(0 0 9.9n 0 10n 5 12n 5 12.1n 0 25.9n 0 26n 5 28n 5 28.1n 0 42n 0)
Venable en 0 PWL(0 0 8.9n 0 9n 5 42n 5)
Aadc0 %gd(d0 0) din0 sinkin0 sourcein0 ADC
Aadc1 %gd(d1 0) din1 sinkin1 sourcein1 ADC
Aadc2 %gd(ck 0) din2 sinkin2 sourcein2 ADC
Aadc3 %gd(en 0) din3 sinkin3 sourcein3 ADC
.model ADC sr_family_adc(in_low=2.1 in_high=2.2 rise_delay=1p fall_delay=1p out_low=0 out_high=5 clamp_low=0 clamp_high=5 clamp_res=10 clamp_bias=.5 schmitt=true time_tol=20p)
Aalb [din0 din1 din2 din3] [dout0 dout1] 0 [sink0 sink1] [source0 source1] STATEMODEL
.model STATEMODEL sr_alb_stateful(source_hex="$hex" reg_delay=1n port_delay=1n)
Adac0 dout0 sink0 source0 %gd(q0 0) DAC
Adac1 dout1 sink1 source1 %gd(q1 0) DAC
.model DAC sr_family_dac(out_low=0 out_high=5 g_pullup=.024 g_pulldown=.034 g_resistive=.001 g_hiz=1e-9 knee_low=2 knee_high=2.75 v_smooth=.5 input_load=-31p t_rise=2n t_fall=2n sink_current=0 source_current=0)
Rout0 q0 0 100k
Rout1 q1 0 100k
.tran 20p 42n 0 20p
.control
run
wrdata native.dat v(q0) v(q1)
quit
.endc
.end
SPICE
(cd "$work" && ngspice -b circuit.cir > sim.log 2>&1) || {
    tail -n 80 "$work/sim.log" >&2
    exit 1
}
if rg -i 'error:|fatal error|timestep too small|no such model' "$work/sim.log"; then
    echo "Unexpected XSPICE error in mixed-stage circuit" >&2
    exit 1
fi
awk '
    function abs(x) { return x < 0 ? -x : x }
    BEGIN {
        t[1] = 4e-9; expect0[1] = 0; expect1[1] = 0;
        t[2] = 18e-9; expect0[2] = 5; expect1[2] = 0;
        t[3] = 24e-9; expect0[3] = 5; expect1[3] = 0;
        t[4] = 35e-9; expect0[4] = 0; expect1[4] = 5;
        n = 4; sample = 1;
    }
    sample <= n && ($1 + 0) >= t[sample] {
        if (abs($2 - expect0[sample]) > 0.75 ||
            abs($4 - expect1[sample]) > 0.75) {
            printf "mixed-stage sample %d at %.3fns got %.4fV/%.4fV, expected %.1fV/%.1fV\n",
                sample, ($1 + 0) * 1e9, $2, $4, expect0[sample], expect1[sample] > "/dev/stderr";
            failed = 1;
        } else {
            printf "mixed-stage sample %d: %.3fns Q0=%.4fV Q1=%.4fV\n",
                sample, ($1 + 0) * 1e9, $2, $4;
        }
        sample++;
    }
    END {
        if (sample <= n || failed) exit 1;
        print "2-bit COMB -> EDGE -> LEVEL physical smoke passed";
    }
' "$work/native.dat"
