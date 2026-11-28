#!/usr/bin/env bash
# Native ngspice-47 operating point oracle for physical VBIC Qbe/Qbc and Cbe/Cbc.
# Emit model variant, QBM, external biases, intrinsic biases, charge, capacitance.
set -euo pipefail
tmpdir=$(mktemp -d /tmp/vbic-charge-oracle-XXXXXX)
trap 'rm -rf "$tmpdir"' EXIT

printf '%s\n' '# variant,qbm,external_vbe,external_vbc,internal_vbe,internal_vbc,qbe,qbc,cbe,cbc'
for variant in full knee_disabled no_modulation; do
    case "$variant" in
        full) charge='tf=8e-12 qtf=.25 xtf=.45 vtf=.6 itf=.02 tr=1.3e-12 qco=8e-15 gamm=.13' ;;
        knee_disabled) charge='tf=8e-12 qtf=.25 xtf=.45 vtf=.6 itf=0 tr=1.3e-12 qco=8e-15 gamm=.13' ;;
        no_modulation) charge='tf=8e-12 qtf=0 xtf=0 vtf=0 itf=0 tr=1.3e-12 qco=0 gamm=.13' ;;
    esac
    for qbm in 0 1; do
        for biases in '0.55 -0.60' '0.70 -0.30' '0.75 -0.35' '0.85 -0.15' '0.90 0.20' '0.65 0.70'; do
            read -r external_vbe external_vbc <<< "$biases"
            vce=$(awk -v a="$external_vbe" -v b="$external_vbc" 'BEGIN {printf "%.12g", a-b}')
            cat > "$tmpdir/input.cir" <<DECK
VBIC physical charge verification
VBE b 0 DC $external_vbe
VCE c 0 DC $vce
Q1 c b 0 0 0 CHARGE
.model CHARGE NPN level=9 tnom=27 is=4.81e-17 nf=1.018 nr=1.01 isrr=1 vef=189 ver=5.3 ikf=0.009 ikr=0.01 nkf=.5 qbm=$qbm
+ $charge
+ cje=3e-15 cjc=2e-15 pe=.92 me=.12 pc=.558 mc=.12 fc=.8 aje=.014 ajc=.009
+ cbeo=0 cbco=0 cjep=0 cjcp=0
+ ibei=0 iben=0 ibci=0 ibcn=0 ibeip=0 ibenp=0 ibcip=0 ibcnp=0 isp=0
+ rcx=0 rci=0 re=0 rbx=0 rbi=0 rbp=0 rs=0 avc1=0
.options gmin=1e-18
.control
set numdgt=15
op
print @q1[vbe] @q1[vbc] @q1[qbe] @q1[qbc] @q1[cbe] @q1[cbc]
quit
.endc
.end
DECK
            ngspice -b "$tmpdir/input.cir" > "$tmpdir/output" 2>&1 || {
                cat "$tmpdir/output" >&2
                exit 1
            }
            if rg -qi 'unrecognized parameter|Warning: Model issue|fatal error' "$tmpdir/output" ||
                ! rg -q 'No. of Data Rows : 1' "$tmpdir/output"; then
                cat "$tmpdir/output" >&2
                exit 1
            fi
            awk -v prefix="$variant,$qbm,$external_vbe,$external_vbc" '
                BEGIN { printf "%s", prefix }
                /@q1\[vbe\] =/ { printf ",%s", $3 }
                /@q1\[vbc\] =/ { printf ",%s", $3 }
                /@q1\[qbe\] =/ { printf ",%s", $3 }
                /@q1\[qbc\] =/ { printf ",%s", $3 }
                /@q1\[cbe\] =/ { printf ",%s", $3 }
                /@q1\[cbc\] =/ { printf ",%s", $3 }
                END {print ""}
            ' "$tmpdir/output"
        done
    done
done
