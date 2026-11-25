#!/usr/bin/env bash
set -euo pipefail
if (( $# != 3 )); then
    printf 'usage: %s CODEMODEL_CM RUST_BIN_DIR SCRATCH_ROOT\n' "$0" >&2
    exit 2
fi
here="$(cd -- "$(dirname -- "$0")" && pwd)"
cm="$(realpath -- "$1")"
bins="$(realpath -- "$2")"
scratch="$(realpath -- "$3")"
for tool in ngspice rg awk sed; do command -v "$tool" >/dev/null; done
for tool in alb-logical-expand alb-family-autowire; do
    test -x "$bins/$tool" || { echo "Missing $tool" >&2; exit 1; }
done
work="$(mktemp -d "$scratch/ngspice_preprocess_vendor_univ_XXXXXXXX")"
trap 'rm -rf -- "$work"' EXIT
fixture="$here/fixtures/family"
"$bins/alb-logical-expand" "$fixture/dynamic-sensed-contention.cir" \
    "$fixture/dynamic-vendor-univ.json" "$work/lowered.cir" "$work/generated.inc"
"$bins/alb-family-autowire" "$work/lowered.cir" "$work/generated.inc" \
    "$fixture/dynamic-vendor-univ.json" "$fixture/vendor-univ-models.json" "$work/base.cir"
[[ $(rg -c 'sr_family_adc\(' "$work/base.cir") == 3 ]] ||
    { echo 'Expected three UNIV input bridges' >&2; exit 1; }
[[ $(rg -c 'sr_family_dac\(' "$work/base.cir") == 3 ]] ||
    { echo 'Expected three UNIV output bridges' >&2; exit 1; }
[[ $(rg -c 'schmitt=true' "$work/base.cir") == 3 ]] ||
    { echo 'Vendor UNIV ADCs must retain their previous bit in the threshold band' >&2; exit 1; }
[[ $(rg -F -c 'input_load=-3.1e-11' "$work/base.cir") == 3 ]] ||
    { echo 'Vendor negative DAC capacitance compensation must remain intact' >&2; exit 1; }
for shunt in 18 20 22 52; do
    for mode in switched startloaded; do
        [[ $shunt == 52 && $mode == startloaded ]] && continue
        sed "s/^Rshunt shunt 0 52$/Rshunt shunt 0 $shunt/" \
            "$work/base.cir" > "$work/circuit.cir"
        if [[ $mode == startloaded ]]; then
            sed -i 's/^Vloadctl gate 0 PWL(.*$/Vloadctl gate 0 DC 3.3/' "$work/circuit.cir"
        fi
        printf 'codemodel %s\n' "$cm" > "$work/.spiceinit"
        (cd "$work"; ngspice -b circuit.cir >sim.log 2>&1) ||
            { tail -n 110 "$work/sim.log" >&2; exit 1; }
        if rg -i 'fatal error|timestep too small|error:|output delay < 0' "$work/sim.log"; then
            echo "Native UNIV model rejected $mode $shunt ohm load" >&2
            exit 1
        fi
        test -s "$work/result.dat"
        awk -v r="$shunt" -v mode="$mode" '
          BEGIN {
            expect_v=(r==18 ? 1.9490481265 : r==20 ? 2.1649658375 :
                      r==22 ? 2.3728854212 : 3.57024);
            expect_o=(r==18 || (r==20 && mode=="startloaded") ? 0 : 4.9979175344);
            initial_seen=0; settled_seen=0;
          }
          NR==1 {
            # With the originally unspecified ALB IC, the combinatorial
            # identity is resolved from the DC input, not UNKNOWN drive.
            if ($2<-.01 || $2>.01 || $8<-.01 || $8>.01) {
              printf "bad vendor-UNIV DC initialization: pin=%f observe=%f\n",$2,$8 > "/dev/stderr";
              exit 1;
            }
            initial_seen=1;
          }
          {
            t=$1*1e9;
            if (t>=64.9 && t<=65.1 &&
                ($2-expect_v)<.015 && ($2-expect_v)>-.015 &&
                ($8-expect_o)<.025 && ($8-expect_o)>-.025) settled_seen=1;
          }
          END {
            if (!initial_seen || !settled_seen) {
              printf "missing vendor-UNIV %s %d-ohm equilibrium (pin %.6f observe %.6f)\n",mode,r,expect_v,expect_o > "/dev/stderr";
              exit 1;
            }
          }
        ' "$work/result.dat" || exit 1
        if [[ $mode == switched && ( $shunt == 20 || $shunt == 52 ) ]]; then
            awk -v shunt="$shunt" -f "$here/verify-vendor-univ-edges.awk" \
                "$fixture/vendor-univ-edge-reference.csv" "$work/result.dat"
        fi
        printf 'UNIV reference: %s %d ohm initial DC, loaded PIN/OBSERVE passed\n' \
            "$mode" "$shunt"
    done
done
echo '7/7 native vendor-UNIV physical equilibria, ADC hysteresis, and DC startup regressions passed'
