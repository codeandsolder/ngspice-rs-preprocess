# Independently authored original XSPICE 12 strength-code event contract.
# The source event file's 12 code literals are fixed in this checker and
# cross-verified with the true ngspice47 eprint of original d_source.
# Also verifies the original C d_to_real(350p) -> real_gain(250p) ->
# real_gain(430p) exact timed event pipeline, including both gain branches.
function abs(v){return v<0?-v:v}
function error(message){print message >"/dev/stderr"; bad=1}
function nearly(actual,expected,tolerance){return abs(actual-expected)<=tolerance}
function qcode(i){return codes[i%12+1]}
function rawstate(i){return i%3==0?-.5:i%3==1?2:.75}
function posgain(v){return 1.6*(v+.2)-.3}
function neggain(v){return -1.2*v+.4}
function chain(v){return -.75*(posgain(v)-.1)+.25}
BEGIN {
    num_codes=split("0s 1s Us 0r 1r Ur 0z 1z Uz 0u 1u Uu",codes," ")
    if(num_codes!=12){error("incomplete original drive-strength alphabet");exit 1}
    route=1
}
FNR==1 && NR>1 {route=2}
$1 ~ /^[0-9]/ {
    if(NF!=7){
        error(sprintf("malformed event trace row %s:%d, 7 fields expected",FILENAME,FNR))
        exit 1
    }
    ns=$1*1e9
    if(rows[route] && ns+.000001<last_time[route]) {
        error(sprintf("non-monotonic original source event trace %s:%.9f ns",FILENAME,ns))
        exit 1
    }
    if(!rows[route]) {
        if(!nearly(ns,0,.000001) || $2!="0s" ||
           !nearly($3,-.5,.000001)||!nearly($4,-.5,.000001)||
           !nearly($5,-.8,.000001)||!nearly($6,1,.000001)||
           !nearly($7,-2.3,.000001)) {
            error(sprintf("wrong original XSPICE event initial state at %s",FILENAME))
            exit 1
        }
        initial[route]=1
    }
    rows[route]++
    for(i=0;i<36;i++){
        edge=12*i
        if(!seen[route,i,"source"]&&nearly(ns,edge,.0005)){
            if($2!=qcode(i)){
                error(sprintf("original strength event mismatch %s t=%g ns: expected %s actual %s",FILENAME,edge,qcode(i),$2))
                exit 1
            }
            seen[route,i,"source"]=1
            strength_events++
        }
        if(i==0)continue
        v=rawstate(i)
        if(!seen[route,i,"raw"]&&nearly(ns,edge+.35,.0005)){
            if($2!=qcode(i)||!nearly($3,v,.000005)){
                error(sprintf("d_to_real exact 350ps event/value mismatch %s at %.5f ns: got %g expected %g",FILENAME,ns,$3,v))
                exit 1
            }
            seen[route,i,"raw"]=1
            checked_pipeline++
        }
        if(!seen[route,i,"pos"]&&nearly(ns,edge+.6,.0005)){
            if(!nearly($5,posgain(v),.000005)){
                error(sprintf("original real_gain 250ps first stage wrong at %.5f ns",ns))
                exit 1
            }
            seen[route,i,"pos"]=1
            checked_pipeline++
        }
        if(!seen[route,i,"neg"]&&nearly(ns,edge+.65,.0005)){
            if(!nearly($6,neggain(v),.000005)){
                error(sprintf("original real_gain 300ps fanout wrong at %.5f ns",ns))
                exit 1
            }
            seen[route,i,"neg"]=1
            checked_pipeline++
        }
        if(!seen[route,i,"chain"]&&nearly(ns,edge+1.03,.0005)){
            if(!nearly($7,chain(v),.000005)){
                error(sprintf("original serial real_gain exact 250+430ps cascade wrong at %.5f ns: got %g expected %g",ns,$7,chain(v)))
                exit 1
            }
            seen[route,i,"chain"]=1
            checked_pipeline++
        }
    }
    # At the two coincident source and physical ALB enable changes, both
    # original event graphs receive data and enable transitions. Their
    # scheduling differs by an added 2pF ALB receiver (~200ps), but every
    # original C real event must converge and timing remain causal.
    if(rows[route]>1) {
        for(j=1;j<=2;j++){
            edge=j==1?144:288
            target=j==1?0:-.5
            if(!gated[route,j]&&ns>=edge&&ns<edge+1.3 &&
               !nearly(previous_gate[route],target,.000001)&&
               nearly($4,target,.000001)){
                gated[route,j]=ns
                changed_enable[route,j]=1
            }
        }
    }
    last_time[route]=ns
    previous_gate[route]=$4
}
END {
    if(bad)exit 1
    if(route!=2||rows[1]<165||rows[2]<165||!initial[1]||!initial[2]) {
        error(sprintf("incomplete original event sources: routes %d, rows %d/%d",route,rows[1],rows[2]))
    }
    for(r=1;r<=2;r++) {
        for(i=0;i<36;i++){
            if(!seen[r,i,"source"])error(sprintf("source strength event missing route %d phase %d %s",r,i,qcode(i)))
            if(i==0)continue
            split("raw pos neg chain",keys," ")
            for(k=1;k<=4;k++)if(!seen[r,i,keys[k]])
                error(sprintf("original C pipeline %s missing route %d phase %d",keys[k],r,i))
        }
    }
    for(j=1;j<=2;j++){
        if(!changed_enable[1,j]||!changed_enable[2,j]){
            error(sprintf("coincident enable event missing for edge %d",j))
            continue
        }
        expected=j==1?144.4196519:288.3996519
        if(!nearly(gated[1,j],expected,.045)){
            error(sprintf("original source coincident enable absolute event time %.6f ns expected %.6f ns",gated[1,j],expected))
        }
        extra=(gated[2,j]-gated[1,j])*1000
        if(!nearly(extra,200,35)){
            error(sprintf("source-compiled extra 2pF enable event delay %.3f ps, expected ~200ps",extra))
        }
        if(j==1||extra<min_delay)min_delay=extra
        if(j==1||extra>max_delay)max_delay=extra
        sum_delay+=extra
    }
    if(!bad)printf("Original XSPICE exact event eprint: %d source strength states (12 combinations × 3 cycles × 2 simulators), %d verified real-value/delay pipeline events (d_to_real 350ps, gain 250/300ps, cascade +430ps), two coincident enable events delta %.3f–%.3f ps mean %.3f, event rows %d/%d PASS\n",strength_events,checked_pipeline,min_delay,max_delay,sum_delay/2,rows[1],rows[2])
    exit bad
}
