#!/usr/bin/env bash
# Reproduce ngspice 47 native HICUM/L2 level=8, model version 2.4.0.
# Compare reported intrinsic B-E/B-C biases, collector current, critical
# current and forward transit time, not nominal source bias alone.
set -euo pipefail
tmpdir=$(mktemp -d /tmp/hicum2-transport-oracle.XXXXXX)
trap 'rm -rf "$tmpdir"' EXIT
printf '%s\n' '# variant,external_vbe,external_vce,internal_vbiei,internal_vbici,ic,ick,tf'
for variant in native historical symmetric elongated high_correction hjei_slope no_lateral no_storage no_collector_storage no_emitter_storage no_barrier historical_no_barrier \
    correction_no_barrier symmetric_no_barrier elongated_no_barrier tiny_no_barrier; do
    geometry='latb=.2 latl=.1'
    weighting='flcomp=2.3'
    correction='ich=0'
    emitter_weighting='ahjei=0 hjei=1'
    storage='thcs=3e-12 tef0=1e-12'
    barrier='vcbar=.05 icbar=.01'
    case "$variant" in
        historical) weighting='flcomp=0' ;;
        symmetric) geometry='latb=.2 latl=.2' ;;
        elongated) geometry='latb=0 latl=.2' ;;
        high_correction) correction='ich=.04' ;;
        hjei_slope) emitter_weighting='ahjei=.5 hjei=1' ;;
        no_lateral) geometry='latb=0 latl=0' ;;
        no_storage) storage='thcs=0 tef0=0' ;;
        no_collector_storage) storage='thcs=0 tef0=1e-12' ;;
        no_emitter_storage) storage='thcs=3e-12 tef0=0' ;;
        no_barrier) barrier='vcbar=0 icbar=.01' ;;
        historical_no_barrier)
            barrier='vcbar=0 icbar=.01'
            weighting='flcomp=0' ;;
        correction_no_barrier)
            barrier='vcbar=0 icbar=.01'
            correction='ich=.04' ;;
        symmetric_no_barrier)
            barrier='vcbar=0 icbar=.01'
            geometry='latb=.2 latl=.2' ;;
        elongated_no_barrier)
            barrier='vcbar=0 icbar=.01'
            geometry='latb=0 latl=.2' ;;
        tiny_no_barrier)
            barrier='vcbar=0 icbar=.01'
            geometry='latb=.005 latl=.002' ;;
    esac
    for biases in '0.55 1.3' '0.65 0.7' '0.75 1.3' '0.85 1.3'         '0.90 1.3' '0.75 0.9' '0.85 0.9' '0.75 2.0' '0.90 2.0'; do
        read -r vbe vce <<< "$biases"
        cat > "$tmpdir/input.cir" <<DECK
Native HICUM L2 2.4.0 current / critical-current / transit-time
VBE b 0 DC $vbe
VCE c 0 DC $vce
Q1 c b 0 0 H2
.model H2 NPN level=8 tnom=27 c10=2e-30 qp0=2e-14 $correction mcf=1
+ t0=2e-12 tr=1e-12 rci0=150 vlim=.5 vces=.1 vpt=100 aick=.001 delck=2
+ cjei0=2.5e-15 vdei=.9 zei=.5 ajei=2.5
+ cjci0=1.4e-15 vdci=.7 zci=.4 vptci=3
+ $storage gtfe=1 ahc=.1 fthc=.6 $geometry
+ hf0=1 hfe=1 hfc=1 $emitter_weighting hjci=1
+ dt0h=0.4e-12 tbvl=.25e-12 $barrier acbar=.01 $weighting
.options gmin=1e-18 temp=27
.control
set numdgt=15
op
print @q1[vbiei] @q1[vbici] @q1[ic] @q1[ick] @q1[tf]
quit
.endc
.end
DECK
        ngspice -b "$tmpdir/input.cir" > "$tmpdir/output" 2>&1 || {
            cat "$tmpdir/output" >&2
            exit 1
        }
        if rg -qi 'Error:|unrecognized parameter|Warning: Model issue|fatal error' "$tmpdir/output" ||
            ! rg -q 'No. of Data Rows : 1' "$tmpdir/output"; then
            cat "$tmpdir/output" >&2
            exit 1
        fi
        awk -v prefix="$variant,$vbe,$vce" '
            BEGIN { printf "%s", prefix }
            /^@q1\[vbiei\] =/ {printf ",%s",$3}
            /^@q1\[vbici\] =/ {printf ",%s",$3}
            /^@q1\[ic\] =/ {printf ",%s",$3}
            /^@q1\[ick\] =/ {printf ",%s",$3}
            /^@q1\[tf\] =/ {printf ",%s",$3}
            END {print ""}
        ' "$tmpdir/output"
    done
done
