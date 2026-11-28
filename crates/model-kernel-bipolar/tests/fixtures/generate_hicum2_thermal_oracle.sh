#!/usr/bin/env bash
# HICUM/L2 2.4.0 native ngspice 47 self-heating equilibrium oracle.
# Source model has no series resistors and no avalanche; pterm is
# Vciei*It (FLSH=1) and also Ibei*Vbiei+Ibci*Vbici (FLSH=2).
# Native @q1[tk] reports unheated reference, so use @q1[dtsh].
set -euo pipefail
scratch=$(mktemp -d /tmp/hicum2-thermal-oracle.XXXXXX)
trap 'rm -rf "$scratch"' EXIT
printf '%s\n' '# mode,ambient_celsius,vbe,vce,rth,zetarth,alrth,dtsh,ic,ibei,ibci,vbiei,vbici'
for mode in 0 1 2; do
    for ambient in -40 27 100; do
        for rth in 100 500; do
            for source_vbe in .68 .75 .82; do
                cat > "$scratch/deck.cir" <<DECK
HICUM L2 2.4 native thermal DC reference
VBE b 0 DC $source_vbe
VCE c 0 DC 1.3
Q1 c b 0 0 H2
.model H2 NPN level=8 tnom=27 c10=2e-30 qp0=2e-14
+ flsh=$mode rth=$rth cth=1e-9 alrth=.0003 zetarth=.5
+ rbi0=0 rcx=0 rbx=0 re=0 rsu=0
+ favl=0 ibeps=0 ireps=0 ibcxs=0 itss=0 iscs=0
.options temp=$ambient gmin=1e-18
.control
set numdgt=15
op
print @q1[dtsh] @q1[ic] @q1[ibei] @q1[ibci] @q1[vbiei] @q1[vbici]
quit
.endc
.end
DECK
                ngspice -b "$scratch/deck.cir" > "$scratch/out" 2>&1 || {
                    cat "$scratch/out" >&2; exit 1
                }
                if rg -qi 'Error:|unrecognized parameter|Warning: Model issue|fatal error' "$scratch/out" ||
                    ! rg -q 'No. of Data Rows : 1' "$scratch/out"; then
                    cat "$scratch/out" >&2; exit 1
                fi
                awk -v prefix="$mode,$ambient,$source_vbe,1.3,$rth,0.5,0.0003" '
                    BEGIN {printf "%s",prefix}
                    /^@q1\[dtsh\] =/ {printf ",%s",$3}
                    /^@q1\[ic\] =/ {printf ",%s",$3}
                    /^@q1\[ibei\] =/ {printf ",%s",$3}
                    /^@q1\[ibci\] =/ {printf ",%s",$3}
                    /^@q1\[vbiei\] =/ {printf ",%s",$3}
                    /^@q1\[vbici\] =/ {printf ",%s",$3}
                    END {print ""}
                ' "$scratch/out"
            done
        done
    done
done
