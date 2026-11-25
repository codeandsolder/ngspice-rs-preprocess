#include <float.h>
#include <math.h>

/*
 * Experimental analog-input family bridge.
 * One physical analog port reflects the resolved downstream digital bundle:
 * its DC current points are (out_low, sink_current) and
 * (out_high, source_current), and its capacitance is TOTAL_LOAD(out).
 * The original Elements 9.40d clamp current is an exponential diode
 * joined continuously to a linear resistor at clamp_bias.
 */

/* Accepted analog state also stores its timestamp. During a same-time
 * EVENT re-entry, T(1) may equal TIME although this accepted state still
 * belongs to the preceding timestep. Preserve the real endpoint slope. */
struct sr_adc_analog_state {
    double voltage;
    double time;
};

/* Track the requested comparator state separately from the actually
 * delivered digital output. All three comparator states participate in
 * inertial supersession, including intermediate UNKNOWN-band requests. */
struct sr_adc_event_state {
    Digital_State_t requested;
    Digital_State_t committed;
    double deadline;
};

static double sr_adc_clamp(double excess, double bias, double resistance,
                           double *derivative)
{
    /* The independently measured 27C and 100C vendor waveforms are
     * byte-identical: the clamp thermal voltage is fixed at nominal 27C,
     * not scaled by operating temperature or the configured knee. */
    // Wide-range signed-bias oracle: 25.864186 mV.
    const double vt = 0.025864186;
    if (excess <= 0.0) {
        *derivative = 0.0;
        return 0.0;
    }
    if (excess < bias) {
        double exponent = exp((excess - bias) / vt);
        *derivative = exponent / resistance;
        /* expm1 avoids subtractive cancellation at clamp onset. Both
         * exponentials are bounded for every supported positive bias. */
        return -(vt / resistance) * exponent * expm1(-excess / vt);
    }
    *derivative = 1.0 / resistance;
    /* This branch includes zero and negative bias. Keep the signed
     * offset instead of silently replacing the junction with an ideal
     * resistor or clipping the extrapolated current. */
    return (excess - bias - vt * expm1(-bias / vt)) / resistance;
}

static Digital_State_t sr_adc_state(double voltage, Digital_State_t previous,
                                     double low, double high, int schmitt)
{
    if (voltage <= low)
        return ZERO;
    if (voltage >= high)
        return ONE;
    if (schmitt)
        return previous == ZERO || previous == ONE ? previous : ZERO;
    return UNKNOWN;
}

/* Request rollback to the interpolated threshold crossing, rather than
 * accepting a delayed state change at a coarse analog timestep endpoint.
 * The previous sample is from ngspice's accepted analog-state vector;
 * tentative iterations cannot overwrite it. A nonlinear input trajectory
 * may require more than one rollback before the crossing is resolved.
 * This relies on ngspice's native minimum-breakpoint spacing. */
static int sr_adc_refine_crossing(double old_v, double new_v,
                                  double old_time, double new_time,
                                  double threshold, double tolerance)
{
    double fraction, crossing;
    if (!((old_v < threshold && new_v > threshold) ||
          (old_v > threshold && new_v < threshold)))
        return 0;

    fraction = (threshold - old_v) / (new_v - old_v);
    crossing = old_time + fraction * (new_time - old_time);
    if (!(crossing > old_time && crossing < new_time) ||
        new_time - crossing <= tolerance)
        return 0;

    /* MIF_OK is zero. If ngspice rejects a breakpoint that is too
       close to the accepted time/minbreak, fall back to normal sampling. */
    return cm_analog_set_temp_bkpt(crossing) == 0;
}

/* The rollback point can land a few ULPs on the pre-crossing side.
 * Evaluate the digital comparator at most time_tol seconds ahead along
 * the last accepted analog slope. This avoids a one-step-late edge on
 * equality without allowing arbitrary state changes off the threshold.
 * Never bias DC, AC or a zero-time transient initial condition. */
static double sr_adc_endpoint_voltage(double value, double previous,
                                       double elapsed, double tolerance)
{
    double fraction;
    /* A large TIME_TOL intentionally disables crossing refinement;
     * it must not make the comparator switch prematurely. */
    if (!(elapsed > 0.0) || !(tolerance > 0.0) || value == previous)
        return value;
    if (tolerance >= elapsed * 0.25) {
        /* Even with refinement disabled for a small step, treat an
         * equality crossing as its outgoing side. A few voltage ULPs
         * correct interpolation roundoff without projecting a large
         * time tolerance onto the threshold. */
        double roundoff = 8.0 * DBL_EPSILON * fmax(fabs(value), fabs(previous));
        return value + copysign(roundoff, value - previous);
    }
    fraction = tolerance / elapsed;
    return value + (value - previous) * fraction;
}

/* Crossing refinement bounds the callback error but does not guarantee
 * that two equal-duration threshold traversals land on equal sample
 * offsets. Recover the comparator request time from the accepted
 * endpoint slope. Never backdate a crossing beyond that analog interval
 * or extrapolate farther than the comparator's allowed projection. */
static double sr_adc_request_time(double value,
                                  const struct sr_adc_analog_state *old,
                                  double now, double tolerance,
                                  double threshold)
{
    double crossing, fraction;
    if (!(now > old->time) || value == old->voltage)
        return now;
    fraction = (threshold - old->voltage) / (value - old->voltage);
    crossing = old->time + fraction * (now - old->time);
    if (crossing < old->time || crossing > now + tolerance)
        return now;
    return crossing;
}

static int sr_adc_before_deadline(double now, double deadline)
{
    /* Compare computed times at the scale of floating-point roundoff,
     * never at the (potentially much larger) threshold time tolerance. */
    double roundoff = 8.0 * DBL_EPSILON * fmax(fabs(now), fabs(deadline));
    return deadline - now > roundoff;
}

void cm_sr_family_adc(ARGS)
{
    struct sr_adc_event_state *previous;
    struct sr_adc_analog_state *voltage_state, *old_voltage;
    double comparator_input;
    double input, current, derivative, extra;
    double low, high, sink, source, conductance, capacitance;
    double resistance = PARAM(clamp_res), bias = PARAM(clamp_bias);
    /* ADC propagation always has the vendor's minimum 1 ps delay.
     * The minimum native threshold-refinement tolerance also applies
     * to accepted signed, zero and fractional-ps model values. */
    double tolerance = fmax(1e-12, PARAM(time_tol));
    Digital_State_t state;
    const Digital_t passive = {ZERO, HI_IMPEDANCE};

    if (INIT) {
        cm_event_alloc(0, sizeof(struct sr_adc_event_state));
        previous = (struct sr_adc_event_state *)cm_event_get_ptr(0, 0);
        previous->requested = (Digital_State_t)(UNKNOWN + 1);
        previous->committed = previous->requested;
        previous->deadline = -1.0;
        cm_analog_alloc(0, sizeof(struct sr_adc_analog_state));
        voltage_state = (struct sr_adc_analog_state *)cm_analog_get_ptr(0, 0);
        voltage_state->voltage = 0.0;
        voltage_state->time = TIME;
        return;
    }
    previous = (struct sr_adc_event_state *)cm_event_get_ptr(0, 0);

    if (!isfinite(resistance) || resistance == 0.0 || !isfinite(bias) ||
        !isfinite(PARAM(rise_delay)) || !isfinite(PARAM(fall_delay)) ||
        !isfinite(PARAM(time_tol))) {
        cm_message_send("ERROR: sr_family_adc needs finite timing and clamp parameters and nonzero clamp resistance");
        return;
    }

    if (CALL_TYPE == EVENT) {
        comparator_input = INPUT(analog);
        if (ANALYSIS == TRANSIENT) {
            old_voltage = (struct sr_adc_analog_state *)cm_analog_get_ptr(0, 1);
            comparator_input = sr_adc_endpoint_voltage(
                comparator_input, old_voltage->voltage,
                TIME - old_voltage->time, tolerance);
        }
        state = sr_adc_state(comparator_input, previous->requested,
                             PARAM(in_low), PARAM(in_high),
                             PARAM(schmitt));
        OUTPUT_STATE(sink_meta) = passive.state;
        OUTPUT_STRENGTH(sink_meta) = passive.strength;
        OUTPUT_STATE(source_meta) = passive.state;
        OUTPUT_STRENGTH(source_meta) = passive.strength;

        if (state == previous->requested) {
            OUTPUT_CHANGED(out) = FALSE;
            return;
        }
        OUTPUT_STATE(out) = state;
        OUTPUT_STRENGTH(out) = STRONG;
        if (ANALYSIS == DC) {
            OUTPUT_DELAY(out) = 0.0;
            previous->committed = state;
            previous->deadline = -1.0;
        } else {
            double delay, deadline, when, threshold;
            int pending;
            old_voltage = (struct sr_adc_analog_state *)cm_analog_get_ptr(0, 1);
            threshold = state == ONE ||
                (state == UNKNOWN && previous->requested == ONE) ?
                PARAM(in_high) : PARAM(in_low);
            when = sr_adc_request_time(INPUT(analog), old_voltage, TIME,
                                       tolerance, threshold);
            pending = sr_adc_before_deadline(when, previous->deadline) &&
                sr_adc_before_deadline(TIME, previous->deadline);
            if (!pending)
                previous->committed = previous->requested;
            delay = state == ONE ||
                    (state == UNKNOWN && previous->requested == ZERO) ?
                    PARAM(rise_delay) : PARAM(fall_delay);
            deadline = when + fmax(1e-12, delay);
            /* A mixed analog/event model cannot reliably deliver a
             * transient output queued at the current event time. If
             * sampling was too late, retain the interface's minimum
             * positive 1 ps delay rather than posting a zero delay. */
            delay = fmax(1e-12, deadline - TIME);
            if (pending) {
                /* An earlier output-queue insertion supersedes all
                 * later events. Cancel the old deadline before posting
                 * its replacement, without creating a visible glitch. */
                double cancel = fmin(previous->deadline - TIME, delay) * 0.5;
                if (state == previous->committed) {
                    delay = cancel;
                    previous->deadline = -1.0;
                } else {
                    Digital_t revert = {previous->committed, STRONG};
                    cm_schedule_output(1, 0, cancel, &revert);
                    previous->deadline = fmax(deadline, TIME + delay);
                }
            } else {
                previous->deadline = fmax(deadline, TIME + delay);
            }
            OUTPUT_DELAY(out) = delay;
        }
        previous->requested = state;
        return;
    }
    if (CALL_TYPE != ANALOG)
        return;

    input = INPUT(analog);
    low = PARAM(out_low);
    high = PARAM(out_high);
    if (high == low) {
        cm_message_send("ERROR: sr_family_adc needs distinct output load levels");
        return;
    }

    /* Resolve capacitance and current across the *actual* digital loads,
       including all real subcircuits on the physical bundle. */
    capacitance = TOTAL_LOAD(out);
    sink = TOTAL_LOAD(sink_meta);
    source = TOTAL_LOAD(source_meta);
    conductance = (source - sink) / (high - low);
    current = sink + (input - low) * conductance;
    derivative = conductance;

    /* Vendor reversed/equal clamp bounds are valid: the upper clamp
     * has priority when both arms could conduct. Never sum two diode
     * currents in the inverted interval. Ordered clamps retain the
     * original independent branches as their domains cannot overlap. */
    /* adc_schmitt ignores CLAMP_LOW and CLAMP_HIGH model parameters.
     * Only the analog bridge honors explicit clamp rails. */
    double clamp_low = PARAM(schmitt) ? low :
        (PARAM_NULL(clamp_low) ? low : PARAM(clamp_low));
    double clamp_high = PARAM(schmitt) ? high :
        (PARAM_NULL(clamp_high) ? high : PARAM(clamp_high));
    if (input > clamp_high) {
        current += sr_adc_clamp(
            input - clamp_high, bias, resistance, &extra);
        derivative += extra;
    } else if (input < clamp_low) {
        current -= sr_adc_clamp(
            clamp_low - input, bias, resistance, &extra);
        derivative += extra;
    }

    if (!isfinite(current) || !isfinite(derivative)) {
        cm_message_send("ERROR: sr_family_adc clamp current or derivative overflow");
        return;
    }

    voltage_state = (struct sr_adc_analog_state *)cm_analog_get_ptr(0, 0);
    old_voltage = (struct sr_adc_analog_state *)cm_analog_get_ptr(0, 1);
    if (ANALYSIS == TRANSIENT) {
        double dt = TIME - old_voltage->time;
        if (dt > 0.0) {
            current += capacitance * (input - old_voltage->voltage) / dt;
            derivative += capacitance / dt;
        }
        voltage_state->voltage = input;
        voltage_state->time = TIME;
    } else if (ANALYSIS == DC) {
        voltage_state->voltage = input;
        voltage_state->time = TIME;
    }

    if (ANALYSIS == AC) {
        Complex_t gain;
        gain.real = derivative;
        gain.imag = RAD_FREQ * capacitance;
        AC_GAIN(analog, analog) = gain;
    } else {
        OUTPUT(analog) = current;
        PARTIAL(analog, analog) = derivative;
    }

    /* Refine only actual threshold crossings, with no effect on DC/AC.
     * Schmitt uses the active edge: low->high at IN_HIGH and high->low at
     * IN_LOW. A normal ADC must additionally locate UNKNOWN-band edges. */
    if (ANALYSIS == TRANSIENT && TIME > old_voltage->time) {
        double t1 = old_voltage->time;
        int schmitt = PARAM(schmitt);
        double threshold1 = PARAM(in_low);
        double threshold2 = PARAM(in_high);

        if (schmitt && previous->requested == ZERO)
            threshold1 = threshold2;
        if (schmitt && previous->requested == ONE)
            threshold2 = threshold1;
        if (input < old_voltage->voltage) {
            double swap = threshold1;
            threshold1 = threshold2;
            threshold2 = swap;
        }
        if (sr_adc_refine_crossing(old_voltage->voltage, input, t1, TIME,
                                    threshold1, tolerance) ||
            (threshold2 != threshold1 &&
             sr_adc_refine_crossing(old_voltage->voltage, input, t1, TIME,
                                    threshold2, tolerance)))
            return;
    }

    comparator_input = input;
    if (ANALYSIS == TRANSIENT)
        comparator_input = sr_adc_endpoint_voltage(
            input, old_voltage->voltage, TIME - old_voltage->time,
            tolerance);
    state = sr_adc_state(comparator_input, previous->requested,
                         PARAM(in_low), PARAM(in_high),
                         PARAM(schmitt));
    if (state != previous->requested)
        cm_event_queue(TIME);
}
