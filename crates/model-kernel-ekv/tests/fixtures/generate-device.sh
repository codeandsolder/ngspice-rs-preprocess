#!/usr/bin/env bash
# Execute with the pinned EKV Verilog-A reference compiled as ekv.osdi.
set -euo pipefail
out=reference-device.tsv
printf 'polarity\tcolumn\tv0\tv1\tv2\tv3\tv4\tv5\tv6\tv7\tI0\tI1\tI2\tI3\tI4\tI5\tI6\tI7\tG0\tC0\tG1\tC1\tG2\tC2\tG3\tC3\tG4\tC4\tG5\tC5\tG6\tC6\tG7\tC7\n' > "$out"
while read -r type vd vg vs vb vdp vgp vsp vbp; do
    vto=$(awk -v t="$type" 'BEGIN {print 0.5*t}')
    if [[ $type == 1 ]]; then
        drain='bp dp'; source='bp sp'
    else
        drain='dp bp'; source='sp bp'
    fi
    for col in {0..7}; do
        cat > device-case.cir <<DECK
EKV device independent linearization
.temp 27
.options abstol=1e-22 reltol=1e-10 vntol=1e-12 gmin=1e-30
.model ekv ekv_va type=$type vto=$vto l=2.98u w=7.99u m=2 ns=3 tnom=27 xd_js=0 xd_jsw=0 xd_jswg=0 xd_cj=0 xd_cjsw=0 xd_cjswg=0
.model dba D(IS=2e-13 N=1.5 CJO=6e-14 VJ=0.8 M=0.5 FC=0.5 TT=3e-9 TNOM=27 XTI=0)
.model dsw D(IS=2e-13 N=1.5 CJO=1e-14 VJ=0.7 M=0.33 FC=0.5 TT=3e-9 TNOM=27 XTI=0)
.model sba D(IS=3e-13 N=1.5 CJO=8e-14 VJ=0.8 M=0.5 FC=0.5 TT=3e-9 TNOM=27 XTI=0)
.model ssw D(IS=3e-13 N=1.5 CJO=2e-14 VJ=0.7 M=0.33 FC=0.5 TT=3e-9 TNOM=27 XTI=0)
V0 d 0 $vd AC $((col == 0))
V1 g 0 $vg AC $((col == 1))
V2 s 0 $vs AC $((col == 2))
V3 b 0 $vb AC $((col == 3))
V4 dp 0 $vdp AC $((col == 4))
V5 gp 0 $vgp AC $((col == 5))
V6 sp 0 $vsp AC $((col == 6))
V7 bp 0 $vbp AC $((col == 7))
N1 dp gp sp bp ekv
Rd d dp 8.5
Rg g gp 14.5
Rs s sp 6.5
Rb b bp 21.5
Rds dp sp 250k
Dda $drain dba
Dds $drain dsw
Dsa $source sba
Dss $source ssw
Cgd gp dp 8e-16
Cgs gp sp 1.6e-15
Cgb gp bp 1.8e-15
.control
pre_osdi ./ekv.osdi
set wr_singlescale
set numdgt=16
op
wrdata device-op.tsv i(v0) i(v1) i(v2) i(v3) i(v4) i(v5) i(v6) i(v7)
ac lin 1 1e6 1e6
wrdata device-ac.tsv i(v0) i(v1) i(v2) i(v3) i(v4) i(v5) i(v6) i(v7)
quit
.endc
.end
DECK
        ngspice -b device-case.cir > device-case.log 2>&1
        test "$(wc -l < device-op.tsv)" -eq 1
        test "$(wc -l < device-ac.tsv)" -eq 1
        printf '%s\t' "$type" "$col" "$vd" "$vg" "$vs" "$vb" "$vdp" "$vgp" "$vsp" "$vbp" >> "$out"
        awk '{for(i=2;i<=9;i++) printf "%.17g\t",-$i}' device-op.tsv >> "$out"
        awk '{for(i=2;i<=17;i++) printf "%.17g%s", (i%2==0?-$i:-$i/(2*atan2(0,-1)*$1)), (i==17?"\n":"\t")}' device-ac.tsv >> "$out"
    done
done <<'CASES'
1 1.01 0.8 0.02 -0.21 1 0.75 0 -0.2
1 -0.15 0.9 0.1 0.25 -0.16 0.85 0.12 0.28
-1 -1.02 -0.8 0.03 0.2 -1 -0.75 0 0.23
-1 0.14 -0.9 -0.11 -0.27 0.16 -0.85 -0.12 -0.28
CASES
