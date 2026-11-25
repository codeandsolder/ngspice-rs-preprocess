#!/usr/bin/env bash
# Genuine original ngspice47 12-state digital clock + real_delay sample/hold,
# data/clock coincidence and downstream real_gain + 2pF native ALB receiver.
set -euo pipefail
here="$(cd -- "$(dirname -- "$0")" && pwd)"
# shellcheck source=runtime/xspice/prepare-original-smoke.sh
source "$here/prepare-original-smoke.sh"
prepare_original_smoke "clocked_real" "$@"
model="native-original-clocked-real.lib"
stage_original_family_circuit "clocked-real"
for src in original-clocked-real-data.txt original-clocked-real-clock.txt; do
    cp -- "$fixtures/$src" "$work/stock/"
    cp -- "$fixtures/$src" "$work/native/"
done
cp -- "$fixtures/georgiatech-d_source-original.ifs" "$work/native/"
for original in d_to_real real_delay real_gain real_to_v; do
    cp -- "$fixtures/original-xtraevt-$original.ifs" "$work/native/"
done

# The independent JSON port/parameter claims must not overpower original
# IFS definitions or actual source-reachable A-instance wiring.
for corrupt in missing-source scalar-original-clock wrong-original-clock-kind \
               reversed-real-clock clock-as-real real-as-clock null-clock \
               null-real-input wrong-real-delay-kind zero-real-delay \
               duplicate-real-delay unknown-original-gain-parameter; do
    bad="$work/invalid-$corrupt"
    mkdir -p "$bad"
    cp -- "$work/native/original.cir" "$work/native/$model" \
          "$work/native/georgiatech-d_source-original.ifs" \
          "$work/native/original-xtraevt-"*.ifs "$bad/"
    bad_config="$config"
    case "$corrupt" in
        missing-source)
            sed -i 's/AsourceClock \[CLK\]/Amissing [CLK]/' "$bad/$model" ;;
        scalar-original-clock)
            sed -i 's/AsourceClock \[CLK\]/AsourceClock CLK/' "$bad/$model" ;;
        wrong-original-clock-kind)
            sed -i 's/ORIGINAL_CLOCK d_source(/ORIGINAL_CLOCK d_ram(/' "$bad/$model" ;;
        reversed-real-clock)
            sed -i 's/Ahold RDATA CLK RHELD/Ahold CLK RDATA RHELD/' "$bad/$model" ;;
        clock-as-real)
            sed -i 's/Ahold RDATA CLK RHELD/Ahold CLK CLK RHELD/' "$bad/$model"
            jq '(.code_model_contracts[] | select(.instance=="Ahold") | .ports.in)=["CLK"]' \
                "$config" >"$bad/poisoned.json";bad_config="$bad/poisoned.json" ;;
        real-as-clock)
            sed -i 's/Ahold RDATA CLK RHELD/Ahold RDATA RDATA RHELD/' "$bad/$model"
            jq '(.code_model_contracts[] | select(.instance=="Ahold") | .ports.clk)=["RDATA"]' \
                "$config" >"$bad/poisoned.json";bad_config="$bad/poisoned.json" ;;
        null-clock)
            sed -i 's/Ahold RDATA CLK RHELD/Ahold RDATA NULL RHELD/' "$bad/$model"
            jq '(.code_model_contracts[] | select(.instance=="Ahold") | .ports.clk)=["NULL"]' \
                "$config" >"$bad/poisoned.json";bad_config="$bad/poisoned.json" ;;
        null-real-input)
            sed -i 's/Ahold RDATA CLK RHELD/Ahold NULL CLK RHELD/' "$bad/$model"
            jq '(.code_model_contracts[] | select(.instance=="Ahold") | .ports.in)=["NULL"]' \
                "$config" >"$bad/poisoned.json";bad_config="$bad/poisoned.json" ;;
        wrong-real-delay-kind)
            sed -i 's/ORIGINAL_DELAY real_delay(/ORIGINAL_DELAY real_gain(/' "$bad/$model" ;;
        zero-real-delay)
            sed -i 's/real_delay(delay=610p)/real_delay(delay=0)/' "$bad/$model" ;;
        duplicate-real-delay)
            sed -i 's/real_delay(delay=610p)/real_delay(delay=610p delay=2n)/' "$bad/$model" ;;
        unknown-original-gain-parameter)
            sed -i 's/gain=-0.7 out_offset/gain=-0.7 badvalue=0 out_offset/' "$bad/$model" ;;
    esac
    if "$bins/alb-boundary-discover" "$bad/original.cir" "$bad_config" \
        "$bad/discovered.json" >"$bad/rejected.log" 2>&1; then
        printf 'Invalid original clocked REAL IFS accepted: %s\n' "$corrupt" >&2
        exit 1
    fi
    rg -i -q 'IFS|model|port|parameter|binding|native|NULL|type' "$bad/rejected.log" || {
        tail -95 "$bad/rejected.log" >&2
        exit 1
    }
    test ! -e "$bad/discovered.json"
    printf 'Original clocked real IFS rejection PASS %s\n' "$corrupt"
done

"$bins/alb-boundary-discover" "$work/native/original.cir" "$config" \
    "$work/native/discovered.json"
"$bins/alb-logical-expand" "$work/native/original.cir" \
    "$work/native/discovered.json" "$work/native/lowered.cir" \
    "$work/native/generated.inc"
"$bins/alb-family-autowire" "$work/native/lowered.cir" \
    "$work/native/generated.inc" "$work/native/discovered.json" \
    "$fixtures/auto-models.json" "$work/native/circuit.cir"
rg -F -x -q \
  'Xexternal LEN LEN_SR_SINK LEN_SR_SOURCE ANALOG_RAW ANALOG_HELD ANALOG_POST ANALOG_ENABLED ANALOG_PROBE SR_ORIGINAL_CLOCKED_REAL' \
  "$work/native/circuit.cir" || {
    echo 'Original clocked REAL physical X ABI changed' >&2; exit 1;
}
[[ $(rg -F -c 'sr_family_adc(' "$work/native/circuit.cir") == 1 ]] || {
    echo 'Expected one original real-clocked physical ALB input ADC' >&2; exit 1;
}
for line in \
    'AsourceData [DATA] ORIGINAL_DATA' \
    'AsourceClock [CLK] ORIGINAL_CLOCK' \
    'Ahold RDATA CLK RHELD ORIGINAL_DELAY' \
    'Again RHELD RPOST ORIGINAL_GAIN' \
    'Aenabled DATA EN RENABLE ORIGINAL_DENABLE' \
    'Aprobe DATA Q_SINK Q_SOURCE %gd(ANALOG_PROBE 0) ORIGINAL_PROBE'; do
    rg -F -x -q "$line" "$work/native/$model" || {
        printf 'Original C-model instance changed: %s\n' "$line" >&2; exit 1;
    }
done
simulate() {
    local dir="$1" deck="$2"
    (
      cd "$dir"
      ngspice -b "$deck" >run.log 2>&1 || {
          tail -110 run.log >&2
          exit 1
      }
      if rg -i 'error:|fatal error|timestep too small|no such model' run.log; then
          echo "Original clocked REAL C source simulation failed: $dir" >&2
          exit 1
      fi
      test -s original-events.txt && test -s result.dat
    )
}
simulate "$work/stock" original.cir
simulate "$work/native" circuit.cir
verify() {
    awk -f "$here/family-dc-equilibrium.awk" -f "$here/verify-original-clocked-real.awk" \
      "$work/stock/original-events.txt" "$1" \
      "$work/stock/result.dat" "$2"
}
verify "$work/native/original-events.txt" "$work/native/result.dat"

# Actual original C code-model corruption is *run* through ngspice;
# independent original digital event timing and loaded physical results
# must reject any changed data source, clock, original IFS semantics or
# source-compiled physical receiver.
for corrupt in wrong-source-data wrong-clock-strength wrong-clock-type \
               wrong-data-delay wrong-clock-sample-delay wrong-real-gain \
               wrong-post-input wrong-original-clock-input bypass-enable \
               removed-enable-receiver swapped-real-outputs wrong-real-ramp; do
    bad="$work/wrong-physical-$corrupt"
    mkdir -p "$bad"
    cp -- "$work/native/circuit.cir" "$work/native/generated.inc" \
          "$work/native/$model" "$work/native/.spiceinit" \
          "$work/native/original-clocked-real-"*.txt "$bad/"
    case "$corrupt" in
        wrong-source-data)
            sed -i 's/18n Ur/18n 1r/' "$bad/original-clocked-real-data.txt" ;;
        wrong-clock-strength)
            sed -i 's/33n 1z/33n Uz/' "$bad/original-clocked-real-clock.txt" ;;
        wrong-clock-type)
            sed -i 's/80n 1r/80n 0r/' "$bad/original-clocked-real-clock.txt" ;;
        wrong-data-delay)
            sed -i 's/zero=-0.5 one=2 delay=220p/zero=-0.5 one=2 delay=520p/' "$bad/$model" ;;
        wrong-clock-sample-delay)
            sed -i 's/real_delay(delay=610p)/real_delay(delay=1.2n)/' "$bad/$model" ;;
        wrong-real-gain)
            sed -i 's/in_offset=0.25 gain=-0.7/in_offset=0.25 gain=-0.5/' "$bad/$model" ;;
        wrong-post-input)
            sed -i 's/Again RHELD RPOST/Again RDATA RPOST/' "$bad/$model" ;;
        wrong-original-clock-input)
            sed -i 's/Ahold RDATA CLK RHELD/Ahold RDATA DATA RHELD/' "$bad/$model" ;;
        bypass-enable)
            sed -i 's/Aenabled DATA EN RENABLE/Aenabled DATA NULL RENABLE/' "$bad/$model" ;;
        removed-enable-receiver)
            sed -i 's/capacitance=2p/capacitance=0/' "$bad/$model" ;;
        swapped-real-outputs)
            sed -i \
              -e 's/Araw RDATA ANALOG_RAW/Araw RDATA ANALOG_HELD/' \
              -e 's/Aheld RHELD ANALOG_HELD/Aheld RHELD ANALOG_RAW/' "$bad/$model" ;;
        wrong-real-ramp)
            sed -i 's/transition_time=400p/transition_time=2n/' "$bad/$model" ;;
    esac
    simulate "$bad" circuit.cir
    if verify "$bad/original-events.txt" "$bad/result.dat" \
        >"$bad/rejected.log" 2>&1; then
        printf 'Original real-delay actual C-model corruption escaped: %s\n' "$corrupt" >&2
        exit 1
    fi
    printf 'Original clocked real actual-C simulation corruption PASS %s\n' "$corrupt"
done
echo 'AUTO PHYSICAL PASS original-clocked-real-x (original 12-state clock real_delay + exact timing, C-model real_gain, 2pF and five loaded analog outputs)'
