#!/usr/bin/env bash
# HICUM/L2 native ngspice 47 level=8 effective temperature regression.
# Records internal bias (not source bias), branch IBEI/IBCI, collector I_C,
# critical ICK and d(Qf)/d(Itf) TF for all flcomp thermal branches.
set -euo pipefail
work=$(mktemp -d /tmp/hicum2-temperature-oracle.XXXXXX)
trap 'rm -rf "$work"' EXIT
printf '%s\n' '# flcomp,ambient_celsius,local_dt,source_vbe,source_vce,vbiei,vbici,ibei,ibci,ic,ick,tf,cpii,cmui'
for flcomp in 0 2.1 2.2 2.3; do
    for celsius in -40 0 27 60 100 125; do
        for local_dt in 0 15; do
            for pair in '0.00 1.30' '0.55 1.30' '0.75 1.30' '0.85 1.25'; do
                read -r source_be source_ce <<< "$pair"
                cat > "$work/deck.cir" <<DECK
HICUM native temperature preparation oracle
VBE b 0 DC $source_be
VCE c 0 DC $source_ce
Q1 c b 0 0 H2
.model H2 NPN level=8 tnom=27 dt=$local_dt flcomp=$flcomp
+ c10=2e-30 qp0=2e-14 ich=0 mcf=1
+ ibeis=1e-18 mbei=1 ireis=3e-18 mrei=2
+ ibeps=1e-19 mbep=1 ireps=5e-19 mrep=2
+ ibcis=1e-16 mbci=1 ibcxs=2e-18 mbcx=1 iscs=3e-18 msc=1.1
+ cjei0=2.5e-15 vdei=.9 zei=.5 ajei=2.5
+ cjep0=1e-15 vdep=.87 zep=.47 ajep=3
+ cjci0=1.4e-15 vdci=.7 zci=.4 vptci=3
+ cjcx0=9e-16 vdcx=.75 zcx=.4 vptcx=2 fbcpar=.4444444444444444
+ cjs0=4e-16 vds=.6 zs=.5 vpts=100
+ cscp0=3e-16 vdsp=0 zsp=.5 vptsp=100
+ t0=2e-12 dt0h=.4e-12 tbvl=.25e-12 tef0=1e-12 gtfe=1
+ thcs=3e-12 ahc=.1 fthc=.6 rci0=150 vlim=.5 vces=.1
+ vpt=100 aick=.001 delck=2 tr=1e-12 vcbar=0 icbar=.01 acbar=.01
+ latb=.2 latl=.1
+ hjei=1.1 ahjei=.15 hjci=1 hf0=1.05 hfe=.9 hfc=1.1
+ vgb=1.15 vge=1.13 vgc=1.18 vgs=1.2
+ f1vg=-1.02377e-4 f2vg=4.3215e-4
+ zetact=2.7 zetabet=3.4 zetaci=.4 zetacx=1.2
+ alb=.001 alvs=.0002 alces=.0001 alt0=.00005 kt0=1e-7
+ zetahjei=1.1 zetavgbe=.9 dvgbe=.06
.options gmin=1e-18 temp=$celsius
.control
set numdgt=15
op
print @q1[vbiei] @q1[vbici] @q1[ibei] @q1[ibci] @q1[ic] @q1[ick] @q1[tf] @q1[cpii] @q1[cmui]
quit
.endc
.end
DECK
                ngspice -b "$work/deck.cir" > "$work/output" 2>&1 || {
                    cat "$work/output" >&2
                    exit 1
                }
                if rg -qi 'Error:|unrecognized parameter|Warning: Model issue|fatal error' "$work/output" ||
                    ! rg -q 'No. of Data Rows : 1' "$work/output"; then
                    cat "$work/output" >&2
                    exit 1
                fi
                awk -v header="$flcomp,$celsius,$local_dt,$source_be,$source_ce" '
                    BEGIN {printf "%s",header}
                    /^@q1\[vbiei\] =/ {printf ",%s",$3}
                    /^@q1\[vbici\] =/ {printf ",%s",$3}
                    /^@q1\[ibei\] =/ {printf ",%s",$3}
                    /^@q1\[ibci\] =/ {printf ",%s",$3}
                    /^@q1\[ic\] =/ {printf ",%s",$3}
                    /^@q1\[ick\] =/ {printf ",%s",$3}
                    /^@q1\[tf\] =/ {printf ",%s",$3}
                    /^@q1\[cpii\] =/ {printf ",%s",$3}
                    /^@q1\[cmui\] =/ {printf ",%s",$3}
                    END {print ""}
                ' "$work/output"
            done
        done
    done
done
