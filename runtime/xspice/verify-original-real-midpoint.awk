# Unmodified original ngspice47 d_to_real UNKNOWN midpoint overflow:
# zero=one=1e308 are finite, but original C computes
#   (zero + one) / 2.0
# producing +Inf when the ORIGINAL digital input state is UNKNOWN.
# This checks the actual original C-model REAL output events, not an
# arithmetic approximation or a generated analog voltage.
function ab(v) {return v<0?-v:v}
function fail(s){print s >"/dev/stderr";bad=1}
function near(a,b,eps){return ab(a-b)<=eps}
function drive(i,d){return codes[((d==0?i:d==1?5*i+4:7*i+3)%12)+1]}
function is_unknown(i){return substr(drive(i,0),1,1)=="U"}
function typ(s){
    if(tolower(s)=="inf"||tolower(s)=="+inf")return "inf"
    if(tolower(s) ~ /nan/)return "nan"
    if(s+0>=9.9e307&&s+0<=1.01e308)return "finite"
    if(s+0==0)return "zero"
    return "other"
}
BEGIN{
 n=split("0s 1s Us 0r 1r Ur 0z 1z Uz 0u 1u Uu",codes," ")
 if(n!=12){fail("original source digital codes invalid");exit 1}
 route=0
}
FNR==1{route++}
$1~/^[0-9]/{
 if(NF!=11||route>2){fail(sprintf("original midpoint eprint invalid at %s:%d NF=%d",FILENAME,FNR,NF));exit 1}
 ns=$1*1e9
 rows[route]++
 phase=int((ns+1e-8)/12)
 if(phase>23)phase=23
 if(phase<0)phase=0
 active=phase
 if(phase>0&&ns<phase*12+.18-.0005)active=phase-1
 expect=is_unknown(active)?"inf":"finite"
 got=typ($2)
 if(got!=expect){
    fail(sprintf("original C d_to_real UNKNOWN midpoint overflows at %s %.8fns: input %s expected %s got %s",FILENAME,ns,drive(active,0),expect,got));exit 1
 }
 # Original enable comes from the independently proven PWL source
 # and true 2pF ALB receiver. The separate original R_ENABLED
 # physical oracle verifies 93.4135/189.4135ns absolute output
 # timestamps (+350ps C delay), and +201/+189.5ps receiver load.
 # This original midpoint's C delay is +180ps instead.
 off=93.2435+(route==2?.201:0)
 on=189.2435+(route==2?.1895:0)
 enabled=ns<off-.0005||ns>=on-.0005
 want_gated=enabled?(is_unknown(active)?"inf":"finite"):(is_unknown(active)?"nan":"zero")
 got_gated=typ($3)
 if(got_gated!=want_gated){
    fail(sprintf("original d_to_real disabled UNKNOWN must generate NaN, %s at %.8fns: expected %s got %s",FILENAME,ns,want_gated,got_gated));exit 1
 }
 for(j=1;j<=2;j++){
    edge=j==1?off:on
    if(!gated_event[route,j] && near(ns,edge,.0005)){
        gated_event[route,j]=1
        if(j==1 && got_gated!="zero" || j==2 && got_gated!="finite"){
            fail(sprintf("original disabled/reenabled midpoint event incorrect %s at %.8fns",FILENAME,ns));exit 1
        }
    }
 }
 events++
 if(rows[route]==1 && (!near(ns,0,.000001)||got!="finite")){
    fail(sprintf("original C midpoint DC result incorrect in %s",FILENAME));exit 1
 }
 for(i=0;i<24;i++){
   edge=i*12
   if(!seen[route,i,"source"]&&near(ns,edge,.0005)){
     if($9!=drive(i,0)||$10!=drive(i,1)||$11!=drive(i,2)){
        fail(sprintf("original C input state/strength mismatch %s phase %d",FILENAME,i));exit 1
     }
     seen[route,i,"source"]=1
     digital++
   }
   if(i==0)continue
   change=is_unknown(i)!=is_unknown(i-1)
   if(change&&!seen[route,i,"midpoint"]&&near(ns,edge+.18,.0005)){
     if(got!=(is_unknown(i)?"inf":"finite")){
       fail(sprintf("original C d_to_real wrong delay/midpoint output at %.8f ns",ns));exit 1
     }
     seen[route,i,"midpoint"]=1
     verified_changes++
     if(!gated_event_from_digital[route,i]){
         gated_event_from_digital[route,i]=1
         verified_gated_changes++
     }
   }
 }
}
END{
 if(bad)exit 1
 if(route!=2||rows[1]<80||rows[2]<80)fail("missing original midpoint simulator event traces")
 for(r=1;r<=2;r++)for(j=1;j<=2;j++){
     if(!gated_event[r,j])
       fail(sprintf("missing original true midpoint enable edge %d route %d",j,r))
 }
 for(r=1;r<=2;r++)for(i=0;i<24;i++){
   if(!seen[r,i,"source"])fail(sprintf("missing original d_source phase %d route %d",i,r))
   if(i>0&&is_unknown(i)!=is_unknown(i-1)&&!seen[r,i,"midpoint"])
      fail(sprintf("missing exact +180ps original d_to_real finite/Inf midpoint transition at phase %d route %d",i,r))
 }
 if(!bad)
 printf("Original ngspice47 d_to_real UNKNOWN midpoint overflow and disabled NaN: finite ZERO/ONE 1e308, enabled UNKNOWN +Inf, disabled UNKNOWN NaN; %d original C digital source states, %d exact +180ps finite/Inf input transitions, %d gated input transitions, 4 physically shifted original-enable events, %d genuine event observations (stock/native) PASS\n",digital,verified_changes,verified_gated_changes,events)
 exit bad
}
