#include <math.h>
#include <string.h>

/*
 * Experimental nonlinear digital-to-analog bridge.
 * The five physical drive curves are evaluated directly as an analog
 * current with a Jacobian. Independent weights preserve interrupted
 * transitions without losing earlier current/strength characteristics.
 *
 * Does not implement SIMetrix family negotiation, supply scaling, or
 * automatic analog-to-digital clamp/load insertion.
 */

enum { SR_STRONG_LOW, SR_STRONG_HIGH, SR_WEAK_LOW, SR_WEAK_HIGH, SR_HIZ, SR_DRIVES };

struct sr_family_iv {
    double current;
    double slope;
};

struct sr_family_transition {
    double from[SR_DRIVES];
    double to[SR_DRIVES];
    double started;
    double duration;
    Digital_t last;
};

/* Ohmic near origin, quadratic C1 knee, and current limited beyond knee. */
static struct sr_family_iv sr_knee(double distance, double conductance,
                                   double knee, double smooth)
{
    struct sr_family_iv iv;
    double start = knee - smooth;
    double end = knee + smooth;
    if (distance <= start) {
        iv.current = distance * conductance;
        iv.slope = conductance;
    } else if (distance >= end) {
        iv.current = knee * conductance;
        iv.slope = 0.0;
    } else {
        double dx = distance - start;
        iv.current = conductance * (distance - dx * dx / (4.0 * smooth));
        iv.slope = conductance * (1.0 - dx / (2.0 * smooth));
    }
    return iv;
}

static void sr_basis(Digital_t state, double weights[SR_DRIVES])
{
    memset(weights, 0, sizeof(double) * SR_DRIVES);
    if (state.strength == HI_IMPEDANCE) {
        weights[SR_HIZ] = 1.0;
    } else if (state.state == UNKNOWN) {
        weights[SR_STRONG_LOW] = 0.5;
        weights[SR_STRONG_HIGH] = 0.5;
    } else if (state.strength == RESISTIVE) {
        weights[state.state == ONE ? SR_WEAK_HIGH : SR_WEAK_LOW] = 1.0;
    } else {
        weights[state.state == ONE ? SR_STRONG_HIGH : SR_STRONG_LOW] = 1.0;
    }
}

static double sr_fraction(const struct sr_family_transition *tr, double now)
{
    double fraction;
    if (tr->duration <= 0.0)
        return 1.0;
    fraction = (now - tr->started) / tr->duration;
    if (fraction <= 0.0)
        return 0.0;
    if (fraction >= 1.0)
        return 1.0;
    return fraction;
}

static void sr_weights(const struct sr_family_transition *tr, double now,
                       double weights[SR_DRIVES])
{
    double fraction = sr_fraction(tr, now);
    int i;
    for (i = 0; i < SR_DRIVES; ++i)
        weights[i] = tr->from[i] + (tr->to[i] - tr->from[i]) * fraction;
}

static double sr_nominal(const double weights[SR_DRIVES], double low, double high)
{
    double middle = 0.5 * (low + high);
    return low * (weights[SR_STRONG_LOW] + weights[SR_WEAK_LOW])
        + high * (weights[SR_STRONG_HIGH] + weights[SR_WEAK_HIGH])
        + middle * weights[SR_HIZ];
}

void cm_sr_family_dac(ARGS)
{
    struct sr_family_transition *tr;
    double weights[SR_DRIVES];
    double low = PARAM(out_low);
    double high = PARAM(out_high);
    double voltage;
    double currents[SR_DRIVES], slopes[SR_DRIVES];
    double current = 0.0, derivative = 0.0;
    struct sr_family_iv iv;
    int i;

    if (INIT) {
        Digital_t initial;
        cm_event_alloc(0, sizeof(struct sr_family_transition));
        tr = (struct sr_family_transition *)cm_event_get_ptr(0, 0);
        initial.state = INPUT_STATE(in);
        initial.strength = INPUT_STRENGTH(in);
        sr_basis(initial, tr->to);
        memcpy(tr->from, tr->to, sizeof(tr->from));
        tr->started = TIME;
        tr->duration = 0.0;
        tr->last = initial;
        LOAD(in) = PARAM(input_load);
        LOAD(sink_meta) = PARAM(sink_current);
        LOAD(source_meta) = PARAM(source_current);
        return;
    }

    tr = (struct sr_family_transition *)cm_event_get_ptr(0, 0);

    if (CALL_TYPE == EVENT) {
        Digital_t next;
        double old_voltage, next_voltage, duration;
        next.state = INPUT_STATE(in);
        next.strength = INPUT_STRENGTH(in);
        if (next.state == tr->last.state && next.strength == tr->last.strength)
            return;
        sr_weights(tr, TIME, weights);
        old_voltage = sr_nominal(weights, low, high);
        sr_basis(next, tr->to);
        next_voltage = sr_nominal(tr->to, low, high);
        memcpy(tr->from, weights, sizeof(tr->from));
        duration = next_voltage >= old_voltage ? PARAM(t_rise) : PARAM(t_fall);
        duration *= fabs(next_voltage - old_voltage) /
                    (fabs(high - low) > 0.0 ? fabs(high - low) : 1.0);
        if (duration < 1e-12 && next_voltage != old_voltage)
            duration = 1e-12;
        tr->started = TIME;
        tr->duration = duration;
        tr->last = next;
        cm_analog_set_perm_bkpt(TIME);
        if (duration > 0.0)
            cm_analog_set_perm_bkpt(TIME + duration);
        return;
    }

    if (CALL_TYPE != ANALOG)
        return;

    /* During DC analysis, digital initialization may be completed after
       the original INIT invocation, so always use the live digital state. */
    if (ANALYSIS == DC) {
        Digital_t dc;
        dc.state = INPUT_STATE(in);
        dc.strength = INPUT_STRENGTH(in);
        sr_basis(dc, weights);
    } else {
        sr_weights(tr, TIME, weights);
    }

    voltage = INPUT(out);
    iv = sr_knee(voltage - low, PARAM(g_pulldown),
                 PARAM(knee_low) - low, PARAM(v_smooth));
    currents[SR_STRONG_LOW] = iv.current;
    slopes[SR_STRONG_LOW] = iv.slope;

    iv = sr_knee(high - voltage, PARAM(g_pullup),
                 high - PARAM(knee_high), PARAM(v_smooth));
    currents[SR_STRONG_HIGH] = -iv.current;
    slopes[SR_STRONG_HIGH] = iv.slope;

    currents[SR_WEAK_LOW] = (voltage - low) * PARAM(g_resistive);
    currents[SR_WEAK_HIGH] = (voltage - high) * PARAM(g_resistive);
    slopes[SR_WEAK_LOW] = slopes[SR_WEAK_HIGH] = PARAM(g_resistive);

    currents[SR_HIZ] = (voltage - 0.5 * (low + high)) * PARAM(g_hiz);
    slopes[SR_HIZ] = PARAM(g_hiz);

    for (i = 0; i < SR_DRIVES; ++i) {
        current += weights[i] * currents[i];
        derivative += weights[i] * slopes[i];
    }
    if (ANALYSIS == AC) {
        Complex_t gain;
        gain.real = derivative;
        gain.imag = 0.0;
        AC_GAIN(out, out) = gain;
    } else {
        OUTPUT(out) = current;
        PARTIAL(out, out) = derivative;
    }
}
