#!/usr/bin/env bash
# Verify a physically switched analog capacitor while a load-sensitive
# XSPICE digital event is already pending. The digital 2pF LOAD() is
# static at INIT: its event is not rescheduled when the *analog* switch
# closes, but the true analog family DAC must charge a real 1..300pF
# capacitor through its 33-ohm switch and satisfy measured KCL.
set -euo pipefail
if (( $# != 2 )); then
    printf 'usage: %s CODEMODEL_CM SCRATCH_ROOT\n' "$0" >&2
    exit 2
fi
here="$(cd -- "$(dirname -- "$0")" && pwd)"
cm="$(realpath -- "$1")"
scratch="$(realpath -- "$2")"
for cmd in ngspice rg awk sed; do command -v "$cmd" >/dev/null; done
test -s "$cm"
work="$(mktemp -d "$scratch/ngspice_preprocess_switched_cap_XXXXXXXX")"
trap 'rm -rf -- "$work"' EXIT
fixture="$here/smokes/switched-capacitance-pending.cir"
oracle="$here/check-switched-capacitance.awk"
test -s "$fixture" && test -s "$oracle"

simulate() {
    local dir="$1"
    (
        cd "$dir"
        ngspice -b circuit.cir >run.log 2>&1 || {
            tail -105 run.log >&2
            exit 1
        }
        if rg -i 'fatal error|error:|timestep too small|no such model' run.log; then
            echo "Native XSPICE switched-cap circuit failed: $dir" >&2
            exit 1
        fi
        test -s result.dat && test -s digital-events.txt
    )
}

for cap_pf in 1 150 300; do
    dir="$work/cap-$cap_pf"
    mkdir -p -- "$dir"
    cap_value="$cap_pf"p
    sed "s/^\.param CCAP=150p$/.param CCAP=$cap_value/" \
        "$fixture" >"$dir/circuit.cir"
    printf 'codemodel %s\n' "$cm" >"$dir/.spiceinit"
    simulate "$dir"
    awk -v cap_pf="$cap_pf" -f "$oracle" \
        "$dir/digital-events.txt" "$dir/result.dat"
done

# Change actual native XSPICE source/physical SPICE elements and rerun
# them; a hardcoded smoke checker that happened to accept the original
# waveform is not adequate without these falsification probes.
for corrupt in no-digital-receiver incorrect-stage-delay incorrect-output-resistance \
               delayed-load-switch incorrect-switch-resistance altered-capacitor \
               reduced-source-current reversed-current-probe removed-capacitor-path \
               spurious-cap-leak changed-analog-slew delayed-cap-release; do
    dir="$work/bad-$corrupt"
    mkdir -p -- "$dir"
    cp -- "$fixture" "$dir/circuit.cir"
    printf 'codemodel %s\n' "$cm" >"$dir/.spiceinit"
    case "$corrupt" in
        no-digital-receiver)
            sed -i 's/sr_bundle_load(capacitance=2p/sr_bundle_load(capacitance=0/' "$dir/circuit.cir" ;;
        incorrect-stage-delay)
            sed -i 's/delay=5n input_load/delay=4n input_load/' "$dir/circuit.cir" ;;
        incorrect-output-resistance)
            sed -i 's/out_res_pos=1k out_res_neg=1k ic=0/out_res_pos=0 out_res_neg=0 ic=0/' "$dir/circuit.cir" ;;
        delayed-load-switch)
            sed -i 's/12n 0 12.1n 3.3/20n 0 20.1n 3.3/' "$dir/circuit.cir" ;;
        incorrect-switch-resistance)
            sed -i 's/SW(Ron=33 /SW(Ron=15 /' "$dir/circuit.cir" ;;
        altered-capacitor)
            sed -i 's/^\.param CCAP=150p$/.param CCAP=300p/' "$dir/circuit.cir" ;;
        reduced-source-current)
            sed -i 's/g_pullup=.024 /g_pullup=.012 /' "$dir/circuit.cir" ;;
        reversed-current-probe)
            sed -i 's/Vcapsense capnode capterminal 0/Vcapsense capterminal capnode 0/' "$dir/circuit.cir" ;;
        removed-capacitor-path)
            sed -i 's/Sload vout capnode gate 0/Sload 0 capnode gate 0/' "$dir/circuit.cir" ;;
        spurious-cap-leak)
            sed -i 's/Rleak capterminal 0 10Meg/Rleak capterminal 0 10k/' "$dir/circuit.cir" ;;
        changed-analog-slew)
            sed -i 's/t_rise=50p t_fall=50p/t_rise=2n t_fall=50p/' "$dir/circuit.cir" ;;
        delayed-cap-release)
            sed -i 's/50n 3.3 50.1n 0/64n 3.3 64.1n 0/' "$dir/circuit.cir" ;;
    esac
    rg -q '^\.param CCAP=' "$dir/circuit.cir"
    simulate "$dir"
    if awk -v cap_pf=150 -f "$oracle" \
        "$dir/digital-events.txt" "$dir/result.dat" >"$dir/rejected.log" 2>&1; then
        printf 'Real switched-capacitor/clock/source mutation escaped: %s\n' "$corrupt" >&2
        exit 1
    fi
    printf 'Original XSPICE pending-event physical mutation rejection PASS %s\n' "$corrupt"
done
echo '3/3 native XSPICE 2pF fixed digital LOAD and independently switched analog capacitor circuits PASS; 12/12 actual physical mutants rejected'
