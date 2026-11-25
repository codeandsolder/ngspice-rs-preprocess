# Genuine original mixed analog/digital ADC+AND+DAC physical oracle.
# Direct stock ngspice47 and source-driven ALB native-X are executed separately.
# Five wrdata traces: 2 physical DAC outputs, differential source + common-mode,
# independently toggled digital source.
function abs(x){return x<0?-x:x}
function error(message) { print message > "/dev/stderr"; fail=1 }
BEGIN {
    n=split("5 15 25 35 45 55 65 75 85 95 105 115 123",phase," ")
    for(i=1;i<=n;i++){
        quarter=(i-1)%4
        analog[i]=(quarter==1||quarter==2)?1:0
        digital[i]=(quarter>=2)?1:0
        output[i]=analog[i]*digital[i]
    }
    # Distinct ADC-origin and ALB-origin causal paths: 200 ps receiver
    # loading is expected only on the original digital-source gate rises.
    for(edge=10;edge<=90;edge+=40){
        j=++edges
        start[j]=edge
        bit[j]=0
        rise[j]=1
        delay[j]=0
    }
    for(edge=30;edge<=110;edge+=40){
        j=++edges
        start[j]=edge
        bit[j]=0
        rise[j]=0
        delay[j]=0
    }
    for(edge=20;edge<=100;edge+=40){
        j=++edges
        start[j]=edge
        bit[j]=1
        rise[j]=1
        delay[j]=200
    }
    for(edge=30;edge<=110;edge+=40){
        j=++edges
        start[j]=edge
        bit[j]=1
        rise[j]=0
        delay[j]=0
    }
    trace=1
}
FNR==1 && NR>1 {trace=2}
{
    t=$1
    if(NF!=10 || (FNR>1&&t<=last[trace])){
        error(sprintf("mixed original waveform malformed: %s row %d",FILENAME,FNR))
        exit 1
    }
    count[trace]++
    vals[0]=$2
    vals[1]=$4
    for(i=1;i<=n;i++){
        if(abs(t*1e9-phase[i])>.045) continue
        expected[0]=3.3*analog[i]
        expected[1]=3.3*output[i]
        for(bitnum=0;bitnum<=1;bitnum++){
            delta=abs(vals[bitnum]-expected[bitnum])
            if(delta>.025){
                error(sprintf("original mixed bridge %s @ %g ns bit %d expected %.6f V got %.6f V",FILENAME,phase[i],bitnum,expected[bitnum],vals[bitnum]))
                exit 1
            }
            if(delta>max_err)max_err=delta
        }
        # Real physical differential input remains 0/3.3 V even though
        # both original ADC terminals have a +1 V common-mode shift.
        if(abs($8-1)>.00001 || abs(($6-$8)-3.3*analog[i])>.00001 \
           || abs($10-3.3*digital[i])>.00001){
            error(sprintf("original differential source / common-mode corrupted %s %.1f ns",FILENAME,phase[i]))
            exit 1
        }
        observations[trace,i]++
    }
    for(j=1;j<=edges;j++){
        if(t*1e9<=start[j]+.01||t*1e9>=start[j]+5||seen[trace,j]||FNR<=1)continue
        v=vals[bit[j]]
        prior=prev[trace,bit[j]]
        cross=rise[j]?(prior<1.65&&v>=1.65):(prior>1.65&&v<=1.65)
        if(cross){
            crossing[trace,j]=oldtime[trace]+(t-oldtime[trace])*(1.65-prior)/(v-prior)
            seen[trace,j]=1
        }
    }
    last[trace]=t
    oldtime[trace]=t
    prev[trace,0]=vals[0]
    prev[trace,1]=vals[1]
}
END {
    if(fail)exit 1
    if(trace!=2||count[1]<5500||count[2]<5500){
        error("original mixed bridge incomplete stock/native physical waveforms")
        exit 1
    }
    for(route=1;route<=2;route++)
      for(i=1;i<=n;i++)if(!observations[route,i]){
        error(sprintf("missing mixed physical phase %g route %d",phase[i],route))
      }
    for(j=1;j<=edges;j++){
        if(!seen[1,j]||!seen[2,j]){
            error(sprintf("missing mixed bridge timing crossing %.2f ns channel %d",start[j],bit[j]))
            continue
        }
        difference=(crossing[2,j]-crossing[1,j])*1e12
        if(abs(difference-delay[j])>35){
            error(sprintf("mixed bridge expected %d ps loading, observed %.3f ps @ %d ns channel %d",delay[j],difference,start[j],bit[j]))
            continue
        }
        if(delay[j]){
            if(!delayed||difference<min_delay)min_delay=difference
            if(!delayed||difference>max_delay)max_delay=difference
            sum_delay+=difference
            delayed++
        }
        checked++
    }
    if(!fail){
        printf "Original differential ADC + AND + DAC: %d plateau phases x 2 analog outputs x 2 simulators = %d physical observations, %d matched causal transitions, digital 2pF load delay min %.3f / avg %.3f / max %.3f ps, max voltage error %.4g V, source rows %d/%d PASS\n",n,n*4,checked,min_delay,sum_delay/delayed,max_delay,max_err,count[1],count[2]
    }
    exit fail
}
