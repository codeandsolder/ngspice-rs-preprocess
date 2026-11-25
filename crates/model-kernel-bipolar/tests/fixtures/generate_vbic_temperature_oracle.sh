#!/usr/bin/env bash
# Reproduce source-native ngspice-47 VBIC diode temperature/ideality currents.
# Includes actual internal junction voltages to avoid hidden min-resistance bias.
set -euo pipefail
tmpdir=$(mktemp -d /tmp/vbic-temperature-XXXXXX)
trap 'rm -rf "$tmpdir"' EXIT
echo '# ambient_celsius,external_vbe,external_vce,intrinsic_vbe,intrinsic_vbc,base_current,conductance_gpi'
for temp in -40 0 27 60 100 125; do
    for emitter in 0.60 0.65 0.72 0.80; do
        cat > "$tmpdir/input.cir" <<DECK
VBIC native temperature-dependent diode
VBE b 0 DC $emitter
VCE c 0 DC 1.3
Q1 c b 0 0 TEMP_DIODE
.model TEMP_DIODE NPN level=9 tnom=27 is=4.81e-17 nf=1.018 nr=1.01 isrr=1
+ ibei=1.9e-19 nei=1.066 wbe=1 iben=0 ibci=0 ibcn=0
+ ibeip=0 ibenp=0 ibcip=0 ibcnp=0 isp=0
+ xii=3 eaie=1.12
+ rcx=0 rci=0 re=0 rbx=0 rbi=0 rbp=0 rs=0
+ cje=0 cjc=0 cjep=0 cjcp=0 cbeo=0 cbco=0 avc1=0
.options gmin=1e-18 temp=$temp
.control
set numdgt=15
op
print @q1[vbe] @q1[vbc] @q1[ib] @q1[gpi]
quit
.endc
.end
DECK
        if ! ngspice -b "$tmpdir/input.cir" > "$tmpdir/output" 2>&1; then
            cat "$tmpdir/output" >&2
            exit 1
        fi
        if rg -qi 'unrecognized parameter|Warning: Model issue|fatal error' "$tmpdir/output" ||
            ! rg -q 'No. of Data Rows : 1' "$tmpdir/output"; then
            cat "$tmpdir/output" >&2
            exit 1
        fi
        awk -v prefix="$temp,$emitter,1.3" '
            BEGIN {printf "%s",prefix}
            /@q1\[vbe\] =/ {printf ",%s",$3}
            /@q1\[vbc\] =/ {printf ",%s",$3}
            /@q1\[ib\] =/ {printf ",%s",$3}
            /@q1\[gpi\] =/ {printf ",%s",$3}
            END {print ""}
        ' "$tmpdir/output"
    done
done
