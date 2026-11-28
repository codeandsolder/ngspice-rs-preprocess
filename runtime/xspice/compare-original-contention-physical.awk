# Independently checks settled physical analog signal and nonlinear electrical
# current from original ngspice47 C models whose output is a MULTI-DRIVEN
# 12-state digital net. Source pair/triple matrix is pinned separately.
function abs(v){return v<0?-v:v}
function fail(m){print m>"/dev/stderr";bad=1}
function input_value(code){return substr(code,1,1)=="0"?-.5:substr(code,1,1)=="1"?2:.75}
function probe_value(code){return biased_hc_probe(code)}
BEGIN{
 # Variant is a circuit property, not an arbitrary scratch directory
 # substring (which might itself contain "pair" or "reordered").
 if(variant!="pair" && variant!="triple" && variant!="triple-reordered"){
  fail("invalid or missing original contention variant");exit 1
 }
 kind=variant=="pair"?"pair":"triple"
 reordered=variant=="triple-reordered"
 expected_cols=kind=="pair"?3:5
 route=0
}
FILENAME==ARGV[1]{
 if($1~/^\*/||NF==0)next
 if(NF!=expected_cols){fail("invalid original resolver expected-state fixture");exit 1}
 ++n
 expected[n]=kind=="pair"?$3:reordered?$5:$4
 next
}
{
 if(FNR==1)route++
 if(NF!=12 || route<1||route>2){
  fail(sprintf("original contention analog waveform invalid fields=%d at %s:%d",NF,FILENAME,FNR));exit 1
 }
 ns=$1*1e9
 if(FNR>1&&ns+.00001<previous[route]){
  fail(sprintf("nonmonotonic analog time %s:%d",FILENAME,FNR));exit 1
 }
 rows[route]++
 current=$10
 probe=$8
 residual=abs(current-(probe-1.2)*.001)
 if(residual>max_kcl)max_kcl=residual
 if(residual>1e-7){
  fail(sprintf("biased original multi-driver analog strength KCL wrong %s at %.5f ns",FILENAME,ns));exit 1
 }
 if(FNR==1){
  initial_state=input_value(expected[1])
  if(abs($2-initial_state)>.000001||abs($4-initial_state)>.000001||abs($6+.8)>.000001){
   fail(sprintf("wrong original multi-driver real event DC initial state %s",FILENAME));exit 1
  }
 }
 # Floating drive is a 10 nS midpoint conductance in parallel with
 # the 1 kOhm pull and 2 pF load. Sample after the 50 ps slew,
 # then verify the analytic RC trajectory rather than assume DC.
 phase=int(ns/8)+1
 if(phase<=n && floating_drive(expected[phase]) && !floating_sample[route,phase] && ns-(phase-1)*8>=.2){
  floating_sample[route,phase]=1
  floating_time[route,phase]=ns
  floating_voltage[route,phase]=probe
 }
 for(i=1;i<=n;i++){
  if(checked[route,i]||abs(ns-((i-1)*8+7.1))>.035)continue
  code=expected[i]
  raw=input_value(code)
  active=$12>1.5
  target[0]=raw
  target[1]=active?raw:0
  target[2]=1.6*(raw+.2)-.3
  target[3]=probe_value(code)
  if(floating_drive(code)){
   if(!floating_sample[route,i]){fail("missing floating RC initial sample");exit 1}
   equilibrium=(.001*1.2+1e-8*1.65)/(.001+1e-8)
   target[3]=equilibrium+(floating_voltage[route,i]-equilibrium)*exp(-(ns-floating_time[route,i])*(.001+1e-8)/.002)
  }
  actual[0]=$2
  actual[1]=$4
  actual[2]=$6
  actual[3]=$8
  for(k=0;k<4;k++){
   error_v=abs(target[k]-actual[k])
   tolerance=k==1?.022:.004
   if(error_v>tolerance){
    fail(sprintf("original %s resolver physical %s phase %d channel %d: %s expected %.7g got %.7g (err %.7g)",kind,FILENAME,i,k,code,target[k],actual[k],error_v))
    exit 1
   }
   if(error_v>max_error)max_error=error_v
   observations++
  }
  checked[route,i]=1
 }
 previous[route]=ns
}
END{
 if(bad)exit 1
 if(route!=2||n!=(kind=="pair"?144:48)||rows[1]<15000||rows[2]<15000)
  fail(sprintf("incomplete original %s contention analog runs %d/%d",kind,rows[1],rows[2]))
 for(r=1;r<=2;r++)for(i=1;i<=n;i++)
  if(!checked[r,i])fail(sprintf("missing original resolved analog case %s %d route %d",kind,i,r))
 if(!bad)
  printf "Original %s%s multi-driver analog: %d phases × 4 physical channels × 2 simulators = %d verified outputs; max original loaded-node voltage deviation %.5g V, max KCL residual %.3g A, rows %d/%d PASS\n",kind,reordered?" reversed driver order":"",n,observations,max_error,max_kcl,rows[1],rows[2]
 exit bad
}
