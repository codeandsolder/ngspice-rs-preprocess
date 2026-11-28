# Independent physical analogue oracle for multiple unmodified 1991 XSPICE
# real UDN drivers. All C code-model REAL outputs drive one shared event
# node, where the original resolver sums their values. The analogue
# probes measure full actual ngspice waves and bias-source KCL.
function abs(x){return x<0?-x:x}
function error(message){print message >"/dev/stderr";bad=1}
function code(i,d){return codes[((d==0?i:d==1?5*i+4:7*i+3)%12)+1]}
function state(i,d){return substr(code(i,d),1,1)}
function a(i){return state(i,0)=="0"?-.5:state(i,0)=="1"?1.5:.5}
function braw(i){return state(i,1)=="0"?1.25:state(i,1)=="1"?-.75:.25}
function b(i){return -1.5*(braw(i)+.125)+.25}
function c(i){return state(i,2)=="0"?.125:state(i,2)=="1"?1.875:1}
function post(v){return -.5*(v+.375)+.125}
function enabled(i){
 return (i<7||i>=15)?(state(i,0)=="0"?.25:state(i,0)=="1"?2.25:1.25):0
}
function probe(i){return biased_hc_probe(code(i,0))}
BEGIN{
 n=split("0s 1s Us 0r 1r Ur 0z 1z Uz 0u 1u Uu",codes," ")
 three=ARGV[1]=="triple" || ARGV[1]=="triple-reordered"
 if(n!=12 || (ARGV[1]!="pair" && !three)){error("invalid original real SUM variant");exit 1}
 variant=ARGV[1]
 ARGV[1]=""
 route=0
}
FNR==1{route++}
{
 if(NF!=12||route>2){
    error(sprintf("malformed original REAL sum analogue output at %s:%d (NF=%d)",FILENAME,FNR,NF))
    exit 1
 }
 ns=$1*1e9
 if(FNR>1 && ns+1e-6<last[route]){
    error(sprintf("original REAL sum analogue output time reversed %s:%d",FILENAME,FNR))
    exit 1
 }
 rows[route]++
 if(FNR==1){
    initial=-.5+.375+(three?.125:0)
    if(abs($2-initial)>.000001||abs($4+1.125)>.000001||abs($6-.25)>.000001){
      error(sprintf("original multi REAL C-model DC init mismatch in %s",FILENAME));exit 1
    }
 }
 kcl=abs($10-($8-1.2)*.001)
 if(kcl>max_kcl)max_kcl=kcl
 if(kcl>1e-7){
    error(sprintf("original REAL summation probe KCL violation %s t=%.7g ns error %.7g A",FILENAME,ns,kcl))
    exit 1
 }
 for(i=0;i<24;i++){
   sample=i*12+((i==7||i==15)?11.5:10.5)
   if(seen[route,i]||abs(ns-sample)>.035)continue
   real=a(i)+b(i)+(three?c(i):0)
   target[0]=real
   target[1]=post(real)
   target[2]=enabled(i)
   target[3]=probe(i)
   measured[0]=$2
   measured[1]=$4
   measured[2]=$6
   measured[3]=$8
   for(k=0;k<4;k++){
      d=abs(measured[k]-target[k])
      tolerance=(k==3&&floating_drive(code(i,0)))?.018:(k==2&&(i==7||i==15))?.018:.005
      if(d>tolerance){
        error(sprintf("original REAL UDN sum %s phase %d route %d channel %d expected %.8g observed %.8g err %.6g",variant,i,route,k,target[k],measured[k],d))
        exit 1
      }
      if(d>max_voltage_error)max_voltage_error=d
      observations++
   }
   if(abs($12-((i<7||i>=15)?3.3:0))>.001){
       error(sprintf("original REAL UDN SUM enable source incorrect %s phase %d",FILENAME,i));exit 1
   }
   seen[route,i]=1
 }
 last[route]=ns
}
END{
 if(bad)exit 1
 if(route!=2||rows[1]<11000||rows[2]<11000){
   error(sprintf("incomplete original real UDN analogue waveforms for %s: %d/%d",variant,rows[1],rows[2]))
 }
 for(route_id=1;route_id<=2;route_id++)
    for(i=0;i<24;i++)
      if(!seen[route_id,i])error(sprintf("missing original REAL UDN physical plateau phase %d route %d",i,route_id))
 if(!bad)
    printf("Original REAL UDN %s physically summed independent C outputs: 24 phases × 4 loaded probes × 2 simulators = %d outputs, max voltage error %.6g V, max source KCL residual %.3g A, original/native rows %d/%d PASS\n",variant,observations,max_voltage_error,max_kcl,rows[1],rows[2])
 exit bad
}
