#include <math.h>

/*
 * Experimental nonlinear digital-to-analog bridge.
 * The physical drive curves are evaluated directly as an analog
 * current with a Jacobian. Constant-rate logic and drive-strength ramps
 * evolve independently, including when an event interrupts only one.
 *
 * Does not implement SIMetrix family negotiation, supply scaling, or
 * automatic analog-to-digital clamp/load insertion.
 */

enum {
    SR_STRONG_LOW, SR_STRONG_HIGH, SR_WEAK_LOW, SR_WEAK_HIGH,
    SR_DRIVES
};

/* A Norton source injects source - conductance * voltage. Represent
 * the intercept directly: subtracting two ohmic currents would turn
 * exactly-zero source terms into rounding-dependent nonzero terms. */
struct sr_family_norton {
    double source;
    double conductance;
};

/* The logic, strong and resistive coordinates have independent slopes.
 * Negative slew times extrapolate rather than saturate. */
enum { SR_LOGIC, SR_WEAK, SR_STRONG, SR_AXES };

struct sr_ramp {
    double from;
    double to;
    double started;
    double duration;
    double rate;
};

struct sr_family_transition {
    struct sr_ramp axis[SR_AXES];
    Digital_t last;
};

/* Linearization of the signed ohmic, quadratic-knee and saturated
 * current law. Direction is -1 for sinking, +1 for sourcing. */
static struct sr_family_norton sr_knee(double voltage, double rail,
                                      double conductance, double knee,
                                      double smooth, double direction)
{
    double distance = direction * (rail - voltage);
    double start = knee - smooth;
    if (distance <= start)
        return (struct sr_family_norton){conductance * rail, conductance};
    if (distance >= knee + smooth)
        return (struct sr_family_norton){direction * conductance * knee, 0.0};

    double dx = distance - start;
    double fraction = dx / (2.0 * smooth);
    double slope = conductance * (1.0 - fraction);
    return (struct sr_family_norton){
        slope * rail + direction * conductance * fraction * (distance + start) * 0.5,
        slope
    };
}

/* High-Z contributes no source or conductance to the active-state
 * interpolation. The leakage conductance floor is applied afterward,
 * preserving any nonzero active Norton intercept. Exactly-zero
 * intercepts instead blend toward a midpoint voltage
 * over the final decade of conductance above the leakage floor.
 * Even arbitrarily small nonzero intercepts bypass that blend. */
static struct sr_family_norton sr_leakage_floor(struct sr_family_norton raw,
                                               double leakage, double midpoint)
{
    double effective = fmax(raw.conductance, leakage);
    if (raw.source == 0.0 && leakage > 0.0) {
        double blend = raw.conductance <= leakage ? 1.0 :
            fmax(0.0, 1.0 + log10(leakage / raw.conductance));
        raw.source = midpoint * effective * blend;
    }
    raw.conductance = effective;
    return raw;
}

static void sr_target(Digital_t state, double target[SR_AXES])
{
    target[SR_LOGIC] = state.state == ZERO ? 0.0 :
                       state.state == ONE ? 1.0 :
                       0.5;
    target[SR_WEAK] = state.strength == RESISTIVE ? 1.0 : 0.0;
    target[SR_STRONG] = state.strength == STRONG ? 1.0 : 0.0;
}

static double sr_ramp_value(const struct sr_ramp *ramp, double now)
{
    double fraction;
    if (ramp->duration == 0.0)
        return ramp->to;
    if (now <= ramp->started)
        return ramp->from;
    fraction = (now - ramp->started) / ramp->duration;
    if (fraction >= 1.0)
        return ramp->to;
    return ramp->from + (ramp->to - ramp->from) * fraction;
}

static void sr_settle(struct sr_family_transition *tr, Digital_t state, double now)
{
    double target[SR_AXES];
    int i;
    sr_target(state, target);
    for (i = 0; i < SR_AXES; ++i)
        tr->axis[i] = (struct sr_ramp){target[i], target[i], now, 0.0, 0.0};
    tr->last = state;
}

static void sr_retarget(struct sr_ramp *ramp, double target, double now,
                        double previous, double rate)
{
    double delta;
    if (target == ramp->to && rate == ramp->rate)
        return;
    /* A request for an already overshot binary endpoint snaps there,
     * but intermediate logic targets can continue signed extrapolation. */
    if ((target == 0.0 && previous <= 0.0) ||
        (target == 1.0 && previous >= 1.0))
        previous = target;
    delta = target - previous;
    ramp->from = previous;
    ramp->to = target;
    ramp->started = now;
    ramp->rate = rate;
    ramp->duration = fabs(delta) * rate;
    if (ramp->duration > 0.0 && now + ramp->duration > now)
        cm_analog_set_perm_bkpt(now + ramp->duration);
}

static void sr_weights(const struct sr_family_transition *tr, double now,
                       double weights[SR_DRIVES])
{
    double high = sr_ramp_value(&tr->axis[SR_LOGIC], now);
    double strong = sr_ramp_value(&tr->axis[SR_STRONG], now);
    double weak = sr_ramp_value(&tr->axis[SR_WEAK], now);

    /* Measured Elements currents are products of logic and drive
     * strength, not weights linearly interpolated between digital modes.
     * Opposite-polarity enable/release is naturally bilinear; interruption
     * changes only the requested axes, retaining their actual slopes. */
    weights[SR_STRONG_LOW] = strong * (1.0 - high);
    weights[SR_STRONG_HIGH] = strong * high;
    weights[SR_WEAK_LOW] = weak * (1.0 - high);
    weights[SR_WEAK_HIGH] = weak * high;
}

void cm_sr_family_dac(ARGS)
{
    struct sr_family_transition *tr;
    double weights[SR_DRIVES];
    double low = PARAM(out_low);
    double high = PARAM(out_high);
    double voltage;
    struct sr_family_norton drives[SR_DRIVES];
    struct sr_family_norton result = {0.0, 0.0};
    double leakage = fmax(0.0, PARAM(g_hiz));
    double midpoint = 0.5 * low + 0.5 * high;
    double low_knee = PARAM(knee_low) - low;
    double high_knee = high - PARAM(knee_high);
    double smooth = fmax(0.0, fmin(PARAM(v_smooth), fmin(low_knee, high_knee)));
    int i;

    if (!isfinite(PARAM(t_rise)) || !isfinite(PARAM(t_fall)) ||
        PARAM(t_rise) == 0.0 || PARAM(t_fall) == 0.0) {
        cm_message_send("ERROR: sr_family_dac needs finite nonzero slew times");
        return;
    }

    if (INIT) {
        Digital_t initial;
        cm_event_alloc(0, sizeof(struct sr_family_transition));
        tr = (struct sr_family_transition *)cm_event_get_ptr(0, 0);
        initial.state = INPUT_STATE(in);
        initial.strength = INPUT_STRENGTH(in);
        sr_settle(tr, initial, TIME);
        LOAD(in) = PARAM(input_load);
        LOAD(sink_meta) = PARAM(sink_current);
        LOAD(source_meta) = PARAM(source_current);
        return;
    }

    tr = (struct sr_family_transition *)cm_event_get_ptr(0, 0);

    if (CALL_TYPE == EVENT) {
        Digital_t next;
        double target[SR_AXES];
        double previous, strength_rate;
        next.state = INPUT_STATE(in);
        next.strength = INPUT_STRENGTH(in);
        /* DC is an equilibrium solve, not a transient drive ramp.
         * Persist that equilibrium in event memory so the first transient
         * analog evaluation agrees with the converged operating point. */
        if (ANALYSIS == DC) {
            sr_settle(tr, next, TIME);
            return;
        }
        if (next.state == tr->last.state && next.strength == tr->last.strength)
            return;
        sr_target(next, target);
        if (target[SR_LOGIC] != tr->axis[SR_LOGIC].to) {
            previous = sr_ramp_value(&tr->axis[SR_LOGIC], TIME);
            sr_retarget(&tr->axis[SR_LOGIC], target[SR_LOGIC], TIME, previous,
                        target[SR_LOGIC] >= previous ? PARAM(t_rise) : PARAM(t_fall));
        }
        /* Destination strength selects both conductance-weight rates.
         * Retarget if that rate changes, even at an unchanged endpoint. */
        strength_rate = next.strength == STRONG ? PARAM(t_rise) : PARAM(t_fall);
        for (i = SR_WEAK; i < SR_AXES; ++i) {
            previous = sr_ramp_value(&tr->axis[i], TIME);
            sr_retarget(&tr->axis[i], target[i], TIME, previous, strength_rate);
        }
        tr->last = next;
        cm_analog_set_perm_bkpt(TIME);
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
        struct sr_family_transition equilibrium;
        sr_settle(&equilibrium, dc, TIME);
        sr_weights(&equilibrium, TIME, weights);
    } else {
        sr_weights(tr, TIME, weights);
    }

    voltage = INPUT(out);
    drives[SR_STRONG_LOW] = sr_knee(voltage, low, PARAM(g_pulldown),
                                   low_knee, smooth, -1.0);
    drives[SR_STRONG_HIGH] = sr_knee(voltage, high, PARAM(g_pullup),
                                    high_knee, smooth, 1.0);
    drives[SR_WEAK_LOW] = (struct sr_family_norton){
        low * PARAM(g_resistive), PARAM(g_resistive)};
    drives[SR_WEAK_HIGH] = (struct sr_family_norton){
        high * PARAM(g_resistive), PARAM(g_resistive)};

    for (i = 0; i < SR_DRIVES; ++i) {
        result.source += weights[i] * drives[i].source;
        result.conductance += weights[i] * drives[i].conductance;
    }
    result = sr_leakage_floor(result, leakage, midpoint);
    if (ANALYSIS == AC) {
        Complex_t gain;
        gain.real = result.conductance;
        gain.imag = 0.0;
        AC_GAIN(out, out) = gain;
    } else {
        OUTPUT(out) = result.conductance * voltage - result.source;
        PARTIAL(out, out) = result.conductance;
    }
}
