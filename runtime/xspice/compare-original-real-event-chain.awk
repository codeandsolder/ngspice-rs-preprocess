# Independent original xtraevt real-node oracle. Both source paths execute the
# original d_to_real -> real_delay -> real_to_v C code unmodified.
# Direct stock and ALB source-compiled native X: real-valued (-0.5, +2) events,
# clock-edge sample-and-hold, loaded analog ramps and +200 ps receiver loading.
function abs(x) {return x < 0 ? -x : x}
function problem(msg) {print msg > "/dev/stderr"; failed = 1}
BEGIN {
    count_phases = split("5 12 18 30 38 50 58 70 78 90 98 110 118", times, " ")
    count_raw = split("-0.5 -0.5 2 2 -0.5 -0.5 2 2 -0.5 -0.5 2 2 2", raw_levels, " ")
    count_held = split("0 0 0 2 2 -0.5 -0.5 2 2 -0.5 -0.5 2 2", held_levels, " ")
    if(count_phases != count_raw || count_phases != count_held) {
        problem("bad independently authored real-event state schedule")
        exit 1
    }
    n_edges = 0
    for(edge = 14; edge <= 94; edge += 20) {
        j = ++n_edges
        when[j] = edge
        channel[j] = 0
        rising[j] = (edge % 40 == 14)
        # Original direct ngspice47 C execution, including PWL input,
        # input ADC threshold, d_to_real(350p) and real_to_v(400p).
        original_latency[j] = rising[j] ? .67183 : .69143
    }
    for(edge = 22; edge <= 102; edge += 20) {
        j = ++n_edges
        when[j] = edge
        channel[j] = 1
        rising[j] = (edge % 40 == 22)
        # The first captured event starts from the original real_delay
        # initialization (analog output 0); subsequent steps begin at
        # sampled -0.5 or +2, with measured original C timing.
        original_latency[j] = (edge == 22) ? 1.03491 : 1.12155
    }
    route = 1
}
FNR == 1 && NR > 1 {route = 2}
{
    time = $1
    if(NF != 8 || (FNR > 1 && time <= previous_time[route])) {
        problem(sprintf("real-event waveform invalid row %d at %s", FNR, FILENAME))
        exit 1
    }
    raw = $2
    held = $4
    if(FNR == 1) {
        # Original real_to_v gain is materially observable at DC/INIT.
        # Testing this is critical: a changed gain can still produce
        # nominal event steady-state voltages in this old C implementation.
        if(abs(raw + .5) > .000001 || abs(held) > .000001) {
            problem(sprintf("original real-event initial analog values corrupted: %s raw %.7f held %.7f", FILENAME,raw,held))
            exit 1
        }
        initial[route] = 1
    }
    ns = time * 1e9
    for(i = 1; i <= count_phases; i++) {
        if(abs(ns-times[i]) > .05) continue
        if(!observations[route,i]) {
            if(abs(raw - raw_levels[i]) > .002 || abs(held - held_levels[i]) > .002) {
                problem(sprintf("wrong real event state %s at %.1f ns: raw %.6f expected %.6f, held %.6f expected %.6f", FILENAME, times[i], raw, raw_levels[i], held, held_levels[i]))
                exit 1
            }
            if(abs($6 - (raw_levels[i] > 0 ? 3.3 : 0)) > .001) {
                problem(sprintf("wrong original ALB data stimulus at %.1f ns",times[i]))
                exit 1
            }
            observations[route,i] = 1
            if(abs(raw-raw_levels[i]) > max_err)max_err=abs(raw-raw_levels[i])
            if(abs(held-held_levels[i]) > max_err)max_err=abs(held-held_levels[i])
        }
    }
    for(j = 1; j <= n_edges; j++) {
        if(seen[route,j] || ns < when[j] || ns >= when[j]+4 || FNR <= 1)continue
        value = channel[j] ? held : raw
        old_value = prior[route,channel[j]]
        crossed = rising[j] ? (old_value < .75 && value >= .75) : (old_value > .75 && value <= .75)
        if(crossed) {
            crossings[route,j] = old_time[route]+(time-old_time[route])*(.75-old_value)/(value-old_value)
            seen[route,j] = 1
        }
    }
    previous_time[route] = time
    old_time[route] = time
    prior[route,0] = raw
    prior[route,1] = held
    row_count[route]++
}
END {
    if(failed)exit 1
    if(route != 2 || row_count[1] < 5500 || row_count[2] < 5500 || !initial[1] || !initial[2]) {
        problem("incomplete original stock/native real-event waveform")
    }
    for(side = 1; side <= 2; side++) {
        for(i=1;i<=count_phases;i++) {
            if(!observations[side,i]) {
                problem(sprintf("missing real-event plateau %.1f ns route %d",times[i],side))
            }
        }
    }
    for(j=1;j<=n_edges;j++) {
        if(!seen[1,j] || !seen[2,j]) {
            problem(sprintf("missing original real-event crossing @ %.1f ns channel %d", when[j],channel[j]))
            continue
        }
        actual_original_latency = (crossings[1,j]-when[j]*1e-9)*1e9
        if(abs(actual_original_latency-original_latency[j]) > .065) {
            problem(sprintf("original real-event C absolute transition %.1f ns channel %d: measured %.4f ns, expected %.4f ns", when[j],channel[j],actual_original_latency,original_latency[j]))
        }
        actual_native_latency = (crossings[2,j]-when[j]*1e-9)*1e9
        if(abs(actual_native_latency-original_latency[j]-.2) > .07) {
            problem(sprintf("compiled real-event C absolute transition %.1f ns channel %d: measured %.4f ns, expected %.4f ns", when[j],channel[j],actual_native_latency,original_latency[j]+.2))
        }
        difference = (crossings[2,j]-crossings[1,j])*1e12
        if(abs(difference-200) > 35) {
            problem(sprintf("real-event 2 pF timing changed at %.1f ns channel %d: %.3f ps expected 200 ps", when[j],channel[j],difference))
        }
        if(!min_seen || difference < min_delay)min_delay=difference
        if(!min_seen || difference > max_delay)max_delay=difference
        min_seen=1
        sum_delay+=difference
        checked++
    }
    if(!failed) {
        printf "Original real event d_to_real→real_delay→real_to_v: %d phases x 2 outputs x 2 simulators = %d physical observations, %d transitions, original 2pF loading min %.3f / mean %.3f / max %.3f ps, max plateau error %.5g V, rows %d/%d PASS\n", count_phases,count_phases*4,checked,min_delay,sum_delay/checked,max_delay,max_err,row_count[1],row_count[2]
    }
    exit failed
}
