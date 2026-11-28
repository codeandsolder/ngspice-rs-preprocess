#!/usr/bin/env bash
# Reproducible ngspice-47 LEVEL=8 HICUM/L2 native AREA and M semantics.
set -euo pipefail
scratch=$(mktemp -d /tmp/hicum2-instance-oracle.XXXXXX)
trap 'rm -rf "$scratch"' EXIT
printf '%s\n' '# area,m,ambient_celsius,vbe,vce,ic,ibei,ibci,ick,tf'
for pair in '1 1' '6 1' '1 6' '2 3' '.5 4'; do
    read -r area multiplicity <<< "$pair"
    for ambient in -40 27 100; do
        for vbe in .55 .65 .72 .78; do
            for vce in .2 1.5; do
                cat > "$scratch/deck.cir" <<DECK
HICUM 2.4.0 instance-scaling oracle: native ngspice 47 level 8
VBE b 0 DC $vbe
VCE c 0 DC $vce
Q1 c b 0 0 H2 area=$area m=$multiplicity
.model H2 NPN level=8
.options temp=$ambient gmin=1e-12
.control
set numdgt=15
op
print @q1[ic] @q1[ibei] @q1[ibci] @q1[ick] @q1[tf]
quit
.endc
.end
DECK
                ngspice -b "$scratch/deck.cir" > "$scratch/output" 2>&1 || {
                    cat "$scratch/output" >&2; exit 1;
                }
                if rg -qi 'Error:|unrecognized parameter|fatal error' "$scratch/output" ||
                  ! rg -q 'No. of Data Rows : 1' "$scratch/output"; then
                    cat "$scratch/output" >&2; exit 1
                fi
                awk -v initial="$area,$multiplicity,$ambient,$vbe,$vce" '
                    BEGIN {printf "%s",initial}
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
done
