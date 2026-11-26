#!/usr/bin/env bash
# Native ngspice 47 forced-junction-temperature DC and AC source-current oracle.
# Ideal terminal drives exclude global circuit-specific convergence policies.
set -euo pipefail
scratch=$(mktemp -d /tmp/hicum2-device-thermal.XXXXXX)
trap 'rm -rf "$scratch"' EXIT
printf '%s\n' '# flsh,flnqs,ambient_celsius,rise_kelvin,vbe,drive,hz,ic,ib,ie,is,heat,ic_re,ic_im,ib_re,ib_im,heat_re,heat_im,dc_dic,dc_dib,dc_dheat,quasistatic_ic,quasistatic_ib,quasistatic_heat,tf'
for mode in 1 2; do
    for nqs in 0 1; do
        for ambient in -40 27 85; do
            for rise in 0 10; do
                for bias in .65 .75; do
                    for drive in b t; do
                        b_ac=0
                        t_ac=0
                        if [[ $drive == b ]]; then b_ac=1; else t_ac=1; fi
                        if [[ $drive == b ]]; then
                            sweep=Vb
                            step=0.00001
                            center=$bias
                        else
                            sweep=Vt
                            step=0.001
                            center=$rise
                        fi
                        read -r lower upper <<< "$(awk -v c="$center" -v h="$step" 'BEGIN {printf "%.12f %.12f",c-h,c+h}')"
                        cat > "$scratch/deck.cir" <<DECK
Native HICUM2 thermal port
Vc c 0 1.2
Vb b 0 $bias AC $b_ac
Ve e 0 0
Vs s 0 -0.1
Vt t 0 $rise AC $t_ac
Q1 c b e s t H2
.model H2 npn level=8 tnom=27 c10=2e-30 qp0=2e-14
+ flcomp=2.3 flsh=$mode rth=200 cth=1e-9 alrth=.0003 zetarth=.5
+ rbi0=0 rcx=0 rbx=0 re=0 rsu=0
+ favl=0 ibeps=0 ireps=0 ibcxs=0 itss=0 iscs=0
+ t0=5e-12 tr=1e-12 flnqs=$nqs alit=.5 alqf=.5
+ cjei0=2.5e-15 cjci0=1.4e-15 cjep0=1e-15 cjcx0=0.9e-15 cjs0=0.4e-15
.temp $ambient
.options gmin=1e-18 reltol=1e-9 abstol=1e-15 vntol=1e-12
.control
set numdgt=17
set wr_singlescale
op
wrdata $scratch/op.dat i(vc) i(vb) i(ve) i(vs) i(vt)
print @q1[tf]
ac lin 1 1 1
wrdata $scratch/static.dat i(vc) i(vb) i(vt)
ac dec 1 1e6 1e9
wrdata $scratch/ac.dat i(vc) i(vb) i(vt)
dc $sweep $lower $upper $step
wrdata $scratch/sweep.dat i(vc) i(vb) i(vt)
quit
.endc
.end
DECK
                        ngspice -b -n "$scratch/deck.cir" > "$scratch/log" 2>&1
                        if rg -qi 'error:|unrecognized parameter|fatal error|failed' "$scratch/log" ||
                            [[ $(wc -l < "$scratch/ac.dat") -ne 4 ]]; then
                            cat "$scratch/log" >&2
                            exit 1
                        fi
                        tf=$(awk '$1=="@q1[tf]" && $2=="=" {print $3}' "$scratch/log")
                        [[ -n $tf && $(wc -l < "$scratch/sweep.dat") -eq 3 ]]
                        awk -v prefix="$mode,$nqs,$ambient,$rise,$bias,$drive" -v tf="$tf" '
                            FILENAME==ARGV[1] {for(i=2;i<=6;i++) dc[i]=-$i; next}
                            FILENAME==ARGV[2] {for(i=1;i<=3;i++) qs[i]=-$(2*i); next}
                            FILENAME==ARGV[3] {
                                if(FNR==1) {lo=$1; for(i=1;i<=3;i++) left[i]=-$(i+1)}
                                if(FNR==3) {for(i=1;i<=3;i++) derivative[i]=(-$(i+1)-left[i])/($1-lo)}
                                next
                            }
                            {printf "%s,%.17e",prefix,$1
                             for(i=2;i<=6;i++) printf ",%.17e",dc[i]
                             for(i=2;i<=7;i++) printf ",%.17e",-$i
                             for(i=1;i<=3;i++) printf ",%.17e",derivative[i]
                             for(i=1;i<=3;i++) printf ",%.17e",qs[i]
                             printf ",%.17e\n",tf}
                        ' "$scratch/op.dat" "$scratch/static.dat" "$scratch/sweep.dat" "$scratch/ac.dat"
                    done
                done
            done
        done
    done
done
