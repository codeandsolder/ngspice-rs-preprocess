#!/usr/bin/env bash
# Native ngspice-47 HICUM/L2 level-8 AC vertical NQS transfer oracle.
# At ideal B-E and C-E drive, zero junction/parasitic capacitances,
# the ratio of collector AC currents with enabled/disabled ALIT/ALQF
# isolates Itxf/Itf. Preserve complex source currents in CSV.
set -euo pipefail
work=$(mktemp -d /tmp/hicum2-nqs-ac-oracle.XXXXXX)
trap 'rm -rf "$work"' EXIT

printf '%s\n' '# variant,flcomp,flnqs,alit,alqf,hz,tf,off_re,off_im,on_re,on_im'

while read -r variant flcomp flnqs alit alqf; do
    for pass in reference selected; do
        phase_current=$alit
        phase_charge=$alqf
        if [[ $pass == reference ]]; then
            phase_current=0
            phase_charge=0
        fi
        cat > "$work/deck.cir" <<DECK
HICUM L2 2.4 vertical NQS AC oracle
VBE b 0 DC 0.78 AC 1
VCE c 0 DC 1.3 AC 0
Q1 c b 0 0 H2
.model H2 NPN level=8 tnom=27 c10=2e-30 qp0=2e-14
+ t0=5e-12 tef0=0 tr=0 alit=$phase_current alqf=$phase_charge
+ flnqs=$flnqs flcomp=$flcomp
+ cjei0=0 cjci0=0 cjep0=0 cjcx0=0 cjs0=0 cscp0=0
+ rbi0=0 rbx=0 rcx=0 re=0 rsu=0
.control
set numdgt=15
op
print @q1[tf]
ac dec 3 1e7 1e12
setplot ac1
print vce#branch
quit
.endc
.end
DECK
        ngspice -b "$work/deck.cir" > "$work/output" 2>&1 || {
            cat "$work/output" >&2; exit 1
        }
        if rg -qi 'Error:|unrecognized parameter|Warning: Model issue|fatal error' "$work/output" ||
          ! rg -q 'No. of Data Rows : 16' "$work/output"; then
            cat "$work/output" >&2; exit 1
        fi
        awk '
          $1 ~ /^[0-9]+$/ && $3 ~ /,$/ {
            gsub(/,/, "", $3)
            gsub(/,/, "", $4)
            printf "%.17e,%.17e,%.17e\n", $2, $3, $4
          }
        ' "$work/output" > "$work/$pass.csv"
        if [[ $(wc -l < "$work/$pass.csv") -ne 16 ]]; then
            cat "$work/output" >&2; exit 1
        fi
        if [[ $pass == selected ]]; then
            tf=$(awk '$1 == "@q1[tf]" && $2 == "=" {print $3}' "$work/output")
            if [[ -z $tf ]]; then
                cat "$work/output" >&2; exit 1
            fi
        fi
    done
    paste -d, "$work/reference.csv" "$work/selected.csv" |
        awk -F, -v prefix="$variant,$flcomp,$flnqs,$alit,$alqf" -v tf="$tf" '
          $1 != $4 {print "frequency mismatch" > "/dev/stderr"; exit 1}
          {printf "%s,%s,%s,%s,%s,%s\n", prefix,$1,tf,$2,$3,$5","$6}
        '
done <<'VARIANTS'
modern_both 2.3 1 0.333 0.167
modern_fast 2.3 1 0.1 0
modern_slow 2.3 1 0.8 0.7
modern_charge_only 2.3 1 0 0.5
modern_current_only 2.3 1 0.7 0
modern_disabled 2.3 0 0.333 0.167
modern_no_delay 2.3 1 0 0
legacy_21 2.1 0 0.333 0.167
legacy_0 0 0 0.333 0.167
intermediate_disabled 2.2 0 0.333 0.167
intermediate_enabled 2.2 1 0.333 0.167
VARIANTS
