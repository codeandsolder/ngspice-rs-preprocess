#include <math.h>

/*
 * Experimental analog-input family bridge.
 * One physical analog port reflects the resolved downstream digital bundle:
 * its DC current points are (out_low, sink_current) and
 * (out_high, source_current), and its capacitance is TOTAL_LOAD(out).
 * The diode-to-resistor clamp curve is a C1 approximation, not a claim
 * to duplicate the proprietary SIMetrix clamp interpolation.
 */

/* Accepted analog state also stores its timestamp. During a same-time
 * EVENT re-entry, T(1) may equal TIME although this accepted state still
 * belongs to the preceding timestep. Preserve the real endpoint slope. */
struct sr_adc_analog_state {
    double voltage;
    double time;
};

static double sr_adc_clamp(double excess, double bias, double resistance,
                           double *derivative)
{
    double vt = bias / 10.0;
    double baseline = exp(-bias / vt);
    double scale = resistance * (1.0 - baseline);
    if (excess <= 0.0) {
        *derivative = 0.0;
        return 0.0;
    }
    if (excess < bias) {
        double exponent = exp((excess - bias) / vt);
        *derivative = (exponent - baseline) / scale;
        return (vt * (exponent - baseline) - baseline * excess) / scale;
    }
    *derivative = 1.0 / resistance;
    return (excess - bias) / resistance +
           (vt * (1.0 - baseline) - baseline * bias) / scale;
}

static Digital_State_t sr_adc_state(double voltage, Digital_State_t previous,
                                     double low, double high, int schmitt,
                                     Digital_State_t ic)
{
    if (voltage <= low)
        return ZERO;
    if (voltage >= high)
        return ONE;
    if (schmitt)
        return previous == ZERO || previous == ONE ? previous : ic;
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
    if (!(elapsed > 0.0) || !(tolerance > 0.0) ||
        tolerance >= elapsed * 0.25)
        return value;
    fraction = tolerance / elapsed;
    return value + (value - previous) * fraction;
}

void cm_sr_family_adc(ARGS)
{
    Digital_t *previous;
    struct sr_adc_analog_state *voltage_state, *old_voltage;
    double comparator_input;
    double input, current, derivative, extra;
    double low, high, sink, source, conductance, capacitance;
    double resistance = PARAM(clamp_res), bias = PARAM(clamp_bias);
    Digital_State_t state;
    const Digital_t passive = {ZERO, HI_IMPEDANCE};

    if (INIT) {
        cm_event_alloc(0, sizeof(Digital_t));
        previous = (Digital_t *)cm_event_get_ptr(0, 0);
        previous->state = (Digital_State_t)(UNKNOWN + 1);
        previous->strength = UNDETERMINED;
        cm_analog_alloc(0, sizeof(struct sr_adc_analog_state));
        voltage_state = (struct sr_adc_analog_state *)cm_analog_get_ptr(0, 0);
        voltage_state->voltage = 0.0;
        voltage_state->time = TIME;
        return;
    }
    previous = (Digital_t *)cm_event_get_ptr(0, 0);

    if (CALL_TYPE == EVENT) {
        comparator_input = INPUT(analog);
        if (ANALYSIS == TRANSIENT) {
            old_voltage = (struct sr_adc_analog_state *)cm_analog_get_ptr(0, 1);
            comparator_input = sr_adc_endpoint_voltage(
                comparator_input, old_voltage->voltage,
                TIME - old_voltage->time, PARAM(time_tol));
        }
        state = sr_adc_state(comparator_input, previous->state,
                             PARAM(in_low), PARAM(in_high),
                             PARAM(schmitt), (Digital_State_t)PARAM(ic));
        OUTPUT_STATE(sink_meta) = passive.state;
        OUTPUT_STRENGTH(sink_meta) = passive.strength;
        OUTPUT_STATE(source_meta) = passive.state;
        OUTPUT_STRENGTH(source_meta) = passive.strength;

        if (state == previous->state && previous->strength == STRONG) {
            OUTPUT_CHANGED(out) = FALSE;
            return;
        }
        OUTPUT_STATE(out) = state;
        OUTPUT_STRENGTH(out) = STRONG;
        if (ANALYSIS == DC) {
            OUTPUT_DELAY(out) = 0.0;
        } else if (state == ONE) {
            OUTPUT_DELAY(out) = PARAM(rise_delay);
        } else if (state == ZERO) {
            OUTPUT_DELAY(out) = PARAM(fall_delay);
        } else {
            OUTPUT_DELAY(out) = previous->state == ZERO ?
                PARAM(rise_delay) : PARAM(fall_delay);
        }
        previous->state = state;
        previous->strength = STRONG;
        return;
    }
    if (CALL_TYPE != ANALOG)
        return;

    input = INPUT(analog);
    low = PARAM(out_low);
    high = PARAM(out_high);
    if (!(high > low) || !(PARAM(in_high) > PARAM(in_low)) ||
        !((PARAM_NULL(clamp_high) ? high : PARAM(clamp_high)) >
          (PARAM_NULL(clamp_low) ? low : PARAM(clamp_low)))) {
        cm_message_send("ERROR: sr_family_adc needs increasing output, input, and clamp rails");
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

    /* Smooth reverse clamps below the low rail and above the high rail. */
    if (input < (PARAM_NULL(clamp_low) ? low : PARAM(clamp_low))) {
        current -= sr_adc_clamp(
            (PARAM_NULL(clamp_low) ? low : PARAM(clamp_low)) - input,
            bias, resistance, &extra);
        derivative += extra;
    }
    if (input > (PARAM_NULL(clamp_high) ? high : PARAM(clamp_high))) {
        current += sr_adc_clamp(
            input - (PARAM_NULL(clamp_high) ? high : PARAM(clamp_high)),
            bias, resistance, &extra);
        derivative += extra;
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
        double t1 = old_voltage->time, tol = PARAM(time_tol);
        int schmitt = PARAM(schmitt);
        double threshold1 = PARAM(in_low);
        double threshold2 = PARAM(in_high);

        if (schmitt && previous->state == ZERO)
            threshold1 = threshold2;
        if (schmitt && previous->state == ONE)
            threshold2 = threshold1;
        if (input < old_voltage->voltage) {
            double swap = threshold1;
            threshold1 = threshold2;
            threshold2 = swap;
        }
        if (sr_adc_refine_crossing(old_voltage->voltage, input, t1, TIME,
                                    threshold1, tol) ||
            (threshold2 != threshold1 &&
             sr_adc_refine_crossing(old_voltage->voltage, input, t1, TIME,
                                    threshold2, tol)))
            return;
    }

    comparator_input = input;
    if (ANALYSIS == TRANSIENT)
        comparator_input = sr_adc_endpoint_voltage(
            input, old_voltage->voltage, TIME - old_voltage->time,
            PARAM(time_tol));
    state = sr_adc_state(comparator_input, previous->state,
                         PARAM(in_low), PARAM(in_high),
                         PARAM(schmitt), (Digital_State_t)PARAM(ic));
    if (state != previous->state || previous->strength != STRONG)
        cm_event_queue(TIME);
}
