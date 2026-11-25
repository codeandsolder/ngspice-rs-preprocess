#!/usr/bin/env bash
# Native HICUM/L2 level-8 HICRBI nonlinear RBI and HF Crbi operating point.
# Carry intrinsic junction voltages and actual Ibei for source-law comparison.
set -euo pipefail
scratch=$(mktemp -d /tmp/hicum2-base-oracle.XXXXXX)
trap 'rm -rf "$scratch"' EXIT
printf '%s\n' '# variant,source_vbe,source_vce,vbiei,vbici,ibei,rbi,crbi'
for variant in baseline no_crowding partition_half charge_modulation no_shunt; do
    base='rbi0=20 fgeo=.6557 fdqr0=0 fqi=1 fcrbi=.35'
    case "$variant" in
        no_crowding) base='rbi0=20 fgeo=0 fdqr0=0 fqi=1 fcrbi=.35' ;;
        partition_half) base='rbi0=20 fgeo=.6557 fdqr0=0 fqi=.4 fcrbi=.35' ;;
        charge_modulation) base='rbi0=20 fgeo=.6557 fdqr0=1.5 fqi=1 fcrbi=.35' ;;
        no_shunt) base='rbi0=20 fgeo=.6557 fdqr0=0 fqi=1 fcrbi=0' ;;
    esac
    for voltages in '0.55 1.3' '0.65 1.3' '0.75 1.3' '0.85 1.3' '0.9 1.3' '0.75 0.9' '0.85 0.9'; do
        read -r vbe vce <<< "$voltages"
        cat > "$scratch/input.cir" <<DECK
Native HICUM/L2 2.4.0 RBI and Qrbi reference
VBE b 0 DC $vbe
VCE c 0 DC $vce
Q1 c b 0 0 H2
.model H2 NPN level=8 tnom=27 c10=2e-30 qp0=2e-14 ich=0 mcf=1
+ t0=2e-12 tr=1e-12 rci0=150 vlim=.5 vces=.1 vpt=100 aick=.001 delck=2
+ cjei0=2.5e-15 vdei=.9 zei=.5 ajei=2.5
+ cjci0=1.4e-15 vdci=.7 zci=.4 vptci=3
+ thcs=3e-12 tef0=1e-12 gtfe=1 ahc=.1 fthc=.6 latb=0 latl=0
+ hf0=1 hfe=1 hfc=1 hjei=1 hjci=1 dt0h=.4e-12 tbvl=.25e-12
+ ibeis=1e-18 ireis=0 ibcis=1e-16
+ vcbar=0 icbar=.01 acbar=.01 flcomp=2.3
+ $base
.options gmin=1e-18 temp=27
.control
set numdgt=15
op
print @q1[vbiei] @q1[vbici] @q1[ibei] @q1[rbi] @q1[crbi]
quit
.endc
.end
DECK
        ngspice -b "$scratch/input.cir" > "$scratch/output" 2>&1 || {
            cat "$scratch/output" >&2; exit 1
        }
        if rg -qi 'Error:|unrecognized parameter|Warning: Model issue|fatal error' "$scratch/output" ||
           ! rg -q 'No. of Data Rows : 1' "$scratch/output"; then
            cat "$scratch/output" >&2; exit 1
        fi
        awk -v header="$variant,$vbe,$vce" '
          BEGIN {printf "%s",header}
          /^@q1\[vbiei\] =/ {printf ",%s",$3}
          /^@q1\[vbici\] =/ {printf ",%s",$3}
          /^@q1\[ibei\] =/ {printf ",%s",$3}
          /^@q1\[rbi\] =/ {printf ",%s",$3}
          /^@q1\[crbi\] =/ {printf ",%s",$3}
          END {print ""}
        ' "$scratch/output"
    done
done
