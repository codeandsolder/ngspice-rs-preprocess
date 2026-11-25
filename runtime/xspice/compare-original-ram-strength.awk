# Independent stock XSPICE d_ram output strength oracle. Both inputs are
# actual ngspice waveforms from original C d_ram, not precomputed expected CSVs.
# Each row is time, i(Vpull), v(Q0), v(Q1) from wrdata (pairs of time/value).
# The two 1-kohm pullups to a real 1.2 V source distinguish HI_Z from
# driven UNKNOWN, which the ordinary dac_bridge would encode identically.
function abs(x) { return x<0 ? -x : x }
function expected_voltage(state) {
    if (state=="Z") return 1.2000045
    if (state=="X") return .433333333
    if (state=="0") return .034285714
    if (state=="1") return 3.21542623
    print "invalid expected state: " state > "/dev/stderr"
    exit 2
}
BEGIN {
    n=split("1 3 10 16 22 35 41 51 62 72 85 100 112", sample_ns, " ")
    split("Z X Z 1 X Z 0 1 Z 1 Z 1 X", q0, " ")
    split("Z X Z 0 X Z 1 0 Z 0 Z 0 X", q1, " ")
    if (n!=13) exit 2
    trace=1
}
FNR==1 && NR>1 { trace=2 }
{
    if (NF<6 || (FNR>1 && $1<=last[trace])) {
        printf "malformed/nonmonotonic stock RAM strength waveform %s line %d\n",FILENAME,FNR > "/dev/stderr"
        failed=1
        exit 2
    }
    last[trace]=$1
    rows[trace]++
    for (i=1; i<=n; i++) {
        if (abs($1*1e9-sample_ns[i]) > .04) continue
        states[1]=q0[i]; states[2]=q1[i]
        for (bit=1; bit<=2; bit++) {
            voltage=bit==1 ? $4 : $6
            error=abs(voltage-expected_voltage(states[bit]))
            if (error>.02) {
                printf "original d_ram path=%d sample=%.1f ns Q%d expected %s (%.7f V), got %.7f V\n",trace,sample_ns[i],bit-1,states[bit],expected_voltage(states[bit]),voltage > "/dev/stderr"
                failed=1
                exit 1
            }
            if (error>max_error) max_error=error
        }
        current=((1.2-$4)+(1.2-$6))*.001
        if (abs($2+current)>1e-5) {
            printf "original RAM native physical current inconsistent with 1k pull: %.8g A vs %.8g A\n",$2,-current > "/dev/stderr"
            failed=1
            exit 1
        }
        if (abs($2)>max_abs_current) max_abs_current=abs($2)
        count[trace,i]++
    }
}
END {
    if (failed) exit 1
    if (trace!=2 || rows[1]<5000 || rows[2]<5000) {
        print "original RAM strength oracle missing independent full traces" > "/dev/stderr"
        exit 1
    }
    for (which=1;which<=2;which++) {
        for (i=1;i<=n;i++) {
            if (count[which,i]<2) {
                printf "missing physical RAM plateau: path %d time %.1f ns\n",which,sample_ns[i] > "/dev/stderr"
                failed=1
            }
        }
    }
    if (!failed) printf "Original stock d_ram strength: %d independent plateau times x 2 bits x 2 simulator paths PASS, max error %.6g V, peak |pull-source current| %.6g A, %d/%d waveform rows\n",n,max_error,max_abs_current,rows[1],rows[2]
    exit failed
}
