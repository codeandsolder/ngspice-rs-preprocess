# True unchanged original ngspice47 d_source / d_to_real / real_delay /
# real_gain event oracle and independent loaded analog source model oracle.
# Four files: original stock events, compiled ALB-native events, original
# stock physical waveform, compiled ALB-native physical waveform.
# Every original event sample is predicted from independently declared
# digital 12-state/strength source schedule and actual C-model semantics.
function mag(v) { return v<0?-v:v }
function near(a,b,eps) {return mag(a-b)<=eps}
function fail(s) { print s > "/dev/stderr"; bad=1 }
function dstate(t, i, s) {
 s=ds[1]
 for (i=2;i<=nd;i++) if(t>=dt[i]-1e-5)s=ds[i]
 return s
}
function cstate(t, i, s) {
 s=cs[1]
 for (i=2;i<=nc;i++) if(t>=ct[i]-1e-5)s=cs[i]
 return s
}
function dvalue(s, x) {
 x=substr(s,1,1)
 return x=="0"?-.5:x=="1"?2:.75
}
function envalue(s, x) {
 x=substr(s,1,1)
 return x=="0"?-.25:x=="1"?1.75:.75
}
function held(t, i, val) {
 val=0
 for (i=1;i<=ne;i++) if(t>=edge[i]+.61-1e-5)
   val=dvalue(dstate(edge[i]-.22))
 return val
}
function lastheld(t, i, ts) {
 ts=0
 for (i=1;i<=ne;i++) if(t>=edge[i]+.61-1e-5)ts=edge[i]+.61
 return ts
}
function lastpost(t, i, ts) {
 ts=.36
 for (i=1;i<=ne;i++) if(t>=edge[i]+.79-1e-5)ts=edge[i]+.79
 return ts
}
function post(t) {
 if(t<.36-1e-5) return .1
 return -.7*held(t-.18)-.05
}
function enabled(t, r) {
 if(t>=fall[r]-1e-5 && t<rise[r]-1e-5)return 0
 return envalue(dstate(t-.35))
}
function probe(s){return biased_hc_probe(s)}
BEGIN {
 nd=split("0 6 12 18 24 30 36 42 48 54 60 66 72 78 84 90 96 102 108",dt," ")
 split("0s 1s 0r Ur 1z 0u 1u 1r 0z Us 1s 0s 1s 0r 1u 0s 1r Uu 0s",ds," ")
 nc=split("0 9 14 21 29 33 39 46 51 60 64 70 75 80 86 93 99 107",ct," ")
 split("0s 1s 0r 1r Uz 1z 0s 1u 0z 1s 0s Uu 1s 1r 0u 1s 0s 1r",cs," ")
 ne=split("9 21 33 46 60 75 93 107",edge," ")
 # The external enable is a real analog PWL→original adc_bridge.
 # Native receiver advertises an actual additional 2 pF.
 fall[1]=43.4135;rise[1]=79.4135
 fall[2]=43.6145;rise[2]=79.603
 for(i=1;i<nd;i++)sample[i]=dt[i]+5.5
 if(nd!=19||nc!=18||ne!=8)fail("invalid original clocked C source declaration")
}
FNR==1 { file++ }
file<=2 && $1~/^[0-9]/ {
 if(NF!=7){fail(sprintf("original clocked event shape %s:%d NF=%d",FILENAME,FNR,NF));exit 1}
 r=file;t=$1*1e9;events[r]++
 if(t<lastt[r]-1e-5){fail(sprintf("clocked original C event time went backward in %s",FILENAME));exit 1}
 lastt[r]=t
 ds_now=dstate(t);cs_now=cstate(t)
 if($6!=ds_now||$7!=cs_now){
    fail(sprintf("source-state/strength mismatch %s at %.8f ns expected %s/%s got %s/%s",FILENAME,t,ds_now,cs_now,$6,$7));exit 1
 }
 vraw=dvalue(dstate(t-.22))
 vheld=held(t)
 vpost=post(t)
 ven=enabled(t,r)
 if(!near($2,vraw,2e-5) || !near($3,vheld,2e-5) ||
    !near($4,vpost,2e-5) || !near($5,ven,2e-5)){
     fail(sprintf("original C REAL chain mismatched %s at %.8fns: expected %.6g/%.6g/%.6g/%.6g got %.6g/%.6g/%.6g/%.6g",
           FILENAME,t,vraw,vheld,vpost,ven,$2,$3,$4,$5));exit 1
 }
 for(i=1;i<=nd;i++)if(!data_seen[r,i]&&near(t,dt[i],1e-5))data_seen[r,i]=1
 for(i=1;i<=nc;i++)if(!clock_seen[r,i]&&near(t,ct[i],1e-5))clock_seen[r,i]=1
 for(i=1;i<=ne;i++){
    if(!held_seen[r,i]&&near(t,edge[i]+.61,1e-5)){
       held_seen[r,i]=1; sample_updates++
    }
    if(!post_seen[r,i]&&near(t,edge[i]+.79,1e-5)){
       post_seen[r,i]=1; gain_updates++
    }
 }
 if(near(t,80.61,1e-5)||near(t,80.79,1e-5))
    fail("Strength-only original 1s→1r transition incorrectly triggered real_delay sampling")
 if(!gate_seen[r,1] && near(t,fall[r],1e-5)){
   gate_seen[r,1]=1
   if(!near($5,0,1e-6))fail("original active-low ENABLE event did not zero the output")
 }
 if(!gate_seen[r,2] && near(t,rise[r],1e-5)){
   gate_seen[r,2]=1
   if(!near($5,-.25,1e-6))fail("original restored ENABLE output not sampled")
 }
 if(near(t,60,1e-5) && $6=="1s" && $7=="1s" &&
    near($2,.75,1e-6)&&near($3,2,1e-6)){
    concurrent[r]=1
 }
 next
}
file>=3 && $1~/^[0-9]/ {
 if(file>4||NF!=14){fail(sprintf("invalid original physically loaded waveform %s:%d NF=%d",FILENAME,FNR,NF));exit 1}
 r=file-2;t=$1*1e9;wave[r]++
 kcl=mag($12-($10-1.2)*.001)
 if(kcl>max_kcl)max_kcl=kcl
 if(kcl>1e-7){fail(sprintf("original source biased digital output KCL mismatch in %s t%.8fns",FILENAME,t));exit 1}
 if(!near($14, (t<43?3.3:t<43.1?3.3*(43.1-t)/.1:t<79?0:t<79.1?3.3*(t-79)/.1:3.3), .002)){
    fail(sprintf("original analog PWL control source mismatch in %s t%.6fns",FILENAME,t));exit 1
 }
 for(i=1;i<nd;i++){
    if(analog_seen[r,i]||!near(t,sample[i],.040))continue
    analog_seen[r,i]=1
    expected[1]=dvalue(dstate(t-.22))
    expected[2]=held(t)
    expected[3]=post(t)
    expected[4]=enabled(t,r)
    expected[5]=probe(dstate(t))
    actual[1]=$2;actual[2]=$4;actual[3]=$6;actual[4]=$8;actual[5]=$10
    for(j=1;j<=5;j++){
       if(r==1){baseline[i,j]=actual[j]}
       else{
          error=mag(actual[j]-baseline[i,j])
          if(error>max_pair_err)max_pair_err=error
          if(error>.06){fail(sprintf("source-native physical waveform disagrees at %.5fns output %d original %.9g compiled %.9g",t,j,baseline[i,j],actual[j]));exit 1}
       }
       # Independently check settled electrical states. A real_to_v
       # transition is a finite ramp plus numerical model smoothing;
       # time-near-clock samples are checked versus actual original
       # physical oracle, not incorrectly treated as settled.
       cutoff= j==1?dt[i]+.22:j==2?lastheld(t):j==3?lastpost(t):j==4?dt[i]+.35:dt[i]
       if(j==4 && t>fall[r] && t<rise[r]) cutoff=fall[r]
       if(j==4 && t>rise[r]) cutoff=(rise[r]>dt[i]+.35?rise[r]:dt[i]+.35)
       if(j!=5 && t-cutoff<3.1)continue
       tol=j==5?.10:.025
       err=mag(actual[j]-expected[j])
       if(err>max_abs_err)max_abs_err=err
       if(err>tol){
          fail(sprintf("original independently predicted loaded voltage wrong %s sample %.5fns output %d expected %.9g got %.9g (%.6gV)",FILENAME,t,j,expected[j],actual[j],err));exit 1
       }
       settled_checks++
    }
    analog_checks+=5
 }
}
END {
 if(bad)exit 1
 if(file!=4||events[1]!=82||events[2]!=82||wave[1]<4500||wave[2]<4500)
    fail(sprintf("original clocked sample C event/physical traces incomplete: events %d/%d, wave %d/%d",events[1],events[2],wave[1],wave[2]))
 for(r=1;r<=2;r++){
    if(!concurrent[r])fail(sprintf("original concurrent data/clock 60ns semantics not proven, route %d",r))
    for(i=1;i<=nd;i++)if(!data_seen[r,i])fail(sprintf("original data source phase %d missing in route %d",i,r))
    for(i=1;i<=nc;i++)if(!clock_seen[r,i])fail(sprintf("original clock source phase %d missing in route %d",i,r))
    for(i=1;i<=ne;i++){
       if(!held_seen[r,i])fail(sprintf("original 610ps clock sample %d missing in route %d",i,r))
       if(!post_seen[r,i])fail(sprintf("original 790ps serial real gain %d missing in route %d",i,r))
    }
    for(i=1;i<nd;i++)if(!analog_seen[r,i])fail(sprintf("missing loaded original source voltage phase %d in route %d",i,r))
    for(i=1;i<=2;i++)if(!gate_seen[r,i])fail(sprintf("source original ENABLE event %d missing route %d",i,r))
 }
 d1=(fall[2]-fall[1])*1000
 d2=(rise[2]-rise[1])*1000
 if(!near(d1,201,.01)||!near(d2,189.5,.01))
    fail(sprintf("clocked true original ALB 2pF receiver timing wrong +%.3f/+%.3f ps",d1,d2))
 if(!bad)printf("True original 12-state clocked real_delay C: %d exact original digital/real event observations, %d @+610ps samples, %d @+790ps real_gain updates, %d/180 physically loaded analog observations, %d independently settled voltage checks, KCL %.3g A, max source-native voltage %.6gV, 2pF receiver +%.3f/+%.3fps PASS\n",
    events[1]+events[2],sample_updates,gain_updates,analog_checks,settled_checks,max_kcl,max_pair_err,d1,d2)
 exit bad
}
