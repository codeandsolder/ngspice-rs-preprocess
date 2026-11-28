#!/usr/bin/env bash
# Original SIMetrix Elements 9.40d HC_DAC switching-current and voltage
# oracles. The current experiments use fixed 0/2.5/4V outputs to separate
# the DAC's intrinsic switching law from the surrounding electrical load.
set -euo pipefail
here="$(cd -- "$(dirname -- "$0")" && pwd)"
# shellcheck source=runtime/xspice/prepare-original-smoke.sh
source "$here/prepare-original-smoke.sh"
prepare_original_smoke "hc_dac_oracle" "$@"
compile_original() {
    local original="$1" dir="$2" save="$3"
    mkdir -p "$dir"
    awk 'toupper($1)!=".PRINT" && toupper($1)!=".END" {print}' \
        "$original" >"$dir/original.cir"
    cat >>"$dir/original.cir" <<CIR
.control
run
wrdata $save
quit
.endc
.end
CIR
    "$bins/alb-boundary-discover" \
        "$dir/original.cir" "$here/fixtures/family/auto-discovery-5v.json" \
        "$dir/discovered.json"
    "$bins/alb-logical-expand" \
        "$dir/original.cir" "$dir/discovered.json" \
        "$dir/lowered.cir" "$dir/generated.inc"
    "$bins/alb-family-autowire" \
        "$dir/lowered.cir" "$dir/generated.inc" \
        "$dir/discovered.json" \
        "$here/fixtures/family/auto-models.json" "$dir/circuit.cir"
    [[ $(rg -c 'sr_family_dac\(' "$dir/circuit.cir") == 2 ]] ||
        { echo 'Original bidirectional model must retain two distinct physical DACs' >&2; exit 1; }
    printf 'codemodel %s\n' "$cm" >"$dir/.spiceinit"
    (
        cd "$dir"
        ngspice -b circuit.cir >run.log 2>&1 || {
            tail -n 90 run.log >&2; exit 1;
        }
        if rg -i 'fatal error|timestep too small|error:|no such model' run.log; then
            echo 'Original HC DAC source failed in native ngspice' >&2
            exit 1
        fi
        test -s data.dat
    )
}
base="$here/fixtures/vendor-bidir"

# 3 independently run actual proprietary voltage-clamped source oracles.
# The signed current includes active drive in BOTH directions. This catches
# the LOW current pulse during Z->HIGH which a pure HIGH ramp cannot model.
count=0
for clamp in 0 2.5 4; do
    dir="$work/clamped-$clamp"
    source="$base/clamped-current/clamped-$clamp.net"
    compile_original "$source" "$dir" 'data.dat i(Vforce) v(oe) v(pin)'
    awk -v clamp="$clamp" \
        -v reference="$base/clamped-current/clamped-current-reference.csv" '
      BEGIN {
        while ((getline record < reference)>0) {
          split(record, fields, ",")
          if(fields[1]!="clamp_v" && fields[1]+0==clamp+0) {
             desired[fields[2]+0]=fields[3]+0
             required++
          }
        }
        close(reference)
      }
      {
        time=$1*1e9
        current=$2
        if (have_previous) {
          for (key in desired) {
            sample=key+0
            if (!(key in measured) && previous_time<=sample &&
                time>=sample && time>previous_time) {
              value=previous_current+(current-previous_current)*(sample-previous_time)/(time-previous_time)
              discrepancy=value-desired[key]
              if (discrepancy<0) discrepancy=-discrepancy
              if (discrepancy>largest_error) largest_error=discrepancy
              # The independent analog ADC event starts ~12ps earlier
              # natively. Keep all sample times unmodified and allow
              # the resulting maximum raw current deviation of 1.1mA.
              # Separate three-load midpoint checks below are <25ps.
              if (discrepancy>0.0011) {
                printf "HC current mismatch: clamp=%g t=%.3f vendor=%+.9f native=%+.9f error=%g A\n",
                    clamp,sample,desired[key],value,discrepancy > "/dev/stderr"
                errors++
              }
              measured[key]=1
              compared++
            }
          }
        }
        have_previous=1
        previous_time=time
        previous_current=current
      }
      END {
        printf "CLAMP=%g samples=%d largest unaligned current delta=%.6f mA\n",
            clamp,compared,largest_error*1000
        if(required!=33 || compared!=required || errors)exit 1
      }' "$dir/data.dat" ||
        { echo "Original SIMetrix HC clamped current oracle failed for V=$clamp" >&2; exit 1; }
    count=$((count+1))
done

# Three original-source external physical loads, each independently run
# in Elements. Verify 10/25/50/75/90% rise points (15 in all) *without*
# fitting or shifting the time axis.
for ohm in 100 1000 10000; do
    dir="$work/load-$ohm"
    mkdir -p "$dir"
    sed "s/^Rext ext pin 1000$/Rext ext pin $ohm/" \
        "$base/turnon/turnon-1000.net" >"$dir/vendor-original.net"
    compile_original "$dir/vendor-original.net" "$dir" \
        'data.dat v(pin) v(observe) v(oe) v(ext)'
    awk -v r="$ohm" -v reference="$base/turnon/turnon-fractions.csv" '
      BEGIN {
        while ((getline record < reference)>0) {
          split(record, fields, ",")
          if(fields[1]!="r_ohm" && fields[1]+0==r+0)
             oracle[fields[2]]=fields[3]+0
        }
        close(reference)
        split("0.10 0.25 0.50 0.75 0.90",fractions," ")
      }
      {
        n++
        times[n]=$1*1e9
        volts[n]=$2
        if(times[n]>=25.5 && times[n]<=27) plateau=volts[n]
      }
      END {
        if(plateau<2.0 || plateau>5.01) {
          print "Invalid HC physical high plateau" > "/dev/stderr"
          exit 1
        }
        for(j=1;j<=5;j++) {
          f=fractions[j]
          target=(f+0)*plateau
          crossing=-1
          for(i=2;i<=n;i++) {
            if(times[i]>24)break
            if(times[i]<18)continue
            if(volts[i-1]<target && volts[i]>=target) {
              crossing=times[i-1]+(times[i]-times[i-1])*(target-volts[i-1])/(volts[i]-volts[i-1])
              break
            }
          }
          err=crossing-oracle[f]
          if(err<0)err=-err
          if(err>largest_error)largest_error=err
          if(crossing<0 || !(f in oracle) || err>.025) {
            printf "HC voltage mismatch: R=%g fraction=%s vendor=%.9f native=%.9f err=%.3fps\n",
                r,f,oracle[f],crossing,err*1000 >"/dev/stderr"
            failures++
          }
          compared++
        }
        printf "LOAD=%d ohm verified %d vendor crossing times; max discrepancy %.3f ps\n",
            r,compared,largest_error*1000
        if(compared!=5 || failures)exit 1
      }' "$dir/data.dat" ||
        { echo "Original SIMetrix HC turn-on waveform oracle failed for R=$ohm" >&2; exit 1; }
    count=$((count+1))
done
printf '%s/6 original proprietary HC DAC current/voltage source circuits passed (99 current samples + 15 analog crossings)\n' "$count"
