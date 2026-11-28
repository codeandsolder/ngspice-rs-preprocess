# Original 1991 XSPICE real UDN event oracle: independent source-coded
# expectation of three exactly representable binary-fraction real drivers.
# Values resolve by SUM, not digital strength priority. This checks all
# direct ngspice and source-compiled native-X event timestamps and values.
function abs(x) {return x<0?-x:x}
function error(s) {print s > "/dev/stderr"; bad=1}
function equal(a,b,epsilon){return abs(a-b)<=epsilon}
function drive(i,d){return codes[((d==0?i:d==1?5*i+4:7*i+3)%12)+1]}
function state(i,d){return substr(drive(i,d),1,1)}
function a(i){return state(i,0)=="0"?-.5:state(i,0)=="1"?1.5:.5}
function braw(i){return state(i,1)=="0"?1.25:state(i,1)=="1"?-.75:.25}
function b(i){return -1.5*(braw(i)+.125)+.25}
function c(i){return state(i,2)=="0"?.125:state(i,2)=="1"?1.875:1}
function sum(i,p){return a(i)+b(i)+(three?c(p):0)}
function post(v){return -.5*(v+.375)+.125}
function observe(i,label,value,expected) {
 if(abs(value-expected)>.000005) {
   error(sprintf("REAL event source mismatch %s phase %d at %.6f ns: %s got %.9g expected %.9g",
         FILENAME,i,ns,label,value,expected))
   exit 1
 }
 seen[route,i,label]=1
 checks++
}
BEGIN {
  n_codes=split("0s 1s Us 0r 1r Ur 0z 1z Uz 0u 1u Uu",codes," ")
  three=ARGV[1]=="triple" || ARGV[1]=="triple-reordered"
  if(n_codes!=12 || (ARGV[1]!="pair" && !three)) {
     error("unknown REAL summation physical fixture variant")
     exit 1
  }
  # Remove the case selector from the list of files processed by awk.
  variant=ARGV[1]
  ARGV[1]=""
  route=0
}
FNR==1 {route++}
$1 ~ /^[0-9]/ {
  if(NF!=(three?8:7) || route>2) {
      error(sprintf("malformed original REAL event eprint %s:%d columns=%d",FILENAME,FNR,NF))
      exit 1
  }
  ns=$1*1e9
  if(rows[route] && ns+1e-6<last[route]) {
    error(sprintf("nonmonotonic event time %.6f in %s",ns,FILENAME))
    exit 1
  }
  rows[route]++
  if(rows[route]==1) {
    initial=-.5+.375+(three?.125:0)
    if(!equal(ns,0,.000001)||!equal($2,initial,.000001)||
       !equal($3,-.75,.000001)||!equal($4,-1.125,.000001)||
       !equal($5,.25,.000001)) {
        error(sprintf("original real UDN DC initialization incorrect in %s",FILENAME))
        exit 1
    }
    initial_verified[route]=1
  }
  for(i=0;i<24;i++) {
    edge=i*12
    if(!seen[route,i,"digital"] && equal(ns,edge,.0005)) {
       if($6!=drive(i,0)||$7!=drive(i,1)||(three&&$8!=drive(i,2))) {
         error(sprintf("original real UDN source-state mismatch %s at phase %d",FILENAME,i))
         exit 1
       }
       seen[route,i,"digital"]=1
       digital_checks++
    }
    if(i==0)continue
    old_c=three?c(i-1):0
    sum_a=a(i)+b(i-1)+old_c
    sum_b=a(i)+b(i)+old_c
    sum_c=a(i)+b(i)+(three?c(i):0)
    if(!seen[route,i,"A"]&&equal(ns,edge+.2,.0005))
      observe(i,"A",$2,sum_a)
    if(!seen[route,i,"BPRE"]&&equal(ns,edge+.3,.0005))
      observe(i,"BPRE",$3,braw(i))
    if(!seen[route,i,"POST_A"]&&equal(ns,edge+.38,.0005))
      observe(i,"POST_A",$4,post(sum_a))
    if(!seen[route,i,"B"]&&equal(ns,edge+.54,.0005))
      observe(i,"B",$2,sum_b)
    if(three && !seen[route,i,"C"]&&equal(ns,edge+.7,.0005))
      observe(i,"C",$2,sum_c)
    if(!seen[route,i,"POST_B"]&&equal(ns,edge+.72,.0005))
      observe(i,"POST_B",$4,post(sum_b))
    if(three && !seen[route,i,"POST_C"]&&equal(ns,edge+.88,.0005))
      observe(i,"POST_C",$4,post(sum_c))
  }
  for(j=1;j<=2;j++){
    edge=(j==1?93:189)
    target=(j==1?0:.25)
    if(!enabled[route,j] && ns>=edge && ns<edge+1.2 &&
       equal($5,target,.000001) && rows[route]>1 &&
       !equal(prev_enabled[route],$5,.000001)){
      enabled[route,j]=1
      enable_ns[route,j]=ns
    }
  }
  last[route]=ns
  prev_enabled[route]=$5
}
END {
  if(bad)exit 1
  if(route!=2 || rows[1]<140 || rows[2]<140 || !initial_verified[1]||!initial_verified[2])
    error(sprintf("incomplete original real UDN event traces %d/%d routes %d",rows[1],rows[2],route))
  for(r=1;r<=2;r++)for(i=0;i<24;i++){
    if(!seen[r,i,"digital"]) error(sprintf("missing original C source state phase %d route %d",i,r))
    if(i>0){
      split(three?"A BPRE POST_A B C POST_B POST_C":"A BPRE POST_A B POST_B",keys," ")
      for(k in keys)if(!seen[r,i,keys[k]])
        error(sprintf("missing original real UDN update %s phase %d route %d",keys[k],i,r))
    }
  }
  for(j=1;j<=2;j++) {
    if(!enabled[1,j]||!enabled[2,j]){
      error(sprintf("missing real UDN independently driven enabled transition %d",j))
      continue
    }
    expected=j==1?93.4135:189.4135
    if(!equal(enable_ns[1,j],expected,.04))
      error(sprintf("original real UDN enabled event absolute timing %.6f ns wrong vs %.6f",enable_ns[1,j],expected))
    extra=(enable_ns[2,j]-enable_ns[1,j])*1000
    if(!equal(extra,200,35))
      error(sprintf("original real UDN compiled 2pF receiver delay %.3f ps vs ~200ps",extra))
    delay[j]=extra
  }
  if(!bad)
    printf("Original REAL UDN %s SUM: %d/%d true original digital source events and %d precisely timed real addition / intermediate-gain updates; 2pF enabled event delays +%.3f / +%.3f ps; event rows %d/%d PASS\n",
       variant,digital_checks,digital_checks,checks,delay[1],delay[2],rows[1],rows[2])
  exit bad
}
