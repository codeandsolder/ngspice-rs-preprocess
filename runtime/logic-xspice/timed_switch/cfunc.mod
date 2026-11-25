#include <math.h>

/* The solver retains this history with its accepted analog timepoints.
 * A rejected trial must not advance the receiver, ramp, or pending event. */
struct timed_switch_state {
    double time;
    double control;
    double anchor_time;
    double anchor_phase;
    double deadline;
    int target;
    int requested;
};

struct timed_switch_parameters {
    double rise;
    double fall;
    double on_delay;
    double off_delay;
    double low;
    double high;
    double tolerance;
};

static int receiver(double control, const struct timed_switch_parameters *p,
                    int previous)
{
    if (control > p->high)
        return 1;
    if (control < p->low)
        return 0;
    return previous;
}

static double ramp_end(const struct timed_switch_state *s,
                       const struct timed_switch_parameters *p)
{
    double distance = s->target ? 1.0 - s->anchor_phase : s->anchor_phase;
    return s->anchor_time + distance * (s->target ? p->rise : p->fall);
}

static double phase_at(const struct timed_switch_state *s,
                       const struct timed_switch_parameters *p, double time)
{
    if (time >= ramp_end(s, p))
        return s->target;
    double delta = fmax(0.0, time - s->anchor_time);
    double phase = s->anchor_phase +
        (s->target ? delta / p->rise : -delta / p->fall);
    return fmax(0.0, fmin(1.0, phase));
}

static void start_ramp(struct timed_switch_state *s,
                       const struct timed_switch_parameters *p,
                       double time, int target)
{
    double phase = phase_at(s, p, time);
    s->anchor_time = time;
    s->anchor_phase = phase;
    s->target = target;
    s->deadline = -1.0;
}

/* Resolve the preceding timer before the next receiver edge only if
 * it really precedes that edge. Equal-duration pulses are cancelled. */
static void accept_edge(struct timed_switch_state *s,
                        const struct timed_switch_parameters *p,
                        double time, int requested)
{
    if (s->deadline >= 0.0 && s->deadline < time)
        start_ramp(s, p, s->deadline, s->requested);

    s->requested = requested;
    if (requested == s->target) {
        s->deadline = -1.0;
        return;
    }

    /* A reversal of an in-flight ramp changes slope at the current
     * phase. Directional delay applies only when leaving a stable rail.
     * A simultaneous endpoint and reversal belongs to the active ramp. */
    double duration = s->target ? p->rise : p->fall;
    double end = ramp_end(s, p);
    int moving = duration > 0.0 &&
        s->anchor_phase != (double)s->target && time <= end;
    if (moving) {
        start_ramp(s, p, time, requested);
        return;
    }

    double delay = requested ? p->on_delay : p->off_delay;
    s->deadline = time + delay;
    if (s->deadline <= time)
        start_ramp(s, p, time, requested);
}

static double crossing_time(const struct timed_switch_state *s,
                            const struct timed_switch_parameters *p,
                            double time, double control, int requested)
{
    double threshold = requested ? p->high : p->low;
    double delta = control - s->control;
    if (delta == 0.0)
        return time;
    double fraction = (threshold - s->control) / delta;
    fraction = fmax(0.0, fmin(1.0, fraction));
    return s->time + fraction * (time - s->time);
}

void cm_preprocess_timed_switch(ARGS)
{
    struct timed_switch_parameters p = {
        PARAM(ton), PARAM(toff), PARAM(td_on), PARAM(td_off),
        PARAM(vt) - PARAM(vh), PARAM(vt) + PARAM(vh), PARAM(timetol)
    };
    double ron = PARAM(ron), roff = PARAM(roff), control = INPUT(control);
    int initial = PARAM(ic);
    if (!isfinite(ron) || !isfinite(roff) || ron <= 0.0 || roff <= 0.0 ||
        !isfinite(p.low) || !isfinite(p.high) || p.low > p.high ||
        !isfinite(p.rise) || !isfinite(p.fall) ||
        p.rise <= 0.0 || p.fall <= 0.0 ||
        !isfinite(p.on_delay) || !isfinite(p.off_delay) ||
        p.on_delay < 0.0 || p.off_delay < 0.0 ||
        !isfinite(p.tolerance) || p.tolerance < 1e-15 ||
        (initial != 0 && initial != 1)) {
        cm_message_send("Error: invalid preprocess_timed_switch parameters");
        return;
    }

    if (INIT)
        cm_analog_alloc(0, sizeof(struct timed_switch_state));
    struct timed_switch_state *state = cm_analog_get_ptr(0, 0);
    double phase;
    if (ANALYSIS == MIF_DC || (ANALYSIS == MIF_TRAN && TIME == 0.0)) {
        int bit = receiver(control, &p, initial);
        *state = (struct timed_switch_state) {
            TIME, control, TIME, (double)bit, -1.0, bit, bit
        };
        phase = bit;
    } else if (ANALYSIS == MIF_TRAN) {
        const struct timed_switch_state *old = cm_analog_get_ptr(0, 1);
        *state = *old;
        int requested = receiver(control, &p, old->requested);
        double edge = TIME;
        if (requested != old->requested) {
            edge = crossing_time(old, &p, TIME, control, requested);
            /* Always try the actual crossing. If too close to a prior
             * breakpoint, refine the bracket to the requested tolerance
             * rather than changing physical event times. */
            if (edge < TIME) {
                int localized = edge > old->time &&
                    !cm_analog_set_temp_bkpt(edge);
                if (!localized && TIME - old->time > p.tolerance) {
                    double midpoint = old->time + 0.5 * (TIME - old->time);
                    if (midpoint > old->time && midpoint < TIME)
                        cm_analog_set_temp_bkpt(midpoint);
                }
            }
            accept_edge(state, &p, edge, requested);
        }

        if (state->deadline >= 0.0 && state->deadline <= TIME)
            start_ramp(state, &p, state->deadline, state->requested);

        if (state->deadline > TIME)
            cm_analog_set_perm_bkpt(state->deadline);
        double end = ramp_end(state, &p);
        if (end > TIME)
            cm_analog_set_perm_bkpt(end);
        state->time = TIME;
        state->control = control;
        phase = phase_at(state, &p, TIME);
    } else {
        /* Small signal analysis freezes the switching state at the
         * operating point: a voltage perturbation within the band has
         * no incremental control-to-branch gain. */
        phase = phase_at(state, &p, state->time);
    }

    /* Cubic log-conductance interpolation. Work in log space without
     * forming ron*roff or ron/roff (which can overflow/underflow). */
    double conductance;
    if (phase <= 0.0)
        conductance = 1.0 / roff;
    else if (phase >= 1.0)
        conductance = 1.0 / ron;
    else {
        double blend = phase * phase * (3.0 - 2.0 * phase);
        conductance = exp(-log(roff) + blend * (log(roff) - log(ron)));
    }
    if (ANALYSIS == MIF_AC) {
        Mif_Complex_t gain = {conductance, 0.0};
        Mif_Complex_t zero = {0.0, 0.0};
        AC_GAIN(branch,branch) = gain;
        AC_GAIN(branch,control) = zero;
    } else {
        OUTPUT(branch) = INPUT(branch) * conductance;
        PARTIAL(branch,branch) = conductance;
        PARTIAL(branch,control) = 0.0;
    }
}
