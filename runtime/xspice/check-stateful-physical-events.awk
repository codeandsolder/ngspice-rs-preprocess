# Reference is an independently measured vendor CSV, including zero-event pins.
BEGIN {
    FS="[ ,\t]+"
    if(tolerance_ns=="") tolerance_ns=0.12
    if(sample_tolerance=="") sample_tolerance=.05
    if(sample_relative_tolerance<0) { bad=1; exit 1 }
    if(unit=="") unit="V"
    voltage_window=voltage_time_tolerance_ns*1e-9
    current_window=current_time_tolerance_ns*1e-9
    if(voltage_window<0 || current_window<0 ||
       voltage_time_tolerance_ns>tolerance_ns ||
       current_time_tolerance_ns>tolerance_ns) { bad=1; exit 1 }
    pin_count=split(pins,ordered,",")
    if (!pin_count) { bad=1; exit 1 }
    for (i=1;i<=pin_count;i++) {
        pin=ordered[i]
        if (pin=="" || pin in column) { bad=1; exit 1 }
        column[pin]=2*i
    }
    pair_count=parse_pairs(equal_pairs,1)
    pair_count+=parse_pairs(distinct_pairs,0)
    pair_count+=parse_pairs(relative_pairs,2)
    if(relative_pairs!="" && relative_tolerance_ns<=0) { bad=1; exit 1 }
    if (quiet_pins!="") {
        quiet_count=split(quiet_pins,quiet,",")
        for(i=1;i<=quiet_count;i++){
            if (!(quiet[i] in column)) { bad=1; exit 1 }
            suppressed[quiet[i]]=1
        }
    }
}
function parse_pairs(spec,mode,    items,parts,n,i,k) {
    if(spec=="") return 0
    n=split(spec,items,",")
    for(i=1;i<=n;i++){
        if(split(items[i],parts,":")!=2 || !(parts[1] in column) || !(parts[2] in column) || parts[1]==parts[2]){
            bad=1
            exit 1
        }
        k=pair_count+i
        pair_left[k]=parts[1]
        pair_right[k]=parts[2]
        pair_mode[k]=mode
    }
    return n
}
function add_threshold(pin,v,    k) {
    if(!((pin,v) in threshold_seen)){
        threshold_seen[pin,v]=1
        k=++threshold_count[pin]
        thresholds[pin,k]=v
    }
}
FNR==NR {
    if ($1!=name) next
    pin=$2
    if (!(pin in column)) { bad=1; next }
    if ($3=="initial") { initial[pin]=$5+0; next }
    if ($3=="sample") {
        n=++sample_count[pin]
        sample_time[pin,n]=$4*1e-9
        sample_value[pin,n]=$5+0
        next
    }
    threshold=$5+0
    add_threshold(pin,threshold)
    if ($3=="threshold") next
    n=++expected[pin,threshold]
    direction[pin,threshold,n]=$3
    reference[pin,threshold,n]=$4+0
    next
}
NF>=2*pin_count {
    # wrdata may have leading whitespace, so the first field is empty.
    offset=($1=="")
    t=$(1+offset)+0
    for(i=1;i<=pin_count;i++){
        pin=ordered[i]
        if(!sample_only && !samples && !threshold_count[pin] && !current_sample_pin(pin)) add_threshold(pin,2.5)
        v=$(column[pin]+offset)+0
        if(sample_window(pin)>0) trace[pin,samples+1]=v
        if(!samples && (!(pin in initial) || abs(v-initial[pin])>pin_tolerance(pin,initial[pin]))){
            printf "%s %s initial %.6f differs from vendor %.6f\n",name,pin,v,initial[pin] >"/dev/stderr"
            bad=1
        }
        for(j=1;j<=threshold_count[pin];j++){
            threshold=thresholds[pin,j]
            if(samples && (previous[pin]-threshold)*(v-threshold)<0){
                n=++observed[pin,threshold]
                crossing=(last_time+(t-last_time)*(threshold-previous[pin])/(v-previous[pin]))*1e9
                if(relative_pairs!="" && threshold==2.5) actual_crossing[pin,n]=crossing
                delta=crossing-reference[pin,threshold,n]
                d=v>previous[pin]?"rise":"fall"
                if(n>expected[pin,threshold] || direction[pin,threshold,n]!=d || abs(delta)>tolerance_ns){
                    printf "%s %s %gV #%d %s %.9f ns differs from vendor %s %.9f ns\n",name,pin,threshold,n,d,crossing,direction[pin,threshold,n],reference[pin,threshold,n] >"/dev/stderr"
                    bad=1
                } else if(abs(delta)>worst) worst=abs(delta)
            }
        }
        while(samples && (n=sample_seen[pin]+1)<=sample_count[pin] && sample_due(sample_time[pin,n],t)){
            at=sample_time[pin,n]
            # Decimal ns reference conversion may round one ULP beyond
            # the final native timestamp. Snap only roundoff, not a
            # genuinely missing sample, to the available endpoint.
            actual=at>=t ? v : previous[pin]+(v-previous[pin])*(at-last_time)/(t-last_time)
            if(abs(actual-sample_value[pin,n])>pin_tolerance(pin,sample_value[pin,n])){
                if(sample_window(pin)>0) {
                    # Defer only explicit time-bounded sample mismatches.
                    # Initial values remain exact.
                    mismatch[pin,n]=actual
                } else {
                    report_sample(pin,n,actual)
                }
            }
            sample_seen[pin]++
        }
        if(abs(v)>peak[pin]) peak[pin]=abs(v)
        previous[pin]=v
    }
    for(i=1;i<=pair_count;i++){
        if(pair_mode[i]==2) continue
        difference=abs(previous[pair_left[i]]-previous[pair_right[i]])
        if(difference>pair_peak[i]) pair_peak[i]=difference
    }
    last_time=t
    samples++
    if(voltage_window>0 || current_window>0) trace_time[samples]=t
}
# Mixed physical event traces can specify a separate absolute ampere
# tolerance. Current-only traces using sample_only keep their existing ABI.
function current_sample_pin(pin) {
    return current_tolerance!="" && pin~/^i\(/
}
# Vendor print precision is relative for very large current values.
# Existing callers retain their exact absolute tolerances.
function pin_tolerance(pin,ref) {
    return (current_sample_pin(pin)?current_tolerance:sample_tolerance)+sample_relative_tolerance*abs(ref)
}
function pin_unit(pin) { return current_sample_pin(pin)?"A":unit }
# Apply an event-time window to current samples only when explicitly
# requested. Static Norton matrices retain exact-time comparisons.
function sample_window(pin) {
    return pin~/^i\(/ ? current_window : voltage_window
}
function abs(v){ return v<0?-v:v }
function sample_due(at,t) {
    return at<=t || at-t<=4.440892098500626e-16*abs(t)
}
function report_sample(pin,n,actual) {
    printf "%s %s sample %.9f ns: %.6f %s differs from vendor %.6f %s\n",name,pin,sample_time[pin,n]*1e9,actual,pin_unit(pin),sample_value[pin,n],pin_unit(pin) >"/dev/stderr"
    bad=1
}
# Find the piecewise-linear native physical value over the allowed time.
# Check both interpolated endpoints and every interior point.
# A matching sample may differ in time by no more than the crossing bound.
function window_matches(pin,n,    span,lo,hi,k,a,b,v,vmin,vmax,ref) {
    span=sample_window(pin)
    lo=sample_time[pin,n]-span
    hi=sample_time[pin,n]+span
    if(lo<trace_time[1]) lo=trace_time[1]
    if(hi>trace_time[samples]) hi=trace_time[samples]
    if(lo>hi) return 0
    k=window_start[pin]+0
    if(k<1) k=1
    while(k<samples && trace_time[k+1]<=lo) k++
    window_start[pin]=k
    a=trace_time[k]; b=trace_time[k+1]
    v=trace[pin,k]
    if(k<samples && b>a) v+=(trace[pin,k+1]-v)*(lo-a)/(b-a)
    vmin=vmax=v
    while(k<samples && trace_time[k+1]<=hi) {
        k++
        v=trace[pin,k]
        if(v<vmin) vmin=v
        if(v>vmax) vmax=v
    }
    if(k<samples && trace_time[k+1]>trace_time[k]) {
        v=trace[pin,k]+(trace[pin,k+1]-trace[pin,k])*(hi-trace_time[k])/(trace_time[k+1]-trace_time[k])
        if(v<vmin) vmin=v
        if(v>vmax) vmax=v
    }
    ref=sample_value[pin,n]
    return ref>=vmin-pin_tolerance(pin,ref) && ref<=vmax+pin_tolerance(pin,ref)
}
END {
    for(i=1;i<=pin_count;i++){
        pin=ordered[i]
        for(j=1;j<=threshold_count[pin];j++){
            threshold=thresholds[pin,j]
            if(observed[pin,threshold]!=expected[pin,threshold]){
                printf "%s %s %gV expected %d crossings, observed %d\n",name,pin,threshold,expected[pin,threshold],observed[pin,threshold] >"/dev/stderr"
                bad=1
            }
            total+=observed[pin,threshold]
        }
        for(n=1;n<=sample_seen[pin];n++) {
            if((pin,n) in mismatch) {
                if(window_matches(pin,n)) window_accepted++
                else report_sample(pin,n,mismatch[pin,n])
            }
        }
        if(sample_seen[pin]!=sample_count[pin] || (sample_only && !sample_count[pin])){
            printf "%s %s missing samples\n",name,pin >"/dev/stderr"
            bad=1
        }
        if(pin in suppressed && peak[pin]>.05){
            printf "%s %s suppressed pulse peak %.6f V (vendor identically zero)\n",name,pin,peak[pin] >"/dev/stderr"
            bad=1
        }
    }
    for(i=1;i<=pair_count;i++){
        if(pair_mode[i]==2){
            left=pair_left[i]; right=pair_right[i]
            count=expected[left,2.5]
            if(!count || count!=expected[right,2.5]) { bad=1; continue }
            for(n=1;n<=count;n++){
                ref_delta=reference[right,2.5,n]-reference[left,2.5,n]
                actual_delta=actual_crossing[right,n]-actual_crossing[left,n]
                if(direction[left,2.5,n]!=direction[right,2.5,n] || abs(actual_delta-ref_delta)>relative_tolerance_ns){
                    printf "%s pair %s/%s crossing %d relative delay %.6f ps differs from vendor %.6f ps\n",name,left,right,n,actual_delta*1000,ref_delta*1000 >"/dev/stderr"
                    bad=1
                }
            }
        } else if((pair_mode[i] && pair_peak[i]>1e-6) || (!pair_mode[i] && pair_peak[i]<1)){
            printf "%s pair %s/%s expected %s, max difference %.9f V\n",name,pair_left[i],pair_right[i],pair_mode[i]?"equal":"distinct",pair_peak[i] >"/dev/stderr"
            bad=1
        }
    }
    if(!samples || bad) exit 1
    if(window_accepted) printf "%s: %d samples matched within explicit time windows (voltage %.3f ps, current %.3f ps)\n",name,window_accepted,voltage_window*1e12,current_window*1e12
    if(sample_only) {
        for(i=1;i<=pin_count;i++) checked+=sample_seen[ordered[i]]
        printf "%s: %d/%d outputs, %d sampled observations PASS\n",name,pin_count,pin_count,checked
        exit 0
    }
    printf "%s: %d/%d outputs, %d crossings, max residual %.3f ps PASS\n",name,pin_count,pin_count,total,worst*1000
}
