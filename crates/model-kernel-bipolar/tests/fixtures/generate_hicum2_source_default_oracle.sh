#!/usr/bin/env bash
# HICUM/L2 ngspice-47 LEVEL=8 with no supplied model coefficients.
# Tests the source-parameter default factory independently of Rust literals.
set -euo pipefail
scratch=$(mktemp -d /tmp/hicum2-default-oracle.XXXXXX)
trap 'rm -rf "$scratch"' EXIT
printf '%s\n' '# ambient_celsius,vbe,vce,vbiei,vbici,ic,ibei,ibci,ick,tf'
for ambient in -40 27 100; do
    for vbe in .55 .65 .72 .78; do
        for vce in .2 .8 1.5; do
            cat > "$scratch/deck.cir" <<DECK
HICUM L2 2.4 native unmodified model defaults
VBE b 0 DC $vbe
VCE c 0 DC $vce
Q1 c b 0 0 H2
.model H2 NPN level=8
.options temp=$ambient gmin=1e-12
.control
set numdgt=15
op
print @q1[vbiei] @q1[vbici] @q1[ic] @q1[ibei] @q1[ibci] @q1[ick] @q1[tf]
quit
.endc
.end
DECK
            ngspice -b "$scratch/deck.cir" > "$scratch/output" 2>&1 || {
                cat "$scratch/output" >&2;exit 1;
            }
            if rg -qi 'Error:|unrecognized parameter|fatal error' "$scratch/output" ||
              ! rg -q 'No. of Data Rows : 1' "$scratch/output"; then
                cat "$scratch/output" >&2;exit 1
            fi
            awk -v initial="$ambient,$vbe,$vce" '
                BEGIN {printf "%s",initial}
                /^@q1\[vbiei\] =/ {printf ",%s",$3}
                /^@q1\[vbici\] =/ {printf ",%s",$3}
                /^@q1\[ic\] =/ {printf ",%s",$3}
                /^@q1\[ibei\] =/ {printf ",%s",$3}
                /^@q1\[ibci\] =/ {printf ",%s",$3}
                /^@q1\[ick\] =/ {printf ",%s",$3}
                /^@q1\[tf\] =/ {printf ",%s",$3}
                END {print ""}
            ' "$scratch/output"
        done
    done
done
