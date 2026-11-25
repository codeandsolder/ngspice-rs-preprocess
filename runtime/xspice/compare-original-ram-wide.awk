# Stock Georgia Tech d_ram 4x8 physical event and drive-strength oracle.
# Compare two *independently executed* original code models: direct XSPICE
# versus native ALB-wired XSPICE, with an independent 2 pF receiver load.
# Both traces use wrdata time/value pairs:
# t i(Vpull) t v(Q0) t v(Q1) t v(Q2) t v(Q3).
function abs(x) { return x < 0 ? -x : x }
function level(c) {
    if (c == "Z") return 1.2000045
    if (c == "X") return .433333333
    if (c == "0") return .034285714
    if (c == "1") return 3.21542623
    printf "unknown reference logic state %s\n", c > "/dev/stderr"
    failed=1
    return -1
}
BEGIN {
    # Independent, authored memory oracle. Stored words:
    # address 0 = D3..D0 1010, address 5 = D3..D0 0101.
    # Strings below are in increasing output port order Q0..Q3.
    n=split("1 5 11 16 22 27 35 44 55 65 78 89 95 105", sample_ns, " ")
    expected[1]="ZZZZ"
    expected[2]="XXXX"
    expected[3]="ZZZZ"
    expected[4]="0101"
    expected[5]="XXXX"
    expected[6]="ZZZZ"
    expected[7]="1010"
    expected[8]="ZZZZ"
    expected[9]="0101"
    expected[10]="1010"
    expected[11]="XXXX"
    expected[12]="0101"
    expected[13]="ZZZZ"
    expected[14]="0101"
    if (n != 14) exit 2
    trace=1
}
FNR == 1 && NR > 1 { trace=2 }
{
    time=$1
    if (NF < 10 || (FNR > 1 && time <= last_time[trace])) {
        printf "invalid original RAM wide waveform %s line %d\n", FILENAME, FNR > "/dev/stderr"
        failed=1
        exit 2
    }
    rows[trace]++
    current_from_pull=0
    for (bit=0; bit<4; bit++) {
        col=4+2*bit
        voltage[bit]=$(col)
        current_from_pull += (1.2-voltage[bit]) * 1e-3
    }
    # Full-waveform KCL of the real 1.2 V pull rail is independently
    # checked instead of merely asserting the final voltages.
    kcl=abs($2+current_from_pull)
    if (kcl > max_kcl) max_kcl=kcl
    if (kcl > 1e-5) {
        printf "original wide RAM KCL violated at t=%.6g s (%.9g A)\n",time,kcl > "/dev/stderr"
        failed=1
        exit 1
    }
    for (i=1; i<=n; i++) {
        if (abs(time*1e9-sample_ns[i]) > .035) continue
        for (bit=0; bit<4; bit++) {
            want=level(substr(expected[i],bit+1,1))
            delta=abs(voltage[bit]-want)
            if (delta > .025) {
                printf "original wide RAM %s t=%.1f ns Q%d expected %s (%.7g V), got %.7g V\n",FILENAME,sample_ns[i],bit,substr(expected[i],bit+1,1),want,voltage[bit] > "/dev/stderr"
                failed=1
                exit 1
            }
            if (delta > max_voltage_error) max_voltage_error=delta
        }
        counts[trace,i]++
    }
    # Prove the independent receiver's 200 ps timing from two physical
    # read-enable transitions across *all four* original RAM output bits.
    for (phase=1; phase<=2; phase++) {
        start=phase==1 ? 13e-9 : 30e-9
        stop=phase==1 ? 18e-9 : 35e-9
        if (time < start || time > stop) continue
        sequence=phase==1 ? "0101" : "1010"
        for (bit=0; bit<4; bit++) {
            c=substr(sequence,bit+1,1)
            threshold=c=="0" ? .6 : 2.0
            prev=previous_voltage[trace,bit]
            curr=voltage[bit]
            crossed=c=="0" ? (prev > threshold && curr <= threshold) : (prev < threshold && curr >= threshold)
            if (FNR>1 && crossed && !has_crossing[trace,phase,bit]) {
                tprev=previous_time[trace]
                at[trace,phase,bit]=tprev+(time-tprev)*(threshold-prev)/(curr-prev)
                has_crossing[trace,phase,bit]=1
            }
        }
    }
    previous_time[trace]=time
    last_time[trace]=time
    for (bit=0;bit<4;bit++) previous_voltage[trace,bit]=voltage[bit]
}
END {
    if (failed) exit 1
    if (trace != 2 || rows[1]<5000 || rows[2]<5000) {
        print "missing original and native physical RAM traces" > "/dev/stderr"
        exit 1
    }
    for (path=1;path<=2;path++) {
        for (i=1;i<=n;i++) {
            if (counts[path,i]<2) {
                printf "missing original wide RAM settled plateau path=%d at %.1f ns\n",path,sample_ns[i] > "/dev/stderr"
                failed=1
            }
        }
    }
    for (phase=1;phase<=2;phase++) {
        for (bit=0;bit<4;bit++) {
            if (!has_crossing[1,phase,bit] || !has_crossing[2,phase,bit]) {
                printf "missing physical RAM drive timing phase=%d bit=%d\n",phase,bit > "/dev/stderr"
                failed=1
                continue
            }
            delay=(at[2,phase,bit]-at[1,phase,bit])*1e12
            if (delay<150 || delay>250) {
                printf "wide original RAM physical loading phase=%d bit=%d delay=%.3f ps (expected 150–250 ps)\n",phase,bit,delay > "/dev/stderr"
                failed=1
            }
            if (delay<min_delay || !transitions) min_delay=delay
            if (delay>max_delay || !transitions) max_delay=delay
            sum_delay+=delay
            transitions++
        }
    }
    if (!failed) printf "Original 4-bit x 8-word d_ram: %d physical phase samples x 4 bits x 2 routes, %d timed transitions (receiver load min %.3f / avg %.3f / max %.3f ps), max plateau error %.7g V, max KCL %.5g A, %d/%d waveform rows PASS\n",n,transitions,min_delay,sum_delay/transitions,max_delay,max_voltage_error,max_kcl,rows[1],rows[2]
    exit failed
}
