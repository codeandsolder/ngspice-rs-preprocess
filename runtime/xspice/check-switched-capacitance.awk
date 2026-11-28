# Independently predict pending XSPICE digital events and switched
# *analog* capacitance physics. Digital LOAD() is a 2pF INIT-time
# registration; physically switched CEXTRA must not retroactively
# reschedule the queued 5ns + 1k*2pF original digital event.
#
# Inputs: -v cap_pf={1,150,300} digital-events.txt result.dat
# wrdata result column pairs: t,vout,t,ain,t,gate,t,cap,t,I(Vcapsense),t,capnode.
function abs(x){return x<0?-x:x}
function near(a,b,tol){return abs(a-b)<=tol}
function die(message){print "Switched analog capacitor oracle: " message > "/dev/stderr"; bad=1}
function sample(t,target){return t>=target && t<target+.028}
function expected_analog(cap,t){
 if (t==22) return cap==1?3.164:cap==150?.807:.617
 if (t==30) return cap==1?3.164:cap==150?1.424:.936
 if (t==48) return cap==1?3.164:cap==150?2.624:1.624
 return 0
}
BEGIN {
 if(cap_pf!=1&&cap_pf!=150&&cap_pf!=300)die("unsupported physical capacitance " cap_pf "pF")
 e_t[1]=0; e_t[2]=10.051;e_t[3]=17.051;e_t[4]=70.071;e_t[5]=77.071
 e_din[1]="0s";e_din[2]="1s";e_din[3]="1s";e_din[4]="0s";e_din[5]="0s"
 e_dout[1]="0s";e_dout[2]="0s";e_dout[3]="1s";e_dout[4]="1s";e_dout[5]="0s"
}
FNR==1 {file++}
file==1 && $1 ~ /^[0-9]/ {
 n_events++;t=$1*1e9
 if(NF!=3 || n_events>5 || !near(t,e_t[n_events],.0002) ||
   $2!=e_din[n_events] || $3!=e_dout[n_events]) {
   die(sprintf("wrong digital event #%d t %.9fns input %s output %s",n_events,t,$2,$3))
   exit 1
 }
 next
}
file==2 && $1 ~ /^[0-9]/ {
 count++
 if(NF!=12){die("unexpected physical wrdata vector width");exit 1}
 t=$1*1e9;vout=$2;ain=$4;gate=$6;cap=$8;current=$10;capnode=$12
 for (i=3;i<=11;i+=2)
    if(!near($i,$1,1e-16)){die("SPICE output time columns do not share a physical timestep");exit 1}
 if(t<prev_t){die("physical time went backwards");exit 1}
 prev_t=t
 # Input ANALOG source and switched capacitor voltage must be independently
 # present at the right physical time relative to an already-queued event.
 if(sample(t,8)&&!before) {
   before=1
   if(abs(vout)>.02 || abs(ain)>.01 || abs(gate)>.01)die("wrong before-pulse physical state")
 }
 if(sample(t,13)&&!pending) {
   pending=1
   if(abs(vout)>.02 || ain<3.2 || gate<3.2 || abs(cap)>.02)
     die("switch must be ON and low output held while 7ns LUT event is still pending")
 }
 if(sample(t,16.8)&&!pre_event) {
   pre_event=1
   if(abs(vout)>.02 || ain<3.2 || gate<3.2)
      die("physical analog DAC drove before its actual digital event")
 }
 for(i=1;i<=3;i++) {
   sample_t=i==1?22:i==2?30:48
   if(!seen[sample_t]&&sample(t,sample_t)) {
      seen[sample_t]=1
      expected=expected_analog(cap_pf,sample_t)
      if(!near(vout,expected,.085))
         die(sprintf("%gpF analog output %.8f V at %gns; genuine nonlinear DAC / 33ohm series RC expected %.5f",cap_pf,vout,sample_t,expected))
      if(gate<3.2 || ain<3.2)die("switched-cap physical control unexpectedly disabled")
      results[i]=vout
   }
 }
 if(sample(t,51)&&!released) {
    released=1
    if(!near(vout,3.164,.08)||gate>.05)die("physical capacitor release did not restore HIGH analog output")
    if(cap_pf>=150 && cap>2.6)die("switched capacitor charge was lost when switch opened")
 }
 if(sample(t,84)&&!final) {
    final=1
    if(abs(vout)>.05 || ain>.05 || gate>.05)
       die("the physical output failed to return LOW with the analog capacitor disconnected")
 }
 # Two independent KCL witnesses from actual analog branch current:
 # (1) exact Ohm: I = (Vout - Vcap) / Ron;
 # (2) C * dVcap/dt plus 10Meg leak agrees with voltage-source I.
 # Compare only settled switch-on integration region, away from the
 # discontinuity at the switch-open transition and input changes.
 if(t>23 && t<47 && count>1) {
   r_err=abs(current-(vout-capnode)/33)
   if(r_err>max_r_err)max_r_err=r_err
   if(r_err>2e-7){die("source current violates switched 33ohm element KCL");exit 1}
   cap_err=abs(current-(cap_pf*1e-12*(cap-old_cap)/($1-old_time)+cap/1e7))
   if(cap_err>max_c_err)max_c_err=cap_err
   if(cap_err>1e-5){die("measured real capacitor current violates C*dV/dt plus leak");exit 1}
   kcl_checks++
 }
 old_cap=cap;old_time=$1
 next
}
END {
 if(file!=2 || n_events!=5 || count<4500 || kcl_checks<1100)
    die(sprintf("incomplete original digital/physical 2pF event proof: files%d events%d samples%d capacitor%d",file,n_events,count,kcl_checks))
 if(!before||!pending||!pre_event||!seen[22]||!seen[30]||!seen[48]||!released||!final)
    die("missing required physical before / pending / after / release waveforms")
 if(!bad)printf("Switched %gpF true analog C: 5/5 exact XSPICE digital events, 7.000ns invariant staged delay, physical 22/30/48ns %.5f/%.5f/%.5fV, 1k + source-DAC, %d KCL checks (Rerr %.3gA, Cerr %.3gA), %d voltage rows PASS\n",cap_pf,results[1],results[2],results[3],kcl_checks,max_r_err,max_c_err,count)
 exit bad
}
