#!/usr/bin/env bash
# Reproduce resolved public IHP HBT card and independent ngspice 47 DC/AC data.
# Usage: generate_vbic_device_oracle.sh PDK_MODELS_DIRECTORY OUTPUT_DIRECTORY
# Pinned PDK source: IHP-GmbH/IHP-Open-PDK@af6b1ebc82c36daf30516a46d3a0a6a5777df0c1
# ihp-sg13g2/libs.tech/ngspice/models/{cornerHBT,sg13g2_hbt_mod,sg13g2_hbt_stat}.lib
set -euo pipefail
pdk=$(realpath "$1")
output=$(realpath "$2")
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
for library in cornerHBT sg13g2_hbt_mod sg13g2_hbt_stat; do
    ln -s "$pdk/$library.lib" "$scratch/$library.lib"
done
cd "$scratch"
cat > extract.cir <<'DECK'
IHP nominal HBT parameter realization
.lib cornerHBT.lib hbt_typ
Vc c 0 1.2
Vb b 0 0.78
Ve e 0 0
Vs s 0 -0.1
X1 c b e s npn13G2 Nx=4 selft=0 mm_ok=0
.temp 27
.control
set numdgt=17
op
listing e
showmod all
quit
.endc
.end
DECK
ngspice -b -n extract.cir > extract.log 2>&1
if ! rg -q 'No. of Data Rows : 1' extract.log || rg -qi 'error:|unrecognized parameter' extract.log; then
    cat extract.log >&2
    exit 1
fi
# listing e emits parameter expressions resolved by numparam, but removes
# runtime-temper TF. Read that slot from the instantiated model instead.
card=$(sed -nE 's/^[[:space:]]*[0-9]+ : \.model x1:npn13g2_nx_vbic npn (.*)$/\1/p' extract.log | sed -E 's/=[[:space:]]+/=/g;s/[[:space:]]+/ /g')
test -n "$card"
if ! rg -q '(^| )tf=' <<< "$card"; then
    tf=$(awk '$1=="tf" {print $2}' extract.log)
    test -n "$tf"
    card="$card tf=$tf"
fi
tr ' ' '\n' <<< "$card" | sort > "$output/vbic_ihp_nx4_card.txt"
echo '# temperature_celsius,base,collector,substrate,ic,ib,ie,is,ci,ac_ic_real,ac_ic_imag,ac_ib_real,ac_ib_imag' > "$output/vbic_ihp_device.csv"
for temperature in -40 27 85 125; do
    # Forward active, high injection, and reverse operation at each temperature.
    for bias in '0.65 1.2 -0.1' '0.78 1.2 -0.1' '0.9 1.2 -0.1' '0.65 0.05 -0.1' '0.1 -0.3 -0.2'; do
        read -r base collector substrate <<< "$bias"
        cat > reference.cir <<DECK
IHP resolved nominal device comparison
Vc c 0 $collector
Vb b 0 $base AC 1
Ve e 0 0
Vs s 0 $substrate
Q1 c b e s 0 REF
.model REF npn $card
.temp $temperature
.options gmin=1e-18 reltol=1e-9 abstol=1e-15 vntol=1e-12
.control
set numdgt=17
set wr_singlescale
op
wrdata op.dat i(vc) i(vb) i(ve) i(vs) v(q1#collCI)
ac lin 1 1e9 1e9
wrdata ac.dat i(vc) i(vb)
quit
.endc
.end
DECK
        rm -f op.dat ac.dat
        ngspice -b -n reference.cir > reference.log 2>&1
        if rg -qi 'error:|unrecognized parameter|no convergence|timestep too small' reference.log || [[ ! -s op.dat || ! -s ac.dat ]]; then
            cat reference.log >&2
            exit 1
        fi
        awk -v prefix="$temperature,$base,$collector,$substrate" 'NR==1{printf "%s,%.17g,%.17g,%.17g,%.17g,%.17g",prefix,-$2,-$3,-$4,-$5,$6}' op.dat >> "$output/vbic_ihp_device.csv"
        awk 'NR==1{printf ",%.17g,%.17g,%.17g,%.17g\n",-$2,-$3,-$4,-$5}' ac.dat >> "$output/vbic_ihp_device.csv"
    done
done
