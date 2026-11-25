#include <math.h>

struct receiver_sample {
    double voltage;
    Digital_State_t bit;
};

struct logic_event_state {
    Digital_State_t committed;
    Digital_State_t requested;
    double deadline;
    double time;
    struct receiver_sample receiver[];
};

static Digital_State_t receiver_value(double value, double low, double high,
                                      int inverted, Digital_State_t previous)
{
    if (value > high)
        return inverted ? ZERO : ONE;
    if (value < low)
        return inverted ? ONE : ZERO;
    return previous;
}

void cm_preprocess_logic_event(ARGS)
{
    struct logic_event_state *state;
    Digital_State_t desired, before;
    int op = PARAM(operation), inertial = PARAM(inertial);
    int i, size = PORT_SIZE(in);
    double delay = PARAM(delay), low = PARAM(low), high = PARAM(high);
    double earliest = TIME;
    if (op >= 4 && size != 2) {
        cm_message_send("preprocess_logic_event: latch requires exactly two inputs");
        OUTPUT_CHANGED(out) = FALSE;
        return;
    }
    if (INIT) {
        cm_event_alloc(0, (int)(sizeof(struct logic_event_state) +
            (size_t)size * sizeof(struct receiver_sample)));
        state = cm_event_get_ptr(0, 0);
        state->committed = (Digital_State_t)PARAM(ic);
        state->requested = (Digital_State_t)(UNKNOWN + 1);
        state->deadline = -1.0;
        state->time = TIME;
        for (i = 0; i < size; ++i) {
            state->receiver[i].voltage = INPUT(in[i]);
            state->receiver[i].bit = (Digital_State_t)PARAM(rx_ic[i]);
        }
    } else {
        state = cm_event_get_ptr(0, 0);
    }
    if (CALL_TYPE != EVENT)
        return;

    before = state->committed;
    if (ANALYSIS == DC) {
        state->committed = (Digital_State_t)PARAM(ic);
        state->deadline = -1.0;
    }

    /* The hybrid receives converged analog values before committing a
     * timer. Locate crossings against the previous accepted samples. */
    if (ANALYSIS == TRANSIENT && TIME > state->time) {
        for (i = 0; i < size; ++i) {
            struct receiver_sample *old = &state->receiver[i];
            int inverted = PARAM(inverted[i]);
            double value = INPUT(in[i]);
            Digital_State_t bit = receiver_value(value, low, high, inverted, old->bit);
            if (bit != old->bit) {
                double threshold = ((old->bit == ONE) != inverted) ? low : high;
                if ((old->voltage <= threshold && value > threshold) ||
                    (old->voltage >= threshold && value < threshold)) {
                    double f = (threshold - old->voltage) / (value - old->voltage);
                    double when = nextafter(state->time + f * (TIME - state->time), TIME);
                    if (when > state->time && when < earliest)
                        earliest = when;
                }
            }
        }
        if (state->deadline > state->time && state->deadline < earliest)
            earliest = state->deadline;
        if (earliest < TIME) {
            double midpoint = state->time + 0.5 * (TIME - state->time);
            /* The requested crossing may be closer than the solver's
             * minimum breakpoint spacing to the last accepted sample,
             * especially when a signal departs a threshold plateau.
             * Narrow the accepted interval instead of silently delaying
             * the transition by the whole next analog step. */
            if (!cm_analog_set_temp_bkpt(earliest) ||
                (earliest < midpoint && midpoint > state->time &&
                 midpoint < TIME && !cm_analog_set_temp_bkpt(midpoint))) {
                OUTPUT_CHANGED(out) = FALSE;
                return;
            }
        }
    }

    for (i = 0; i < size; ++i) {
        struct receiver_sample *old = &state->receiver[i];
        old->voltage = INPUT(in[i]);
        old->bit = receiver_value(old->voltage, low, high, PARAM(inverted[i]),
            ANALYSIS == DC ? (Digital_State_t)PARAM(rx_ic[i]) : old->bit);
    }
    state->time = TIME;

    if (op < 4) {
        int conjunction = op == 0 || op == 2;
        desired = conjunction ? ONE : ZERO;
        for (i = 0; i < size; ++i) {
            Digital_State_t bit = state->receiver[i].bit;
            if (bit == (conjunction ? ZERO : ONE)) {
                desired = bit;
                break;
            }
        }
        if (op >= 2)
            desired = desired == ONE ? ZERO : ONE;
    } else {
        Digital_State_t set = state->receiver[0].bit;
        Digital_State_t reset = state->receiver[1].bit;
        desired = state->committed;
        if (set == ONE && reset == ONE) {
            if (op == 4) desired = ZERO;
            else if (op == 5) desired = ONE;
        } else if (set == ONE && reset == ZERO) {
            desired = ONE;
        } else if (set == ZERO && reset == ONE) {
            desired = ZERO;
        }
    }

    if (ANALYSIS == DC) {
        state->committed = desired;
        state->requested = desired;
        OUTPUT_DELAY(out) = 0.0;
    } else if (!inertial) {
        if (desired == state->requested) {
            OUTPUT_CHANGED(out) = FALSE;
            return;
        }
        state->committed = desired;
        state->requested = desired;
        OUTPUT_DELAY(out) = delay;
    } else {
        if (state->deadline >= 0.0 && TIME >= state->deadline && desired == state->requested) {
            state->committed = state->requested;
            state->deadline = -1.0;
        }
        if (desired == state->committed) {
            state->deadline = -1.0;
            state->requested = desired;
        } else if (state->deadline < 0.0 || desired != state->requested) {
            state->requested = desired;
            state->deadline = TIME + delay;
            if (state->deadline <= TIME) {
                state->committed = desired;
                state->deadline = -1.0;
            } else {
                cm_analog_set_perm_bkpt(state->deadline);
            }
        }
        if (state->committed == before) {
            OUTPUT_CHANGED(out) = FALSE;
            return;
        }
        OUTPUT_DELAY(out) = 0.0;
    }
    OUTPUT_STATE(out) = state->committed;
    OUTPUT_STRENGTH(out) = STRONG;
}
