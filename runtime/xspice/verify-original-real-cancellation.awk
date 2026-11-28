# Source-original ngspice47 REAL UDN nonassociative IEEE-754 cancellation.
# Three genuine unchanged original C-model output drivers have binary64
# contributions +1e16, +1, -1e16 into ONE shared real event node:
#  [A,B,C] folds to 0 while [A,C,B] folds to 1.
# Verify both stock and source-compiled native original real events and
# physically loaded analog outputs, bias-current KCL and enable timing.
function abs(x){return x<0?-x:x}
function error(s){print s >"/dev/stderr";bad=1}
function eq(a,b,eps){return abs(a-b)<=eps}
function code(i,d){return codes[((d==0?i:d==1?5*i+4:7*i+3)%12)+1]}
function state(i){return substr(code(i,0),1,1)}
function enable_value(i){return (i<7||i>=15)?(state(i)=="0"?.25:state(i)=="1"?2.25:1.25):0}
function probe(i){return biased_hc_probe(code(i,0))}
BEGIN{
 n=split("0s 1s Us 0r 1r Ur 0z 1z Uz 0u 1u Uu",codes," ")
 if(n!=12 || (ORDER!="left"&&ORDER!="grouped")){error("unknown original REAL UDN cancellation variant");exit 1}
 value=ORDER=="left"?0:1
 post_value=-.5*(value+.375)+.125
 file=0
}
FNR==1{file++}
file<=2 && $1~/^[0-9]/{
 if(NF!=8){error(sprintf("malformed original C model cancellation eprint %s:%d",FILENAME,FNR));exit 1}
 route=file
 ns=$1*1e9
 rows[route]++
 if(abs($2-value)>.000001){
    error(sprintf("original REAL UDN binary64 cancellation order changed %s at %.6f ns: expected %g got %g",FILENAME,ns,value,$2))
    exit 1
 }
 if(rows[route]==1){
    if(!eq(ns,0,1e-6)||!eq($4,-1.125,.000001)||!eq($5,.25,.000001)){
      error(sprintf("original multi-real-driver DC initialization mismatch %s",FILENAME))
      exit 1
    }
 }
 if(eq(ns,.36,.0005)){
    if(abs($4-post_value)>.000001){
       error(sprintf("original post-gain event +360ps wrong in %s",FILENAME))
       exit 1
    }
    first_gain[route]=1
 }
 if(ns>.36+.0001 && abs($4-post_value)>.000001){
    error(sprintf("original real UDN cancellation downstream original real_gain wrong %s @ %.4f ns",FILENAME,ns))
    exit 1
 }
 for(i=0;i<24;i++){
    if(!seen[route,i]&&eq(ns,i*12,.0005)){
        if($6!=code(i,0)||$7!=code(i,1)||$8!=code(i,2)){
           error(sprintf("original REAL UDN cancellation changed source strength %s phase %d",FILENAME,i))
           exit 1
        }
        seen[route,i]=1
        inputs++
    }
 }
 for(j=1;j<=2;j++){
    edge=j==1?93:189
    target=j==1?0:.25
    if(!enabled[route,j]&&ns>=edge&&ns<edge+1.2&&
       eq($5,target,.000001)&&rows[route]>1&&!eq(prior_enabled[route],$5,.000001)){
         enabled[route,j]=1; enable_time[route,j]=ns
    }
 }
 prior_enabled[route]=$5
 next
}
file>=3{
 if(NF!=12||file>4){error(sprintf("invalid source-original cancellation analogue waveform %s:%d",FILENAME,FNR));exit 1}
 route=file-2
 ns=$1*1e9
 points[route]++
 if(points[route]==1){
    if(!eq($2,value,.000001)||!eq($4,-1.125,.000001)||!eq($6,.25,.000001)){
       error(sprintf("original cancellation physical DC initialization wrong in %s",FILENAME))
       exit 1
    }
 }
 kcl=abs($10-($8-1.2)*.001)
 if(kcl>max_kcl)max_kcl=kcl
 if(kcl>1e-7){error(sprintf("REAL cancellation source KCL wrong %s t=%.7g ns",FILENAME,ns));exit 1}
 for(i=0;i<24;i++){
    sample=i*12+((i==7||i==15)?11.5:10.5)
    if(sampled[route,i]||abs(ns-sample)>.035)continue
    expected[0]=value
    expected[1]=post_value
    expected[2]=enable_value(i)
    expected[3]=probe(i)
    actual[0]=$2;actual[1]=$4;actual[2]=$6;actual[3]=$8
    for(k=0;k<4;k++){
       difference=abs(actual[k]-expected[k])
       tolerance=k==3&&floating_drive(code(i,0))?.018:k==2&&(i==7||i==15)?.018:.005
       if(difference>tolerance){
           error(sprintf("original REAL cancellation physical mismatch %s route %d phase %d channel %d expected %.7g got %.7g",FILENAME,route,i,k,expected[k],actual[k]))
           exit 1
       }
       if(difference>max_voltage_error)max_voltage_error=difference
       observations++
    }
    sampled[route,i]=1
 }
}
END{
 if(bad)exit 1
 if(file!=4||rows[1]<48||rows[2]<48||points[1]<11000||points[2]<11000)
    error("incomplete original cancellation source/model/output physical traces")
 for(route=1;route<=2;route++){
   if(!first_gain[route])error(sprintf("missing original post gain initialization event route %d",route))
   for(i=0;i<24;i++){
      if(!seen[route,i])error(sprintf("missing original cancellation event source phase %d route %d",i,route))
      if(!sampled[route,i])error(sprintf("missing original cancellation voltage phase %d route %d",i,route))
   }
 }
 for(j=1;j<=2;j++){
    if(!enabled[1,j]||!enabled[2,j]){
       error(sprintf("missing original source-compiled 2pF enabled event %d",j));continue
    }
    reference=j==1?93.4135:189.4135
    if(!eq(enable_time[1,j],reference,.035)){
       error(sprintf("original binary64 event absolute enable timing wrong: %.6f",enable_time[1,j]))
    }
    difference=(enable_time[2,j]-enable_time[1,j])*1000
    if(!eq(difference,200,35)){
       error(sprintf("original binary64 +2pF source receiver delay wrong: %.3f ps",difference))
    }
    delay[j]=difference
 }
 if(!bad)
    printf("Original REAL UDN IEEE-754 sequential cancellation (%s): +1e16,+1,-1e16 resolves exactly %.0f, %d/%d source states, %d/192 original C physical voltages, max KCL %.3g A, receiver extra +%.3f/%.3f ps PASS\n",
       ORDER,value,inputs,inputs,observations,max_kcl,delay[1],delay[2])
 exit bad
}
