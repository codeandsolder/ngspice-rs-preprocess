# Compare independently captured vendor analog reference against the native
# simulated waveform by linear interpolation on the *native* timebase.
# Invocation:
# awk -v reference_column=2 -v native_column=2 -v abs_tol=.01 -v rms_tol=.002 \
#     -v value_unit=V \
#     -f compare-vendor-waveform.awk reference.dat native-wrdata.dat
# Both inputs: whitespace numeric; reference columns start with time;
# native is either ngspice wrdata's repeated (time, value) pairs or
# a compact time-first multichannel trace. Selected columns may hold
# physical voltages or signed terminal currents.
function absolute(value) { return value < 0 ? -value : value }
function compare(sample, time, value, difference) {
    difference = absolute(sample - value)
    if (difference > worst) {
        worst = difference
        worst_time = time
    }
    squared += difference * difference
    compared++
}
BEGIN {
    if (native_column == 0) native_column = 2
    if (reference_column == 0) reference_column = 2
    if (value_unit == "") value_unit = "V"
    if (min_samples == 0) min_samples = 1000
    if (min_samples < 2 || reference_column < 2 || (value_unit != "V" && value_unit != "A") ||
        native_column < 2 ||
        abs_tol <= 0 || rms_tol <= 0) {
        print "invalid waveform comparator parameters" > "/dev/stderr"
        exit 2
    }
}
FNR == NR {
    if (NF < reference_column || (reference_count && $1 <= reference_time[reference_count])) {
        print "missing or nonmonotonic vendor reference sample" > "/dev/stderr"
        failed = 1
        exit 2
    }
    reference_time[++reference_count] = $1
    reference_value[reference_count] = $(reference_column)
    next
}
{
    if (NF < native_column) {
        print "missing native voltage column" > "/dev/stderr"
        failed = 1
        exit 2
    }
    time = $1
    voltage = $(native_column)
    if (FNR == 1) {
        previous_time = time
        previous_voltage = voltage
        if (reference_time[1] < time) {
            print "native waveform starts after vendor reference" > "/dev/stderr"
            failed = 1
            exit 2
        }
    } else if (time < previous_time) {
        print "nonmonotonic native waveform" > "/dev/stderr"
        failed = 1
        exit 2
    }
    while (current + 1 <= reference_count &&
           reference_time[current+1] <= time) {
        current++
        sample_time = reference_time[current]
        if (time == previous_time) {
            interpolated = voltage
        } else {
            interpolated = previous_voltage + (voltage - previous_voltage) * (sample_time - previous_time) / (time - previous_time)
        }
        compare(reference_value[current], sample_time, interpolated)
    }
    previous_time = time
    previous_voltage = voltage
}
END {
    if (failed) exit 2
    if (reference_count < min_samples || compared != reference_count) {
        printf "incomplete vendor/native waveform coverage: %d/%d\n",
            compared, reference_count > "/dev/stderr"
        exit 1
    }
    rms = sqrt(squared / compared)
    printf "Vendor/native waveform: %d samples, max |d%s|=%.9g at %.9g s, RMS=%.9g %s\n",
        compared, value_unit == "A" ? "I" : "V", worst, worst_time, rms, value_unit
    if (worst > abs_tol || rms > rms_tol) exit 1
}
