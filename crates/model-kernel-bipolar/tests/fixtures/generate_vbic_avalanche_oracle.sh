#!/usr/bin/env bash
# VBIC 1.1.5 impact ionization and B-C diode .op oracle, ngspice 47.
set -euo pipefail
tmpdir=$(mktemp -d /tmp/vbic-avalanche-XXXXXX)
trap 'rm -rf "$tmpdir"' EXIT
echo '# variant,qbm,external_vbe,external_vbc,internal_vbe,internal_vbc,ib,gmu'
for variant in enabled disabled; do
    if [[ "$variant" = enabled ]]; then
        avalanche='avc1=.08 avc2=.15'
    else
        avalanche='avc1=0 avc2=.15'
    fi
    for qbm in 0 1; do
        for biases in '0.55 -0.50' '0.65 -1.00' '0.80 -1.50' '0.90 -2.00' '0.55 0.20' '0.70 0.50'; do
            read -r external_be external_bc <<< "$biases"
            vce=$(awk -v a="$external_be" -v b="$external_bc" 'BEGIN {printf "%.12g", a-b}')
            cat > "$tmpdir/input.cir" <<DECK
VBIC impact-ionization source reference
VBE b 0 DC $external_be
VCE c 0 DC $vce
Q1 c b 0 0 0 AVALANCHE
.model AVALANCHE NPN level=9 tnom=27 is=4.81e-17 nf=1.018 nr=1.01 isrr=1 vef=189 ver=5.3 ikf=.009 ikr=.01 nkf=.5 qbm=$qbm
+ ibei=0 iben=0 ibci=1e-16 nci=1.01 ibcn=0 ibeip=0 ibenp=0 ibcip=0 ibcnp=0 isp=0
+ $avalanche pc=.75 mc=.33
+ cje=0 cjc=0 cjep=0 cjcp=0 cbeo=0 cbco=0 rcx=0 rci=0 re=0 rbx=0 rbi=0 rbp=0 rs=0
.options gmin=1e-18
.control
set numdgt=15
op
print @q1[vbe] @q1[vbc] @q1[ib] @q1[gmu]
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
            awk -v prefix="$variant,$qbm,$external_be,$external_bc" '
                BEGIN {printf "%s",prefix}
                /@q1\[vbe\] =/ {printf ",%s",$3}
                /@q1\[vbc\] =/ {printf ",%s",$3}
                /@q1\[ib\] =/ {printf ",%s",$3}
                /@q1\[gmu\] =/ {printf ",%s",$3}
                END {print ""}
            ' "$tmpdir/output"
        done
    done
done
