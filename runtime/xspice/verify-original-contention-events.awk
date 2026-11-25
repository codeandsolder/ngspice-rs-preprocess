# Verifies the ORIGINAL ngspice47 digital 12-state contention resolution,
# not a guessed boolean truth table. Input 1 is an independently pinned
# matrix source, input 2 is the direct original C-model event trace, input 3
# is the source-compiled native event trace (ngspice eprint).
# For three drivers, original idndig.c left-fold evaluation is compared
# against the alternate grouping, which differs on all 48 chosen vectors.
function abs(v){return v<0?-v:v}
function fail(message){print message >"/dev/stderr"; bad=1}
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
 if($1 ~ /^\*/ || NF==0)next
 if(NF!=expected_cols){fail("invalid independently authored original digital contention table row");exit 1}
 ++n
 drive0[n]=$1;drive1[n]=$2
 if(kind=="triple") {
  drive2[n]=$3
  left[n]=$4;right[n]=$5
  if(left[n]==right[n]){fail(sprintf("triple %d unexpectedly associative",n));exit 1}
  expected[n]=reordered?right[n]:left[n]
 } else expected[n]=$3
 next
}
{
 if(FNR==1)route++
 # Avoid header text and insist on true 4-signal original C event reports
 if($1 !~ /^[0-9]/)next
 if(NF!=5 || route<1 || route>2){
  fail(sprintf("malformed original XSPICE eprint %s row %d fields %d",FILENAME,FNR,NF))
  exit 1
 }
 ns=$1*1e9
 if(rows[route] && ns+0.000001<prev[route]){
  fail(sprintf("nonmonotonic original contention events %s",FILENAME));exit 1
 }
 rows[route]++
 for(i=1;i<=n;i++){
  t=(i-1)*8
  if(!seen[route,i] && abs(ns-t)<.0005){
   if($2!=expected[i]){
    fail(sprintf("ORIGINAL digital resolver %s phase %d: %s+%s%s expected %s, got %s",FILENAME,i,drive0[i],drive1[i],kind=="triple"?"+"drive2[i]:"",expected[i],$2))
    exit 1
   }
   seen[route,i]=1
   if(kind=="triple" && reordered && $2==left[i]){
     fail(sprintf("original 3-driver reordered event unexpectedly matched original declaration ordering in case %d",i))
     exit 1
   }
  }
 }
 # Original d_to_real C consumes resolved digital state, not strength.
 for(i=2;i<=n;i++){
  t=(i-1)*8+.35
  target=substr(expected[i],1,1)=="0"?-.5:substr(expected[i],1,1)=="1"?2:.75
  if(!real_seen[route,i] && abs(ns-t)<.0005){
   if(abs($3-target)>.000001){
     fail(sprintf("original resolved strength->real delay wrong %s at t=%g ns: got %g expected %g",FILENAME,ns,$3,target))
     exit 1
   }
   real_seen[route,i]=1
  }
  # The original d_to_real and real_gain C models must schedule new
  # real-valued event data at exactly +350ps and +600ps when the
  # *resolved state* actually changes. Strength-only transitions need
  # not produce a distinct real event, because d_to_real ignores strength.
  previous_state=substr(expected[i-1],1,1)
  current_state=substr(expected[i],1,1)
  if(previous_state!=current_state &&
     !gain_seen[route,i] && abs(ns-(i-1)*8-.6)<.0005){
   input=substr(expected[i],1,1)=="0"?-.5:substr(expected[i],1,1)=="1"?2:.75
   gain=1.6*(input+.2)-.3
   if(abs($5-gain)>.000001){
    fail(sprintf("original contested real_gain event value/timing wrong %s t=%.6f ns",FILENAME,ns))
    exit 1
   }
   gain_seen[route,i]=1
  }
 }
 # Dedicated physical ALB enable changes are isolated from original
 # d_source state transitions. A nonzero resolved state must follow the
 # actual analog input threshold plus genuine receiver capacitance delay.
 for(j=1;j<=2;j++){
  edge=kind=="pair"?(j==1?386:773):(j==1?130:261)
  want=j==1?0:$3
  if(!enable_checked[route,j] && rows[route]>1 &&
     ns>=edge && ns<edge+1.3 &&
     abs($4-want)<.00001 && abs(prior_gated[route]-$4)>.00001) {
   enable_checked[route,j]=1
   enable_ns[route,j]=ns
  }
 }
 prior_gated[route]=$4
 prev[route]=ns
}
END{
 if(bad)exit 1
 if(n!=(kind=="pair"?144:48)||route!=2){
  fail(sprintf("incomplete original resolver matrix: %s, entries=%d routes=%d",kind,n,route))
 }
 for(r=1;r<=2;r++){
  for(i=1;i<=n;i++){
   if(!seen[r,i])fail(sprintf("missing physical original digital event %s route %d phase %d",kind,r,i))
   if(i>1 && substr(expected[i-1],1,1)!=substr(expected[i],1,1)){
    if(!real_seen[r,i])fail(sprintf("missing original contested d_to_real +350ps event %s route %d phase %d",kind,r,i))
    if(!gain_seen[r,i])fail(sprintf("missing original contested real_gain +600ps event %s route %d phase %d",kind,r,i))
    timed_events+=2
   }
  }
 }
 for(j=1;j<=2;j++){
  if(!enable_checked[1,j]||!enable_checked[2,j]){
   fail(sprintf("missing original shared-net physical enable event %s edge %d",kind,j))
   continue
  }
  edge=kind=="pair"?(j==1?386:773):(j==1?130:261)
  if(abs(enable_ns[1,j]-edge-.4135)>.035)
   fail(sprintf("original stock multi-driver enable absolute timing incorrect: %g ns at %d",enable_ns[1,j],edge))
  difference=(enable_ns[2,j]-enable_ns[1,j])*1000
  if(abs(difference-200)>35)
   fail(sprintf("compiled original multi-driver 2pF receiver: %.3f ps at %d, expected 200ps",difference,edge))
  if(j==1||difference<min_delay)min_delay=difference
  if(j==1||difference>max_delay)max_delay=difference
  sum_delay+=difference
 }
 if(!bad)
   printf "Original XSPICE %s%s contention matrix: %d exact 12-state digital resolved inputs per route, %d/%d direct/native observations, %d original C event trace rows, %d absolute d_to_real/gain timing checks; actual physical 2pF receiver +%.3f/+%.3f ps (mean %.3f) PASS\n",
   kind,reordered?" (driver order reversed)":"",n,n*2,n*2,rows[1]+rows[2],timed_events,
   (enable_ns[2,1]-enable_ns[1,1])*1000,(enable_ns[2,2]-enable_ns[1,2])*1000,sum_delay/2
 exit bad
}
