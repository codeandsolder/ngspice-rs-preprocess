# Validate every measured 2.5 V analog crossing against real SIMetrix.
# Vendor reference in CSV; ngspice wrdata time/value pairs in the other file.
function pin_index(label, suffix) {
    if (label == "q") return 1
    if (label !~ /^q[0-9]+$/) return -1
    suffix = substr(label, 2)
    return (suffix + 0) + 1
}
BEGIN {
    FS = "[,[:space:]]+"
    tolerance_ns = 0.12
    failed = 0
}
{
    sub(/^[[:space:]]+/, "", $0)
}
FILENAME == ARGV[1] {
    if (FNR == 1) {
        if ($1 != "scenario" || $2 != "pin" || $3 != "direction") {
            print "Unexpected stateful vendor CSV header" > "/dev/stderr"
            exit 2
        }
        next
    }
    if ($1 != name) next
    pin = pin_index($2)
    if (pin < 1 || pin > pins || ($3 != "rise" && $3 != "fall")) {
        printf "%s: invalid vendor pin or direction\n", name > "/dev/stderr"
        exit 2
    }
    expected_total++
    event = ++expected_count[pin]
    expected_time[pin, event] = $4 + 0
    expected_direction[pin, event] = $3
    next
}
FILENAME == ARGV[2] {
    if (FNR == 1) {
        if ($1 != "scenario" || $2 != "pin" || $3 != "initial_v") {
            print "Unexpected vendor initial-voltage CSV header" > "/dev/stderr"
            exit 2
        }
        next
    }
    if ($1 != name) next
    pin = pin_index($2)
    if (pin < 1 || pin > pins || initial_seen[pin]++) {
        printf "%s: invalid or duplicated initial pin %s\n", name, $2 > "/dev/stderr"
        exit 2
    }
    initial_voltage[pin] = $3 + 0
    initial_count++
    next
}
FILENAME == ARGV[3] {
    if (NF < 2 * pins) {
        print "Malformed native stateful trace" > "/dev/stderr"
        exit 2
    }
    if (FNR == 1) {
        for (pin = 1; pin <= pins; pin++) {
            measured_voltage = $(2 * pin) + 0
            voltage_difference = measured_voltage - initial_voltage[pin]
            printf "%s q%d initial: vendor %.6f V, native %.6f V, delta %+.6f V\n", \
                name, pin - 1, initial_voltage[pin], measured_voltage, voltage_difference
            if (!initial_seen[pin] || voltage_difference < -0.75 || voltage_difference > 0.75) {
                failed = 1
            }
        }
    }
    for (pin = 1; pin <= pins; pin++) {
        time_s = $(2 * pin - 1) + 0
        value = $(2 * pin) + 0
        if (has_previous[pin]) {
            direction = ""
            if (previous_value[pin] < 2.5 && value >= 2.5) direction = "rise"
            if (previous_value[pin] >= 2.5 && value < 2.5) direction = "fall"
            if (direction != "") {
                event = ++actual_count[pin]
                measured_ns = (previous_time[pin] + (time_s - previous_time[pin]) \
                    * (2.5 - previous_value[pin]) / (value - previous_value[pin])) * 1e9
                if (event > expected_count[pin]) {
                    printf "%s q%d: extra %s event at %.9f ns\n", \
                        name, pin - 1, direction, measured_ns > "/dev/stderr"
                    failed = 1
                } else {
                    difference_ns = measured_ns - expected_time[pin, event]
                    printf "%s q%d #%d %s: vendor %.9f ns, native %.9f ns, delta %+.3f ps\n", \
                        name, pin - 1, event, direction, expected_time[pin, event], \
                        measured_ns, difference_ns * 1e3
                    if (direction != expected_direction[pin, event] \
                        || difference_ns < -tolerance_ns || difference_ns > tolerance_ns) {
                        failed = 1
                    }
                }
            }
        }
        previous_time[pin] = time_s
        previous_value[pin] = value
        has_previous[pin] = 1
    }
    next
}
END {
    if (initial_count != pins) {
        printf "%s: expected %d initial voltage references, loaded %d\n", \
            name, pins, initial_count > "/dev/stderr"
        failed = 1
    }
    for (pin = 1; pin <= pins; pin++) {
        if (actual_count[pin] != expected_count[pin]) {
            printf "%s q%d: expected %d events, native emitted %d\n", \
                name, pin - 1, expected_count[pin], actual_count[pin] > "/dev/stderr"
            failed = 1
        }
    }
    if (failed) exit 1
}
