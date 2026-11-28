#!/usr/bin/env bash
# Native ngspice-47 LEVEL=8 series-R nonlinear operating-point corpus.
set -euo pipefail
scratch=$(mktemp -d /tmp/hicum2-dc-internal.XXXXXX)
trap 'rm -rf "$scratch"' EXIT
printf '%s\n' '# variant,ambient_celsius,vbe,vce,vbiei,vbici,vbpbi,ic,ib,is'
for variant in all half mostly large all_scaled large_scaled; do
    area_m=''
    case "$variant" in
        all_scaled) area_m='area=2 m=3';;
        large_scaled) area_m='area=0.5 m=4';;
    esac
    case "$variant" in
        all|all_scaled)    resistance='rbx=12 rbi0=9 rcx=6 re=3 rsu=4';;
        half)   resistance='rbx=2 rbi0=0 rcx=4 re=0 rsu=8';;
        mostly) resistance='rbx=0 rbi0=5 rcx=0 re=3 rsu=0';;
        large|large_scaled)  resistance='rbx=50 rbi0=15 rcx=10 re=8 rsu=5';;
    esac
    for ambient in 27 100; do
        for vbe in .62 .72 .80; do
            for vce in .4 1.2; do
                cat > "$scratch/deck.cir" <<DECK
Native ngspice-47 source HICUM level8 series resistor oracle
VBE b 0 DC $vbe
VCE c 0 DC $vce
Q1 c b 0 0 H2 $area_m
.model H2 NPN level=8 flcomp=2.3 $resistance
.options temp=$ambient gmin=1e-12
.control
set numdgt=15
op
print @q1[vbiei] @q1[vbici] @q1[vbpbi] @q1[ic] @q1[ib] @q1[is]
quit
.endc
.end
DECK
                ngspice -b "$scratch/deck.cir" > "$scratch/result" 2>&1
                if rg -qi 'Error:|unrecognized parameter|fatal error|failed' "$scratch/result" ||
                   ! rg -q 'No. of Data Rows : 1' "$scratch/result"; then
                    cat "$scratch/result" >&2
                    exit 1
                fi
                awk -v initial="$variant,$ambient,$vbe,$vce" '
                    BEGIN {printf "%s", initial}
                    /^@q1\[vbiei\] =/ {printf ",%s",$3;n++}
                    /^@q1\[vbici\] =/ {printf ",%s",$3;n++}
                    /^@q1\[vbpbi\] =/ {printf ",%s",$3;n++}
                    /^@q1\[ic\] =/ {printf ",%s",$3;n++}
                    /^@q1\[ib\] =/ {printf ",%s",$3;n++}
                    /^@q1\[is\] =/ {printf ",%s",$3;n++}
                    END {if(n!=6)exit 1;print ""}
                ' "$scratch/result"
            done
        done
    done
done
