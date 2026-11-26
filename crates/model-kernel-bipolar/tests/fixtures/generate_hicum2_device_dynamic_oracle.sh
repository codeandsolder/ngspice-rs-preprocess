#!/usr/bin/env bash
# Independent native ngspice-47 transient and forced-temperature noise oracle.
set -euo pipefail
scratch=$(mktemp -d /tmp/hicum2-device-dynamic.XXXXXX)
trap 'rm -rf "$scratch"' EXIT
mode=${1:?noise or transient}
case "$mode" in
noise) printf '%s\n' '# ambient,rise,vbe,correlated,hz,collector_psd' ;;
transient) printf '%s\n' '# flsh,ambient,time,vbe,rise,ic' ;;
*) exit 2 ;;
esac
for ambient in -40 27 85; do
    if [[ $mode == noise ]]; then
        for rise in 0 10 30; do
            for bias in .65 .75; do
                for correlated in 0 1; do
                    cat > "$scratch/deck.cir" <<DECK
HICUM2 forced thermal collector noise
Vc c 0 1.2
Vb b 0 $bias AC 1
Ve e 0 0
Vs s 0 -.1
Vt t 0 $rise
Q1 c b e s t H2
Hout out 0 Vc 1
.model H2 npn level=8 flcomp=2.3 flsh=2 rth=200 cth=1e-9
+ alrth=.0003 zetarth=.5 t0=5e-12 tr=1e-12 flnqs=0
+ flcono=$correlated alit=.5 alqf=.5
.temp $ambient
.options gmin=1e-18 reltol=1e-9 abstol=1e-15 vntol=1e-12
.control
set numdgt=17
set wr_singlescale
noise v(out) Vb dec 1 1e6 1e9
setplot noise1
wrdata $scratch/data onoise_spectrum
quit
.endc
.end
DECK
                    ngspice -b -n "$scratch/deck.cir" > "$scratch/log" 2>&1
                    if rg -qi 'error:|unrecognized parameter|fatal error|failed' "$scratch/log" ||
                        [[ $(wc -l < "$scratch/data") -ne 4 ]]; then
                        cat "$scratch/log" >&2; exit 1
                    fi
                    awk -v prefix="$ambient,$rise,$bias,$correlated" '{printf "%s,%.17e,%.17e\n",prefix,$1,$2*$2}' "$scratch/data"
                done
            done
        done
    else
        for flsh in 1 2; do
            cat > "$scratch/deck.cir" <<DECK
HICUM2 free thermal transient
Vc c 0 1.2
Vb b 0 PWL(0 .68 10n .68 20n .75 1u .75)
Ve e 0 0
Vs s 0 -.1
Q1 c b e s t H2
.model H2 npn level=8 flcomp=2.3 flsh=$flsh rth=200 cth=1e-9
+ alrth=.0003 zetarth=.5 t0=5e-12 tr=1e-12 flnqs=0
.temp $ambient
.options gmin=1e-18 reltol=1e-9 abstol=1e-15 vntol=1e-12
.control
set numdgt=17
set wr_singlescale
tran 2n 1u 0 .25n
linearize v(b) v(t) i(vc)
wrdata $scratch/data v(b) v(t) i(vc)
quit
.endc
.end
DECK
            ngspice -b -n "$scratch/deck.cir" > "$scratch/log" 2>&1
            if rg -qi 'error:|unrecognized parameter|fatal error|failed' "$scratch/log" ||
                [[ $(wc -l < "$scratch/data") -ne 501 ]]; then
                cat "$scratch/log" >&2; exit 1
            fi
            awk -v prefix="$flsh,$ambient" '{printf "%s,%.17e,%.17e,%.17e,%.17e\n",prefix,$1,$2,$3,-$4}' "$scratch/data"
        done
    fi
done
