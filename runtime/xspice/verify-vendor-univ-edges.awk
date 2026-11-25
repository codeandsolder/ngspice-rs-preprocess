# Compare native ngspice source-only ALB transitions with a real SIMetrix
# Elements 9.40d oracle after matching its external switch/time-step semantics.
# The oracle CSV is the sole source of absolute expected edge times.
BEGIN { tolerance_ns = 0.10; threshold_v = 2.5 }
FNR == NR {
    if ($0 ~ /^#/ || $0 ~ /^shunt_ohm/) next;
    split($0, columns, ",");
    if (columns[1] == shunt) {
        if (have_reference++) { print "duplicate vendor reference" > "/dev/stderr"; exit 1 }
        ref["pin_rise"] = columns[2] + 0;
        ref["pin_fall"] = columns[3] + 0;
        ref["observe_rise"] = columns[4] + 0;
        ref["observe_fall"] = columns[5] + 0;
    }
    next;
}
{
    t = $1 * 1e9;
    pin = $2;
    observe = $8;
    if (FNR > 1) {
        if (prev_t >= 30 && t <= 38 && prev_pin < threshold_v && pin >= threshold_v)
            actual["pin_rise"] = prev_t + (t-prev_t) * (threshold_v-prev_pin) / (pin-prev_pin);
        if (prev_t >= 88 && t <= 101 && prev_pin > threshold_v && pin <= threshold_v)
            actual["pin_fall"] = prev_t + (t-prev_t) * (threshold_v-prev_pin) / (pin-prev_pin);
        if (prev_t >= 30 && t <= 38 && prev_observe < threshold_v && observe >= threshold_v)
            actual["observe_rise"] = prev_t + (t-prev_t) * (threshold_v-prev_observe) / (observe-prev_observe);
        if (prev_t >= 88 && t <= 101 && prev_observe > threshold_v && observe <= threshold_v)
            actual["observe_fall"] = prev_t + (t-prev_t) * (threshold_v-prev_observe) / (observe-prev_observe);
    }
    prev_t = t;
    prev_pin = pin;
    prev_observe = observe;
}
END {
    if (!have_reference) {
        printf "no SIMetrix timing reference for %s ohms\n", shunt > "/dev/stderr";
        exit 1;
    }
    failed = 0;
    for (name in ref) {
        if (!(name in actual)) {
            printf "%s: missing %s crossing\n", shunt, name > "/dev/stderr";
            failed = 1;
            continue;
        }
        delta = actual[name] - ref[name];
        if (delta < -tolerance_ns || delta > tolerance_ns) {
            printf "%s ohms %s native %.6f ns vendor %.6f ns delta %.3f ns exceeds %.3f ns\n", shunt, name, actual[name], ref[name], delta, tolerance_ns > "/dev/stderr";
            failed = 1;
        }
    }
    if (failed) exit 1;
    printf "%s ohms: four vendor-referenced PIN/OBSERVE 2.5V transition times within %.0f ps\n", shunt, tolerance_ns * 1000;
}
