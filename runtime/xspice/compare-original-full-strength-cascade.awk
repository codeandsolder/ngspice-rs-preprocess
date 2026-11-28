# Independent physical oracle for original d_source 3 digital states × 4
# strengths, a non-null coincident enable, and two-stage real_gain cascade.
# Uses six separately loaded physical outputs and true 1.2V pull-source KCL.
function abs(x){return x<0?-x:x}
function error(message){print message>"/dev/stderr";bad=1}
function raw_state(i){return i%3==0?-.5:i%3==1?2:.75}
function pos_gain(v){return 1.6*(v+.2)-.3}
function neg_gain(v){return -1.2*v+.4}
function chain_gain(v){return -.75*(pos_gain(v)-.1)+.25}
BEGIN {
    n=36
    ncode=split("0s 1s Us 0r 1r Ur 0z 1z Uz 0u 1u Uu",codes," ")
    if(ncode!=12){error("incomplete original strength code matrix");exit 1}
    for(i=0;i<12;i++) probe[i]=biased_hc_probe(codes[i+1])
    route=1
}
FNR==1 && NR>1 {route=2}
{
    if(NF!=16) {
        error(sprintf("original full-strength waveform malformed (%s:%d fields=%d)",FILENAME,FNR,NF))
        exit 1
    }
    ns=$1*1e9
    if(FNR>1&&ns+0.000001<previous_ns[route]){
        error(sprintf("non-monotonic physical time %s:%d",FILENAME,FNR))
        exit 1
    }
    rows[route]++
    vraw=$2
    vgate=$4
    vpos=$6
    vneg=$8
    vchain=$10
    vprobe=$12
    current=$14
    ena=$16
    if(FNR==1) {
        if(abs(vraw+.5)>.000001||abs(vgate+.5)>.000001||
           abs(vpos+.8)>.000001||abs(vneg-1)>.000001||
           abs(vchain+2.3)>.000001) {
            error(sprintf("original real_gain cascade DC IC mismatch %s",FILENAME))
            exit 1
        }
        init[route]=1
    }
    kcl=abs(current-(vprobe-1.2)*.001)
    if(kcl>max_kcl)max_kcl=kcl
    if(kcl>1e-7){
        error(sprintf("original physical digital strength probe KCL violation %s t=%g ns residual=%g A",FILENAME,ns,kcl))
        exit 1
    }
    for(i=0;i<n;i++){
        if(sampled[route,i]||abs(ns-(12*i+11))>.03)continue
        v=raw_state(i)
        active=(i<12||i>=24)
        expected[0]=v
        expected[1]=active?v:0
        expected[2]=pos_gain(v)
        expected[3]=neg_gain(v)
        expected[4]=chain_gain(v)
        expected[5]=probe[i%12]
        actual[0]=vraw
        actual[1]=vgate
        actual[2]=vpos
        actual[3]=vneg
        actual[4]=vchain
        actual[5]=vprobe
        for(k=0;k<6;k++){
            difference=abs(actual[k]-expected[k])
            tolerance=(floating_drive(codes[i%12+1])&&k==5)?.012:.004
            if(difference>tolerance){
                error(sprintf("original 12-strength/%s phase %d route %d channel %d wrong: want %.7g actual %.7g",codes[i%12+1],i,route,k,expected[k],actual[k]))
                exit 1
            }
            if(difference>max_plateau_error)max_plateau_error=difference
            physically_checked++
        }
        if(abs(ena-(active?3.3:0))>.001){
            error(sprintf("original ALB coincident enable stimulus incorrect phase %d: %g",i,ena))
            exit 1
        }
        sampled[route,i]=1
    }
    previous_ns[route]=ns
}
END {
    if(bad)exit 1
    if(route!=2||rows[1]<20000||rows[2]<20000||!init[1]||!init[2]){
        error(sprintf("original strength matrix physical traces incomplete %d/%d",rows[1],rows[2]))
    }
    for(r=1;r<=2;r++)
        for(i=0;i<n;i++)
            if(!sampled[r,i])error(sprintf("missing original physical matrix phase %d for route %d",i,r))
    if(!bad)
        printf("Original complete 12-code strength matrix and two-stage real_gain physical oracle: %d phases × 6 loaded outputs × 2 original-C simulator routes = %d observations; original full-matrix KCL max %.3g A, max plateau deviation %.6f V, %d/%d waveform rows PASS\n",n,physically_checked,max_kcl,max_plateau_error,rows[1],rows[2])
    exit bad
}
