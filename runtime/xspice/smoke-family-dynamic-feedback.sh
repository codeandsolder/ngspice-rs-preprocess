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
work="$(mktemp -d "$scratch/ngspice_preprocess_dynamic_feedback_XXXXXXXX")"
trap 'rm -rf -- "$work"' EXIT
fixture="$here/fixtures/family/dynamic-sensed-contention"
"$bins/alb-logical-expand" "$fixture.cir" "$fixture.json" \
    "$work/lowered.cir" "$work/generated.inc"
"$bins/alb-family-autowire" "$work/lowered.cir" "$work/generated.inc" \
    "$fixture.json" "$here/fixtures/family/auto-models.json" "$work/base.cir"

rg -q '^\.subckt TOP PIN A B OBSERVE$' "$work/base.cir"
rg -q '^Xone PIN A B OBSERVE TOP$' "$work/base.cir"
rg -q '^\.subckt GATE .*SR_PROMOTED_OUT_0.*SR_PROMOTED_OUT_1' "$work/base.cir"
rg -q '^\.subckt GROUP .*SR_PROMOTED_OUT_0.*SR_PROMOTED_OUT_1' "$work/base.cir"
rg -q '^Xgateway .*D_SENSE D_SENSE_SR_SINK D_SENSE_SR_SOURCE.*D_OBSERVE D_OBSERVE_SR_SINK D_OBSERVE_SR_SOURCE.*D_LEFT D_LEFT_SR_SINK D_LEFT_SR_SOURCE.*D_RIGHT D_RIGHT_SR_SINK D_RIGHT_SR_SOURCE GATE$' "$work/base.cir"
rg -q '^Xidle .*SPARE SPARE_SR_SINK SPARE_SR_SOURCE.*IDLE_SENSE IDLE_SENSE_SR_SINK IDLE_SENSE_SR_SOURCE.*GROUP$' "$work/base.cir"
for state in D_LEFT D_RIGHT; do
    rg -q "^AsrFamily[[:digit:]]+ $state ${state}_SR_SINK ${state}_SR_SOURCE %gd\(PIN 0\)" "$work/base.cir" || {
        echo "Missing independently driven PIN DAC for $state" >&2
        exit 1
    }
done
rg -q '^AsrFamily[[:digit:]]+ %gd\(PIN 0\) D_SENSE D_SENSE_SR_SINK D_SENSE_SR_SOURCE' "$work/base.cir"
rg -q '^AsrFamily[[:digit:]]+ D_OBSERVE D_OBSERVE_SR_SINK D_OBSERVE_SR_SOURCE %gd\(OBSERVE 0\)' "$work/base.cir"
[[ $(rg -c 'sr_family_adc\(' "$work/base.cir") == 3 ]] ||
    { echo 'Expected three independent physical ADCs (A, B, PIN)' >&2; exit 1; }
[[ $(rg -c 'sr_family_dac\(' "$work/base.cir") == 3 ]] ||
    { echo 'Expected three physical DACs (LEFT, RIGHT, OBSERVE)' >&2; exit 1; }
[[ $(rg -c 'sr_bundle_load\(' "$work/generated.inc") == 2 ]] ||
    { echo 'Expected separate ALB load models for both gate and monitor' >&2; exit 1; }

printf 'codemodel %s\n' "$cm" > "$work/.spiceinit"
for shunt in 35 52 80; do
    # Reuse identical compiled ALB/family models: vary only the physical load.
    sed "s/^Rshunt shunt 0 52$/Rshunt shunt 0 $shunt/" "$work/base.cir" > "$work/circuit.cir"
    rg -q "^Rshunt shunt 0 $shunt$" "$work/circuit.cir"
    (cd "$work"; ngspice -b circuit.cir >sim.log 2>&1) ||
        { tail -n 120 "$work/sim.log" >&2; exit 1; }
    if rg -i 'fatal error|timestep too small|error:|no such model' "$work/sim.log"; then
        echo "Native family ADC/DAC transient error with $shunt ohm load" >&2
        exit 1
    fi
    test -s "$work/result.dat"
    # Columns: TIME PIN, TIME A, TIME B, TIME OBSERVE, TIME GATE.
    # Two HIGH DACs current-limit at I=(3.3-2.75)*0.025 A each.
    # The enabled shunt is Rshunt + switch Ron=0.05 ohm, in parallel
    # with 100 kohm, so the predicted pin voltage includes both sources.
    # A HIGH/LOW fight adds 40-ohm pulldown conductance in parallel.
    # ADC thresholds scale from nominal 5 V HC to 3.3 V: 1.386..1.452 V.
    awk -v r="$shunt" '
      function near(actual, expect, tolerance) {
        return actual > expect-tolerance && actual < expect+tolerance
      }
      BEGIN {
        g_hi=.025; g_lo=.025; high=3.3; knee=2.75;
        s=.05+r; g_load=1/s+1/100000;
        i_hi=g_hi*(high-knee);
        eq_both=2*i_hi/g_load;
        eq_opposed=i_hi/(g_lo+g_load);
        adc_low=2.1*3.3/5; adc_high=2.2*3.3/5;
        # The physical HC ADC still reports UNKNOWN in the threshold band,
        # but the source-compiled SIMetrix ALB expression coerces X to LOW.
        # That differs from the old generic consensus experiment (0.54956 V).
        expect_monitor=(eq_both>adc_high ? 3.3 : 0);
        printf "R=%d ohm: predicted loaded bus %.5f V, ALB monitor %.5f V, opposing-drive bus %.5f V\n",r,eq_both,expect_monitor,eq_opposed;
      }
      {
        t=$1*1e9; v=$2; a=$4; b=$6; observe=$8; gate=$10;
        if (t>8.3 && t<9 && v<.025 && observe<.12 && gate<.1) initial=1;
        if (t>24.5 && t<25.5 && near(v,.54,.055) && observe<.12 && a>3.2 && b<.1) opposing=1;
        if (t>42.5 && t<43.5 && v>3.15 && observe>3.1 && a>3.2 && b>3.2 && gate<.1) high_phase=1;
        if (t>46 && t<47 && v>eq_both+.20 && v<3.31 && gate>3.2) capacitive=1;
        if (t>69.5 && t<70.5 && near(v,eq_both,.04) && near(observe,expect_monitor,.12) && gate>3.2) loaded=1;
        if (t>84.5 && t<85.5 && v>3.25 && observe>3.1 && gate<.1) release=1;
        if (t>103.5 && t<104.5 && near(v,eq_opposed,.045) && observe<.12 && gate>3.2) low_loaded=1;
        if (t>113.5 && t<114.5 && near(v,.53,.07) && observe<.12 && gate<.1) low_release=1;
        if (t>134.5 && t<135.5 && v<.05 && observe<.12 && a<.1 && b<.1) end_low=1;
        if (r==52 && NR>1) {
          if (t>48 && t<72 && prior_v>adc_high && v<=adc_high && !down_v) down_v=t;
          if (t>48 && t<72 && prior_o>1.5 && observe<=1.5 && !down_o) down_o=t;
          if (t>72 && t<85 && prior_v<adc_high && v>=adc_high && !up_v) up_v=t;
          if (t>72 && t<85 && prior_o<1.5 && observe>=1.5 && !up_o) up_o=t;
        }
        prior_v=v; prior_o=observe;
      }
      END {
        if (!initial || !opposing || !high_phase || !capacitive || !loaded ||
            !release || !low_loaded || !low_release || !end_low) {
          printf "R=%d: failed phases initial=%d opposing=%d both_high=%d capacitance=%d load=%d release=%d contested_load=%d contested_release=%d final=%d\n",r,initial,opposing,high_phase,capacitive,loaded,release,low_loaded,low_release,end_low > "/dev/stderr";
          exit 1;
        }
        if (r==52) {
          # The ADC and the sensing ALB must respond *after* the analog
          # threshold crossing, but within the known 1ns ALB + 2ns DAC
          # pipeline rather than waiting until the next phase.
          if (!down_v || !down_o || !up_v || !up_o ||
              down_o-down_v<.4 || down_o-down_v>3.5 ||
              up_o-up_v<.4 || up_o-up_v>3.5) {
            printf "ADC transient edge latency wrong: down=(%.4f %.4f) up=(%.4f %.4f) ns\n",down_v,down_o,up_v,up_o > "/dev/stderr";
            exit 1;
          }
          printf "ADC threshold crossing and readback edges: down %.3f -> %.3f ns, up %.3f -> %.3f ns\n",down_v,down_o,up_v,up_o;
        }
      }
    ' "$work/result.dat" || { echo "Failed native dynamic physical-load/readback regression at R=$shunt" >&2; exit 1; }
done
echo '3/3 ngspice dynamic physical-load ADC sensing and dual-DAC transient regressions pass'
