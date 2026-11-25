#include <math.h>

/* State is stored by the simulator, not in static memory, so a rejected
 * analog step restores both the pending deadline and the committed bit. */
struct logic_event_state {
    Digital_State_t committed;
    Digital_State_t requested;
    double deadline;
};

void cm_preprocess_logic_event(ARGS)
{
    struct logic_event_state *state;
    Digital_State_t desired, prev, bit;
    int op = PARAM(operation);
    int i, size = PORT_SIZE(in);
    double delay = PARAM(delay);
    int inertial = PARAM(inertial);

    if (INIT) {
        cm_event_alloc(0, sizeof(struct logic_event_state));
        state = (struct logic_event_state *)cm_event_get_ptr(0, 0);
        state->committed = (Digital_State_t)PARAM(ic);
        state->requested = (Digital_State_t)(UNKNOWN + 1);
        state->deadline = -1.0;
        for (i = 0; i < size; ++i)
            LOAD(in[i]) = 0.0;
    } else {
        state = (struct logic_event_state *)cm_event_get_ptr(0, 0);
    }

    if (CALL_TYPE != EVENT)
        return;

    /* Recompute DC from IC each time: iteration is not a physical event.
     * In transient, a completed event commits the bit before evaluating
     * the next latch request. */
    prev = state->committed;
    if (ANALYSIS == DC) {
        state->committed = (Digital_State_t)PARAM(ic);
        state->deadline = -1.0;
    } else if (inertial && state->deadline >= 0.0 &&
               TIME >= state->deadline) {
        state->committed = state->requested;
        state->deadline = -1.0;
    }

    if (op < 4) {
        int conjunction = op == 0 || op == 2;
        int inverted = op >= 2;
        desired = conjunction ? ONE : ZERO;
        for (i = 0; i < size; ++i) {
            bit = INPUT_STATE(in[i]);
            if (bit == (conjunction ? ZERO : ONE)) {
                desired = bit;
                break;
            }
            if (bit == UNKNOWN)
                desired = UNKNOWN;
        }
        if (inverted && desired != UNKNOWN)
            desired = desired == ONE ? ZERO : ONE;
    } else {
        Digital_State_t set = INPUT_STATE(in[0]);
        Digital_State_t reset = INPUT_STATE(in[1]);
        desired = state->committed;
        if (set == ONE && reset == ONE) {
            if (op == 4)
                desired = ZERO;
            else if (op == 5)
                desired = ONE;
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
        /* Constant transport delay yields an ordered queue. Same-time
         * delta updates replace the prior event at the same deadline. */
        if (desired == state->requested) {
            OUTPUT_CHANGED(out) = FALSE;
            return;
        }
        state->committed = desired;
        state->requested = desired;
        OUTPUT_DELAY(out) = delay;
    } else {
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
                cm_event_queue(state->deadline);
            }
        }
        if (state->committed == prev) {
            OUTPUT_CHANGED(out) = FALSE;
            return;
        }
        OUTPUT_DELAY(out) = 0.0;
    }
    OUTPUT_STATE(out) = state->committed;
    OUTPUT_STRENGTH(out) = STRONG;
}
