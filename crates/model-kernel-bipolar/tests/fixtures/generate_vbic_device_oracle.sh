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

# The same resolved IHP card, with its published Nx=4 thermal RC activated.
# A voltage source at the fifth port forces the native temperature rise.
# Compare both electrical and heat-balance residuals and the thermal-drive
# small-signal response. The native temperature coordinate is a rise in K.
thermal_card=$(sed -E 's/(^| )rth=[^ ]+/ rth=3260/; s/(^| )cth=[^ ]+/ cth=1.6e-12/' <<< "$card")
echo '# ambient_celsius,rise,base,collector,substrate,ic,ib,ie,is,heat,ci,ac_ic_real,ac_ic_imag,ac_ib_real,ac_ib_imag,ac_heat_real,ac_heat_imag,dc_heat_slope' > "$output/vbic_ihp_thermal.csv"
for temperature in -40 27 85; do
    for rise in 0 20; do
        for bias in '0.78 1.2 -0.1' '0.65 0.05 -0.1'; do
            read -r base collector substrate <<< "$bias"
            read -r lower upper <<< "$(awk -v rise="$rise" 'BEGIN{printf "%.6f %.6f",rise-0.001,rise+0.001}')"
            cat > thermal.cir <<DECK
IHP thermal response
Vc c 0 $collector
Vb b 0 $base
Ve e 0 0
Vs s 0 $substrate
Vt t 0 $rise AC 1
Q1 c b e s t REF
.model REF npn $thermal_card selft=1
.temp $temperature
.options gmin=1e-18 reltol=1e-9 abstol=1e-15 vntol=1e-12
.control
set numdgt=17
set wr_singlescale
op
wrdata op.dat i(vc) i(vb) i(ve) i(vs) i(vt) v(q1#collCI)
ac lin 1 1e9 1e9
wrdata ac.dat i(vc) i(vb) i(vt)
dc Vt $lower $upper 0.002
wrdata slope.dat i(vt)
quit
.endc
.end
DECK
            rm -f op.dat ac.dat slope.dat
            ngspice -b -n thermal.cir > thermal.log 2>&1
            if rg -qi 'error:|unrecognized parameter|no convergence|timestep too small' thermal.log || [[ ! -s op.dat || ! -s ac.dat || ! -s slope.dat ]]; then
                cat thermal.log >&2
                exit 1
            fi
            awk -v prefix="$temperature,$rise,$base,$collector,$substrate" 'NR==1{printf "%s,%.17g,%.17g,%.17g,%.17g,%.17g,%.17g",prefix,-$2,-$3,-$4,-$5,-$6,$7}' op.dat >> "$output/vbic_ihp_thermal.csv"
            awk 'NR==1{printf ",%.17g,%.17g,%.17g,%.17g,%.17g,%.17g",-$2,-$3,-$4,-$5,-$6,-$7}' ac.dat >> "$output/vbic_ihp_thermal.csv"
            awk 'NR==1{first=$2; low=$1} NR==2{printf ",%.17g\n",(first-$2)/($1-low)}' slope.dat >> "$output/vbic_ihp_thermal.csv"
        done
    done
done

# Excess phase: independently measure the two-state response with TD enabled.
phase_card=$(sed -E 's/(^| )td=[^ ]+/ td=2e-12/' <<< "$card")
echo '# temperature_celsius,base,collector,substrate,frequency,ic,ib,ie,is,ci,ac_ic_real,ac_ic_imag,ac_ib_real,ac_ib_imag' > "$output/vbic_ihp_phase.csv"
for temperature in 27 85; do
    for frequency in 1e9 1e11; do
        cat > phase.cir <<DECK
IHP excess phase response
Vc c 0 1.2
Vb b 0 0.78 AC 1
Ve e 0 0
Vs s 0 -0.1
Q1 c b e s 0 REF
.model REF npn $phase_card
.temp $temperature
.options gmin=1e-18 reltol=1e-9 abstol=1e-15 vntol=1e-12
.control
set numdgt=17
set wr_singlescale
op
wrdata op.dat i(vc) i(vb) i(ve) i(vs) v(q1#collCI)
ac lin 1 $frequency $frequency
wrdata ac.dat i(vc) i(vb)
quit
.endc
.end
DECK
        rm -f op.dat ac.dat
        ngspice -b -n phase.cir > phase.log 2>&1
        if rg -qi 'error:|unrecognized parameter|no convergence|timestep too small' phase.log || [[ ! -s op.dat || ! -s ac.dat ]]; then
            cat phase.log >&2
            exit 1
        fi
        awk -v prefix="$temperature,0.78,1.2,-0.1,$frequency" 'NR==1{printf "%s,%.17g,%.17g,%.17g,%.17g,%.17g",prefix,-$2,-$3,-$4,-$5,$6}' op.dat >> "$output/vbic_ihp_phase.csv"
        awk 'NR==1{printf ",%.17g,%.17g,%.17g,%.17g\n",-$2,-$3,-$4,-$5}' ac.dat >> "$output/vbic_ihp_phase.csv"
    done
done

# Unforced thermal terminal: solve the full electrical/thermal equilibrium.
echo '# temperature_celsius,base,collector,substrate,ic,ib,ie,is,rise,ci' > "$output/vbic_ihp_equilibrium.csv"
for temperature in 27 85; do
    for base in 0.65 0.78; do
        cat > equilibrium.cir <<DECK
IHP self-heating equilibrium
Vc c 0 1.2
Vb b 0 $base
Ve e 0 0
Vs s 0 -0.1
Q1 c b e s t REF
.model REF npn $thermal_card selft=1
.temp $temperature
.options gmin=1e-18 reltol=1e-9 abstol=1e-15 vntol=1e-12
.control
set numdgt=17
set wr_singlescale
op
wrdata op.dat i(vc) i(vb) i(ve) i(vs) v(t) v(q1#collCI)
quit
.endc
.end
DECK
        rm -f op.dat
        ngspice -b -n equilibrium.cir > equilibrium.log 2>&1
        if rg -qi 'error:|unrecognized parameter|no convergence|timestep too small' equilibrium.log || [[ ! -s op.dat ]]; then
            cat equilibrium.log >&2
            exit 1
        fi
        awk -v prefix="$temperature,$base,1.2,-0.1" 'NR==1{printf "%s,%.17g,%.17g,%.17g,%.17g,%.17g,%.17g\n",prefix,-$2,-$3,-$4,-$5,$6,$7}' op.dat >> "$output/vbic_ihp_equilibrium.csv"
    done
done
