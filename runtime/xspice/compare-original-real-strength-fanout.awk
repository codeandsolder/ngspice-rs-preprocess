# Independent, original-source digital strength -> real events -> two original
# real_gain fanout models -> real_to_v, and a physically biased digital probe.
# Source is original 1991 d_source file, not inferred from native X netlist.
function abs(x) {return x<0?-x:x}
function error(s) {print s > "/dev/stderr"; fail=1}
BEGIN {
    n=14
    for (i=1;i<=n;i++) {
        phase_ns[i]=10+15*(i-1)
        states[i]=((i-1)%4)
        enabled[i]=(i<5 || i>8)
    }
    # 0s and 1s are STRONG, Us is STRONG UNKNOWN and Uz is HI_Z UNKNOWN.
    # Both unknown state strengths map through original d_to_real to 0.75,
    # yet our genuine nonlinear DAC converts their drive strengths to very
    # different physical voltages under a 1k pull-up to 1.2 V.
    value[0]=-.5
    value[1]=2
    value[2]=.75
    value[3]=.75
    probe[0]=.034285714
    probe[1]=3.21542623
    probe[2]=.433333333
    probe[3]=1.2
    route=1
}
FNR==1 && NR>1 {route=2}
{
    ns=$1*1e9
    if(NF!=14 || (FNR>1 && ns+1e-6<last[route])) {
        error(sprintf("malformed original real-strength waveform %s row %d",FILENAME,FNR))
        exit 1
    }
    rows[route]++
    vraw=$2
    vgate=$4
    vpos=$6
    vneg=$8
    vprobe=$10
    current=$12
    v_enable=$14
    if(FNR==1) {
        # Original real_gain DC initialization deliberately differs from its
        # computed first transient gain value (-0.78, 1.0 vs -0.8, 1.0).
        if(abs(vraw+.5)>.00001 || abs(vgate+.5)>.00001 ||
           abs(vpos+.8)>.00001 || abs(vneg-1)>.00001) {
            error(sprintf("real_strength original C DC initialization wrong %s raw=%.6f gate=%.6f gainpos=%.6f gainneg=%.6f",FILENAME,vraw,vgate,vpos,vneg))
            exit 1
        }
    }
    # True Vpull branch current conservation (only the 1k branch touches
    # the supplied 1.2V rail, and the nonlinear DAC drives its own node).
    kcl=abs(current-(vprobe-1.2)*.001)
    if(kcl>max_kcl)max_kcl=kcl
    if(kcl>1e-7) {
        error(sprintf("biased digital probe KCL mismatch %s t=%.5f ns error=%.6g A",FILENAME,ns,kcl))
        exit 1
    }
    for(i=1;i<=n;i++) {
        if(observed[route,i] || abs(ns-phase_ns[i])>.03)continue
        v=value[states[i]]
        want[0]=v
        want[1]=enabled[i]?v:0
        want[2]=1.6*(v+.2)-.3
        want[3]=-1.2*v+.4
        want[4]=probe[states[i]]
        got[0]=vraw
        got[1]=vgate
        got[2]=vpos
        got[3]=vneg
        got[4]=vprobe
        for(k=0;k<5;k++) {
            d=abs(got[k]-want[k])
            # A high-impedance digital strength allows the physical 2pF
            # node to relax towards the 1.2V bias (7 RC time constants).
            tol=(states[i]==3 && k==4)? .009 : .005
            if(d>tol) {
                error(sprintf("real-strength original %s phase %d @ %.2f ns output %d wrong, expected %.6f observed %.6f (delta %.6f)",FILENAME,i,phase_ns[i],k,want[k],got[k],d))
                exit 1
            }
            if(d>max_voltage_error)max_voltage_error=d
        }
        want_enable=enabled[i]?3.3:0
        if(abs(v_enable-want_enable)>.001) {
            error(sprintf("source enable incorrectly driven %s at %.2f ns",FILENAME,phase_ns[i]))
            exit 1
        }
        observed[route,i]=1
        # By design the original digital UNKNOWN is identical as a real
        # number (0.75), yet 0.75V analog from d_to_real cannot reveal
        # whether the original code model emitted STRONG or HI_Z strength.
        # The independently loaded digital probe must discriminate both.
    }
    # Independent enable event changes at 65 and 125 ns. Neither event is
    # concurrent with original d_source data changes, so delay is causal.
    for(j=1;j<=2;j++) {
        timepoint=(j==1?65:125)
        if(FNR<=1 || crossed[route,j] || ns<timepoint || ns>timepoint+4)continue
        prior=old_gate[route]
        rise=(j==1)
        hit=rise ? (prior<-.25 && vgate>=-.25) : (prior>-.25 && vgate<=-.25)
        if(hit) {
            crossed[route,j]=1
            crossings[route,j]=old_time[route]+(ns-old_time[route])*(-.25-prior)/(vgate-prior)
        }
    }
    last[route]=ns
    old_time[route]=ns
    old_gate[route]=vgate
}
END {
    if(fail)exit 1
    if(route!=2 || rows[1]<10000 || rows[2]<10000) {
        error("real-strength full stock/native waveform traces are incomplete")
    }
    for(side=1;side<=2;side++)
        for(i=1;i<=n;i++)if(!observed[side,i])
            error(sprintf("missing original strength plateau %d route %d",i,side))
    for(j=1;j<=2;j++) {
        if(!crossed[1,j]||!crossed[2,j]) {
            error(sprintf("missing original active enable transition %d",j))
            continue
        }
        edge=(j==1?65:125)
        reference=(j==1?65.691426:125.671774)
        if(abs(crossings[1,j]-reference)>.06)
            error(sprintf("original enable absolute analog delay %.5f ns expected %.5f ns",crossings[1,j],reference))
        difference=(crossings[2,j]-crossings[1,j])*1000
        if(abs(difference-200)>33)
            error(sprintf("original+ALB receiver expected 200 ps, observed %.5f ps at %d ns",difference,edge))
        if(j==1 || difference<min_delay)min_delay=difference
        if(j==1 || difference>max_delay)max_delay=difference
        sum_delay+=difference
        count_edges++
    }
    if(!fail)
        printf "Original d_source 0s/1s/Us/Uz → active d_to_real + two real_gain fanouts: %d plateau phases × 5 physical outputs × 2 simulators = %d verified observations, %d enable transitions, 2pF delay %.3f–%.3f ps (mean %.3f), max settled error %.6g V, max KCL error %.3g A, rows %d/%d PASS\n",n,n*5*2,count_edges,min_delay,max_delay,sum_delay/count_edges,max_voltage_error,max_kcl,rows[1],rows[2]
    exit fail
}
