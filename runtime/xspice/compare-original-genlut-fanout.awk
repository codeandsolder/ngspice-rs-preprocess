# Original 2016 d_genlut with 1991 d_and/d_xor downstream event consumers:
# native code model versus independent source-driven physical ALB compiler.
# wrdata time/value pairs: t current(Vpull), then 5 separate analog outputs.
# The expected physical histories below are authored independently of the
# native X wrapper and of the code-model table under test.
function abs(x) { return x < 0 ? -x : x }
function voltage_for(state) {
    if (state=="0") return .034285714
    if (state=="1") return 3.21542623
    if (state=="Z") return 1.2000045
    if (state=="X") return .433333333
    printf "unexpected authored logic state: %s\n",state > "/dev/stderr"
    failed=1
    return -1
}
BEGIN {
    n=split("5 15 25 35 45 55 65 75 85 95 105 115 123",sample_ns," ")
    for (i=1;i<=n;i++) {
        cycle=(i-1)%4
        expected[i]=cycle==0?"00Z0X":cycle==1?"10001":cycle==2?"01Z0X":"10110"
    }
    # Direct and ALB-native are separately simulated. Verify 24 selected
    # actual physical transitions on Q0, Q1 and downstream F0.
    # At every 10 ns input edge, original Q0 toggles; Q1 and fanout AND
    # have narrower, distinct transition sets.
    for (edge=10;edge<=120;edge+=10) {
        key=++edges
        start[key]=edge
        bit[key]=0
        dir[key]=(edge%20)==10?1:0
    }
    for (edge=20;edge<=110;edge+=10) {
        if (edge%40!=20&&edge%40!=30) continue
        key=++edges
        start[key]=edge
        bit[key]=1
        dir[key]=(edge%40)==20?1:0
    }
    for (edge=30;edge<=120;edge+=10) {
        if (edge%40!=30&&edge%40!=0) continue
        key=++edges
        start[key]=edge
        bit[key]=3
        dir[key]=(edge%40)==30?1:0
    }
    trace=1
}
FNR==1&&NR>1 {trace=2}
{
    time=$1
    if (NF<12 || (FNR>1&&time<=last[trace])) {
        printf "malformed original genlut output %s line %d\n",FILENAME,FNR > "/dev/stderr"
        failed=1
        exit 2
    }
    count[trace]++
    current_from_pull=0
    for (channel=0;channel<5;channel++) {
        volts[channel]=$(4+2*channel)
        current_from_pull+=(1.2-volts[channel])*1e-3
    }
    err=abs($2+current_from_pull)
    if (err>max_kcl)max_kcl=err
    if (err>1e-5) {
        printf "original genlut pull-source KCL mismatch %.9g A at %.7g ns\n",err,time*1e9 > "/dev/stderr"
        failed=1
        exit 1
    }
    for (i=1;i<=n;i++) {
        if (abs(time*1e9-sample_ns[i])>.04) continue
        for (channel=0;channel<5;channel++) {
            want_state=substr(expected[i],channel+1,1)
            want=voltage_for(want_state)
            diff=abs(volts[channel]-want)
            if (diff>.025) {
                printf "original genlut %s t=%g ns Q%d expected %s (%.8fV), got %.8fV\n",FILENAME,sample_ns[i],channel,want_state,want,volts[channel] > "/dev/stderr"
                failed=1
                exit 1
            }
            if (diff>max_voltage_error)max_voltage_error=diff
        }
        samples[trace,i]++
    }
    for (i=1;i<=edges;i++) {
        edge=start[i]
        if (time*1e9<=edge+.1||time*1e9>=edge+5||seen[trace,i]) continue
        channel=bit[i]
        prev=prev_volts[trace,channel]
        now=volts[channel]
        threshold=1.65
        crossing=dir[i] ? prev<threshold&&now>=threshold : prev>threshold&&now<=threshold
        if (FNR>1&&crossing) {
            t_before=prev_time[trace]
            transition[trace,i]=t_before+(time-t_before)*(threshold-prev)/(now-prev)
            seen[trace,i]=1
        }
    }
    last[trace]=time
    prev_time[trace]=time
    for(channel=0;channel<5;channel++) prev_volts[trace,channel]=volts[channel]
}
END {
    if (failed)exit 1
    if (trace!=2||count[1]<5500||count[2]<5500) {
        print "incomplete original direct/native genlut fanout waveforms" > "/dev/stderr"
        exit 1
    }
    for (route=1;route<=2;route++)
        for (i=1;i<=n;i++) if (samples[route,i]<2) {
            printf "missing genlut plateau %.1f ns route %d\n",sample_ns[i],route > "/dev/stderr"
            failed=1
        }
    for (i=1;i<=edges;i++) {
        if (!seen[1,i]||!seen[2,i]) {
            printf "missing native genlut timing event t=%d ns channel=%d\n",start[i],bit[i] > "/dev/stderr"
            failed=1
            continue
        }
        delta=(transition[2,i]-transition[1,i])*1e12
        if(delta<150||delta>250) {
            printf "genlut fanout native loading changed: %.3f ps at t=%d ns output=%d\n",delta,start[i],bit[i] > "/dev/stderr"
            failed=1
        }
        if(!timed||delta<min_delay)min_delay=delta
        if(!timed||delta>max_delay)max_delay=delta
        total_delay+=delta
        timed++
    }
    if(!failed) printf "Original multi-output d_genlut→d_and/d_xor: %d plateau phases × 5 outputs × 2 simulators, %d verified output transitions; 2pF native delay min %.3f mean %.3f max %.3f ps; max V error %.3g V KCL %.3g A, rows %d/%d PASS\n",n,timed,min_delay,total_delay/timed,max_delay,max_voltage_error,max_kcl,count[1],count[2]
    exit failed
}
