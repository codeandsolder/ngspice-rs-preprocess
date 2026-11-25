#!/usr/bin/env bash
# Reproduce VBIC native ngspice-47 internal epitaxial resistor IrcI.
# Requires ngspice 47; all temporary decks are cleaned from /tmp.
# ngspice 47 clamps explicit RCX=0 to a physical 0.01-ohm resistor;
# older/newer ngspice revisions may use a different clamp. The Rust oracle
# reconstructs the actual collector CX voltage from that model-specific
# resistance and the measured external source current. None of the VBIC
# physics kernels implement this parser-level source-model minimum.
set -euo pipefail
tmpdir=$(mktemp -d /tmp/vbic-resistance-XXXXXX)
trap 'rm -rf "$tmpdir"' EXIT
echo '# variant,qbm,external_vbe,external_vce,internal_vbe,internal_vbc,collector_internal_v,collector_supply_current'
for variant in linear field velocity highcurrent; do
    case "$variant" in
        linear) extra='gamm=0 vo=0 hrcf=0' ;;
        field) extra='gamm=.1 vo=0 hrcf=0' ;;
        velocity) extra='gamm=.1 vo=.5 hrcf=0' ;;
        highcurrent) extra='gamm=.1 vo=.5 hrcf=.5' ;;
    esac
    for qbm in 0 1; do
        for biases in '0.60 0.80' '0.65 1.10' '0.80 1.20' '0.90 1.40' '0.74 1.80'; do
            read -r external_be external_ce <<< "$biases"
            cat > "$tmpdir/input.cir" <<DECK
VBIC native epitaxial collector resistor
VBE b 0 DC $external_be
VCE c 0 DC $external_ce
Q1 c b 0 0 RX
.model RX NPN level=9 tnom=27 is=4.81e-17 nf=1.018 nr=1.01 isrr=1 vef=189 ver=5.3 ikf=.009 ikr=.01 nkf=.5 qbm=$qbm
+ rci=5 rcx=0 rbx=0 rbi=0 re=0 rbp=0 rs=0 $extra
+ ibei=0 iben=0 ibci=0 ibcn=0 ibeip=0 ibenp=0 ibcip=0 ibcnp=0 isp=0
+ cje=0 cjc=0 cjep=0 cjcp=0 cbeo=0 cbco=0 avc1=0
.options gmin=1e-18
.control
set numdgt=15
op
print @q1[vbe] @q1[vbc] v(q1#collCI) i(vce)
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
            awk -v prefix="$variant,$qbm,$external_be,$external_ce" '
                BEGIN {printf "%s",prefix}
                /@q1\[vbe\] =/ {printf ",%s",$3}
                /@q1\[vbc\] =/ {printf ",%s",$3}
                /^v\(q1#collci\) =/ {printf ",%s",$3}
                /^i\(vce\) =/ {printf ",%s",$3}
                END {print ""}
            ' "$tmpdir/output"
        done
    done
done
