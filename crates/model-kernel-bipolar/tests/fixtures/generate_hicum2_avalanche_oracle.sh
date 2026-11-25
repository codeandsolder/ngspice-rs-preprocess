#!/usr/bin/env bash
# Native ngspice-47 HICUM/L2 level=8 HICAVL avalanche oracle.
# Explicitly sample intrinsic Vbiei/Vbici and IAVL, not only source bias.
set -euo pipefail
scratch=$(mktemp -d /tmp/hicum2-avalanche-oracle.XXXXXX)
trap 'rm -rf "$scratch"' EXIT
printf '%s\n' '# variant,source_vbe,source_vce,vbiei,vbici,iavl,ic'
for variant in weak strong very_strong reachthrough off; do
    case "$variant" in
        weak) avalanche='favl=.04 qavl=4e-16 kavl=0 vptci=3' ;;
        strong) avalanche='favl=.04 qavl=4e-16 kavl=.4 vptci=3' ;;
        very_strong) avalanche='favl=.4 qavl=4e-16 kavl=1.2 vptci=3' ;;
        reachthrough) avalanche='favl=.04 qavl=4e-16 kavl=.4 vptci=100' ;;
        off) avalanche='favl=0 qavl=4e-16 kavl=.4 vptci=3' ;;
    esac
    for bias in '0.55 0.95' '0.7 1.2' '0.75 1.3' '0.85 1.3' '0.90 1.3' '0.7 2.0' '0.8 2.0' '0.9 2.0'; do
        read -r emitter_voltage collector_voltage <<< "$bias"
        cat > "$scratch/input.cir" <<DECK
HICUM L2 native avalanche oracle
VBE b 0 DC $emitter_voltage
VCE c 0 DC $collector_voltage
Q1 c b 0 0 H2
.model H2 NPN level=8 tnom=27 c10=2e-30 qp0=2e-14 ich=0 mcf=1
+ t0=2e-12 tr=1e-12 rci0=150 vlim=.5 vces=.1 vpt=100 aick=.001 delck=2
+ cjei0=2.5e-15 vdei=.9 zei=.5 ajei=2.5
+ cjci0=1.4e-15 vdci=.7 zci=.4 $avalanche
+ thcs=3e-12 tef0=1e-12 gtfe=1 ahc=.1 fthc=.6 latb=0 latl=0
+ hf0=1 hfe=1 hfc=1 hjei=1 hjci=1 dt0h=.4e-12 tbvl=.25e-12
+ vcbar=0 icbar=.01 acbar=.01 flcomp=2.3
.options gmin=1e-18 temp=27
.control
set numdgt=15
op
print @q1[vbiei] @q1[vbici] @q1[iavl] @q1[ic]
quit
.endc
.end
DECK
        ngspice -b "$scratch/input.cir" > "$scratch/output" 2>&1 || {
            cat "$scratch/output" >&2
            exit 1
        }
        if rg -qi 'Error:|unrecognized parameter|Warning: Model issue|fatal error' "$scratch/output" ||
          ! rg -q 'No. of Data Rows : 1' "$scratch/output"; then
            cat "$scratch/output" >&2
            exit 1
        fi
        awk -v header="$variant,$emitter_voltage,$collector_voltage" '
          BEGIN {printf "%s",header}
          /^@q1\[vbiei\] =/ {printf ",%s",$3}
          /^@q1\[vbici\] =/ {printf ",%s",$3}
          /^@q1\[iavl\] =/ {printf ",%s",$3}
          /^@q1\[ic\] =/ {printf ",%s",$3}
          END {print ""}
        ' "$scratch/output"
    done
done
