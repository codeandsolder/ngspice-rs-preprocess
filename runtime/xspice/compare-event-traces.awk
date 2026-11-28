# Compare independently executed original-XSPICE digital event traces against
# compiler-wired native event traces observed through analog DACs.
#
# Usage: awk -v expected_q='0,1,0,X,...' -v expected_nq='1,0,1,X,...' \
#   -v min_delta_ps=150 -v max_delta_ps=250 \
#   -f compare-event-traces.awk original.dat lowered.dat
#
# Both data files use ngspice's time/value pairs with q in column 2 and
# inverted q in column 4. An intermediate analog DAC transition is not a
# new digital state. Full stable event sequences, UNKNOWN handling and every
# switching latency are independently checked; mere final-state parity is
# insufficient for stateful models.
function absolute(x) { return x < 0 ? -x : x }
function classify(v) {
    if (absolute(v - low_v) <= threshold) return "0"
    if (absolute(v - high_v) <= threshold) return "1"
    if (absolute(v - unknown_v) <= threshold) return "X"
    return ""
}
function record(which, channel, time, v, value, n) {
    value=classify(v)
    if (value == "") return
    if (value == previous[which,channel]) return
    previous[which,channel]=value
    n=++events[which,channel]
    state[which,channel,n]=value
    at[which,channel,n]=time
}
BEGIN {
    low_v=0
    high_v=3.3
    unknown_v=1.65
    threshold=.3
    if (model_name == "") model_name="d_dlatch"
    if (label_q == "") label_q="Q"
    if (label_nq == "") label_nq="NQ"
    which=1
    if (min_delta_ps == 0) min_delta_ps=150
    if (max_delta_ps == 0) max_delta_ps=250
    if (min_samples == 0) min_samples=5000
    if (min_delta_ps < 0 || min_delta_ps >= max_delta_ps ||
        expected_q == "" || expected_nq == "") {
        print "invalid original native event oracle inputs" > "/dev/stderr"
        exit 2
    }
    expectations[1]=split(expected_q,expected_states_q,",")
    expectations[2]=split(expected_nq,expected_states_nq,",")
}
FNR == 1 && NR > 1 { which=2 }
{
    if (NF < 4 || (FNR > 1 && $1 <= last_time[which])) {
        printf "nonmonotonic or malformed original-event waveform in %s line %d\n", FILENAME,FNR > "/dev/stderr"
        bad=1
        exit 2
    }
    last_time[which]=$1
    samples[which]++
    record(which,1,$1,$2)
    record(which,2,$1,$4)
}
END {
    if (bad) exit 2
    if (samples[1] < min_samples || samples[2] < min_samples) {
        print "original/native XSPICE trace has insufficient samples" > "/dev/stderr"
        exit 1
    }
    for (channel=1;channel<=2;channel++) {
        name=channel==1 ? label_q : label_nq
        expected=expectations[channel]
        if (events[1,channel] != expected || events[2,channel] != expected) {
            printf "%s event counts diverged: expected=%d direct=%d native=%d\n",name,expected,events[1,channel],events[2,channel] > "/dev/stderr"
            bad=1
            continue
        }
        for (i=1;i<=expected;i++) {
            want=channel==1 ? expected_states_q[i] : expected_states_nq[i]
            if (state[1,channel,i] != want || state[2,channel,i] != want) {
                printf "%s event %d mismatch expected=%s original=%s native=%s\n",name,i,want,state[1,channel,i],state[2,channel,i] > "/dev/stderr"
                bad=1
                continue
            }
            if (i == 1) continue
            dt=(at[2,channel,i]-at[1,channel,i])*1e12
            if (dt < min_delta_ps || dt > max_delta_ps) {
                printf "%s event %d: relative native delay %.3f ps outside [%g,%g] ps\n",name,i,dt,min_delta_ps,max_delta_ps > "/dev/stderr"
                bad=1
                continue
            }
            if (!deltas || dt<min_delay) min_delay=dt
            if (!deltas || dt>max_delay) max_delay=dt
            total_delay+=dt
            deltas++
        }
    }
    if (!bad) printf "Original stock %s XSPICE: %d two-channel state observations, %d transitions, relative native receiver loading min=%.3f mean=%.3f max=%.3f ps, direct=%d native=%d samples\n",model_name,events[1,1]+events[1,2],deltas,min_delay,total_delay/deltas,max_delay,samples[1],samples[2]
    exit bad
}
