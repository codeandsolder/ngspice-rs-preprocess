#!/usr/bin/env bash
# Run in a scratch directory with ekv.osdi built from the pinned reference.
set -euo pipefail
out=reference-linearization.tsv
printf 'type\ttemp_c\tlength\twidth\tm\tns\te0\ttheta\tvd\tvg\tvs\tvb\tcolumn\tid\tig\tis\tib\tg_d\tc_d\tg_g\tc_g\tg_s\tc_s\tg_b\tc_b\n' > "$out"
while read -r type temp l w m ns e0 theta vd vg vs vb; do
    vto=$(awk -v t="$type" 'BEGIN {print 0.5*t}')
    for col in 0 1 2 3; do
        cat > case.cir <<DECK
EKV current and AC linearization witness
.temp $temp
.options abstol=1e-22 reltol=1e-10 vntol=1e-12
.model ekv ekv_va type=$type vto=$vto l=$l w=$w m=$m ns=$ns e0=$e0 theta=$theta xd_js=0 xd_jsw=0 xd_jswg=0 xd_cj=0 xd_cjsw=0 xd_cjswg=0
Vd d 0 $vd AC $((col == 0))
Vg g 0 $vg AC $((col == 1))
Vs s 0 $vs AC $((col == 2))
Vb b 0 $vb AC $((col == 3))
N1 d g s b ekv
.control
pre_osdi ./ekv.osdi
set wr_singlescale
set numdgt=16
op
wrdata case-op.tsv i(vd) i(vg) i(vs) i(vb)
ac lin 1 1e6 1e6
wrdata case-ac.tsv i(vd) i(vg) i(vs) i(vb)
quit
.endc
.end
DECK
        ngspice -b case.cir > case.log 2>&1
        test "$(wc -l < case-op.tsv)" -eq 1
        test "$(wc -l < case-ac.tsv)" -eq 1
        printf '%s\t' "$type" "$temp" "$l" "$w" "$m" "$ns" "$e0" "$theta" "$vd" "$vg" "$vs" "$vb" "$col" >> "$out"
        awk '{for(i=2;i<=5;i++) printf "%.17g\t",-$i}' case-op.tsv >> "$out"
        awk '{for(i=2;i<=9;i++) printf "%.17g%s", (i%2==0?-$i:-$i/(2*atan2(0,-1)*$1)), (i==9?"\n":"\t")}' case-ac.tsv >> "$out"
    done
done <<'CASES'
1 27 1e-6 10e-6 1 1 1e8 0 1 0.75 0 0
1 -40 2e-6 4e-6 1 1 1e8 0 0.1 0.35 0 -0.2
1 125 0.18e-6 2e-6 1 1 1e8 0 1.8 1.2 0 -0.3
1 27 1e-6 10e-6 1 1 0 0.2 1 1.4 0 0
1 27 1e-6 10e-6 1 1 1e8 0 -0.8 0.8 0.1 -0.3
1 27 1e-6 10e-6 2 3 1e8 0 2 1.0 0 -0.2
1 27 1e-6 10e-6 1 1 1e8 0 0 0.5 0 0
1 27 1e-6 10e-6 1 1 1e8 0 1 -0.25 0 0
-1 27 1e-6 10e-6 1 1 1e8 0 -1 -0.75 0 0
-1 125 0.18e-6 2e-6 1 1 0 0.2 0.5 -1.2 -0.1 0.3
1 27 1e-6 10e-6 1 1 1e8 0 8 1 0 0
CASES
