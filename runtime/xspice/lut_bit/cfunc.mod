#include "ngspice/inertial.h"
#include "../common/load_delay.h"
#include <string.h>

/* The staged event must retain drive strength as well as logic level:
 * a delayed 0->Z or Z->1 transition is not just a state transition. */
struct sr_lut_inertial {
    Digital_t previous;
    double when;
};

/* Raw logical request identity is distinct from a physical drive
 * after current limiting and dormant-polarity resolution. */
struct sr_lut_request {
    Digital_t logical;
    Digital_t physical;
};

static int sr_digital_equal(Digital_t first, Digital_t second)
{
    return first.state == second.state && first.strength == second.strength;
}

/* Vendor output timing uses requested logic, even for HI-Z.
 * A generic consensus LUT has no dormant logic in its Z symbol, so
 * its release resistance follows the previous physical drive. */
static double sr_output_resistance(Digital_t bit,
                                   double positive, double negative)
{
    if (bit.state == ONE)
        return positive;
    if (bit.state == ZERO)
        return negative;
    return positive == negative ? positive : positive / 2.0 + negative / 2.0;
}

void cm_sr_alb_lut_bit(ARGS)
{
    Digital_t value = {UNKNOWN, UNDETERMINED};
    const Digital_t passive_meta = {ZERO, HI_IMPEDANCE};
    struct sr_lut_request *last;
    struct sr_lut_inertial *pending;
    int logical_changed;
    int i;
    int width = PORT_SIZE(in);

    if (INIT) {
        size_t expected, length;
        const char *table = PARAM(table_values);
        cm_event_alloc(0, sizeof(struct sr_lut_request));
        /* cm_event_alloc may realloc the shared event storage: do not
         * retain a pointer into it across the subsequent allocations. */
        STATIC_VAR(is_inertial) = cm_is_inertial(
            PARAM_NULL(inertial_delay) ? Not_set : PARAM(inertial_delay));
        STATIC_VAR(transient_wake_queued) = FALSE;
        if (STATIC_VAR(is_inertial)) {
            cm_event_alloc(1, sizeof(struct sr_lut_inertial));
            pending = (struct sr_lut_inertial *)cm_event_get_ptr(1, 0);
            pending->when = -1.0;
        }
        STATIC_VAR(valid) = width > 0 && width <= 16;
        expected = STATIC_VAR(valid) ? (size_t)1 << width : 0;
        length = strlen(table);
        if (length != expected)
            STATIC_VAR(valid) = FALSE;
        for (i = 0; STATIC_VAR(valid) && (size_t)i < length; i++) {
            char symbol = table[i];
            if (symbol != '0' && symbol != '1' && symbol != 'Z' && symbol != 'z')
                STATIC_VAR(valid) = FALSE;
        }
        if (!STATIC_VAR(valid))
            cm_message_send("ERROR: sr_alb_lut_bit requires 1..16 input bits and exactly 2^N table characters (0,1,Z)");
        if (PARAM(unknown_as_last) && STATIC_VAR(valid)) {
            Digital_State_t *history;
            cm_event_alloc(2, width * sizeof(Digital_State_t));
            history = (Digital_State_t *)cm_event_get_ptr(2, 0);
            /* Vendor ALB expressions initialize an unobserved UNKNOWN
             * input as LOW, but retain the most recent known input on
             * later UNKNOWN transitions. Store each LUT source separately
             * so unrelated inputs never share a history bit. */
            for (i = 0; i < width; i++)
                history[i] = ZERO;
        }
        /* Every event storage allocation is complete; this pointer
         * remains valid until the next allocation. */
        last = (struct sr_lut_request *)cm_event_get_ptr(0, 0);
        last->logical.state = (Digital_State_t)(UNKNOWN + 1);
        last->logical.strength = UNDETERMINED;
        last->physical = last->logical;
        for (i = 0; i < width; i++)
            LOAD(in[i]) = PARAM(input_load);
    } else {
        last = (struct sr_lut_request *)cm_event_get_ptr(0, 0);
    }

    if (CALL_TYPE == ANALOG) {
        if (ANALYSIS == TRANSIENT && !STATIC_VAR(transient_wake_queued)) {
            cm_event_queue(TIME);
            STATIC_VAR(transient_wake_queued) = TRUE;
        }
        return;
    }
    if (CALL_TYPE != EVENT)
        return;

    /* Digital outputs carry the state and its two independent load sums.
     * The metadata ports never assert a logic value on the shared bundle. */
    OUTPUT_STATE(sink_meta) = passive_meta.state;
    OUTPUT_STRENGTH(sink_meta) = passive_meta.strength;
    OUTPUT_STATE(source_meta) = passive_meta.state;
    OUTPUT_STRENGTH(source_meta) = passive_meta.strength;

    /* An explicitly known initial bit overrides the DC combinational
     * equation. With IC=UNKNOWN, resolve the LUT from actual DC input
     * states, rather than driving a spurious UNKNOWN analog equilibrium
     * even when the input levels are already known. */
    if (ANALYSIS == DC && PARAM(ic) != UNKNOWN) {
        value.state = (Digital_State_t)PARAM(ic);
        value.strength = STRONG;
    } else if (STATIC_VAR(valid)) {
        /* The independent static 12-state vendor source matrix says
         * HIGH -> 1 and initial LOW/UNKNOWN -> 0, ignoring strength.
         * The real 9.40d time-domain HC ADC->ALB vs ADC->DAC experiment
         * adds one distinct requirement: an ALB input becoming UNKNOWN
         * after a known level RETAINS that last known level. A generic
         * XSPICE LUT still exposes its own 3-valued consensus behavior.
         */
        unsigned known_ones = 0;
        unsigned unknown_mask = 0;
        unsigned possible = 0;
        Digital_State_t *history = PARAM(unknown_as_last) ?
            (Digital_State_t *)cm_event_get_ptr(2, 0) : NULL;
        const char *table = PARAM(table_values);

        for (i = 0; i < width; i++) {
            Digital_State_t bit = INPUT_STATE(in[i]);
            if (history != NULL) {
                if (bit == ZERO || bit == ONE)
                    history[i] = bit;
                else
                    bit = history[i];
            }
            if (bit == ONE)
                known_ones |= 1u << i;
            if (bit == UNKNOWN)
                unknown_mask |= 1u << i;
        }
        if (PARAM(unknown_as_zero)) {
            char symbol = table[known_ones];
            possible = 1u << ((symbol == 'z' || symbol == 'Z') ? 2 :
                              (symbol == '1') ? 1 : 0);
        } else {
            /* Generic Boolean-consensus envelope over every compatible
             * binary row, preserving strong known values under ambiguity. */
            unsigned subset = unknown_mask;
            for (;;) {
                char symbol = table[known_ones | subset];
                int row_value = (symbol == 'z' || symbol == 'Z') ? 2 :
                                (symbol == '1') ? 1 : 0;
                possible |= 1u << row_value;
                if (possible == 7)
                    break;
                if (subset == 0)
                    break;
                subset = (subset - 1u) & unknown_mask;
            }
        }
        if (possible != 0 && (possible & 4u) == 0) {
            value.state = possible == 1 ? ZERO : possible == 2 ? ONE : UNKNOWN;
            value.strength = STRONG;
        } else if (possible == 4) {
            value.state = UNKNOWN;
            value.strength = HI_IMPEDANCE;
        }
    }

    logical_changed = !sr_digital_equal(value, last->logical);
    last->logical = value;

    if ((value.state == ZERO && TOTAL_LOAD(sink_meta) < PARAM(min_sink)) ||
        (value.state == ONE && TOTAL_LOAD(source_meta) > PARAM(max_source))) {
        value.state = UNKNOWN;
        value.strength = STRONG;
    }

    if (PARAM(retain_disabled_polarity) && value.strength == HI_IMPEDANCE) {
        /* last is the requested drive, not the committed physical drive.
         * Preserve it even when its inertial event is still pending. */
        value.state = ANALYSIS != DC && last->physical.state <= UNKNOWN ? last->physical.state :
            (TOTAL_LOAD(sink_meta) < PARAM(min_sink) ? UNKNOWN : ZERO);
    }

    pending = STATIC_VAR(is_inertial) && ANALYSIS == TRANSIENT ?
        (struct sr_lut_inertial *)cm_event_get_ptr(1, 0) : NULL;
    /* An unchanged raw request must not restart the timer. A different
     * raw request can map to the same pending overloaded drive, but must
     * still supersede the previous inertial event. Once that drive has
     * committed, there is no redundant physical transition to schedule. */
    if (sr_digital_equal(value, last->physical) &&
        (!logical_changed || pending == NULL || pending->when <= TIME)) {
        OUTPUT_CHANGED(out) = FALSE;
        return;
    }
    {
        Digital_t previous = last->physical;
        if (pending != NULL && pending->when > TIME)
            previous = pending->previous;
        Digital_t timing = value.strength == HI_IMPEDANCE &&
                           !PARAM(retain_disabled_polarity) ? previous : value;
        OUTPUT_DELAY(out) = ANALYSIS == DC ? 0.0 :
            sr_effective_load_delay(PARAM(delay),
                sr_output_resistance(timing, PARAM(out_res_pos), PARAM(out_res_neg)),
                TOTAL_LOAD(out));
        if (ANALYSIS != DC && OUTPUT_DELAY(out) < PARAM(minimum_delay))
            OUTPUT_DELAY(out) = PARAM(minimum_delay);
    }

    if (pending != NULL) {
        if (pending->when <= TIME) {
            pending->previous = last->physical;
            pending->when = TIME + OUTPUT_DELAY(out);
        } else if (!sr_digital_equal(value, pending->previous)) {
            Digital_t rollback = pending->previous;
            cm_schedule_output(1, 0, (pending->when - TIME) / 2.0, &rollback);
            pending->when = TIME + OUTPUT_DELAY(out);
        } else {
            OUTPUT_DELAY(out) = (pending->when - TIME) / 2.0;
            pending->when = -1.0;
        }
    }
    last->physical = value;
    OUTPUT_STATE(out) = value.state;
    OUTPUT_STRENGTH(out) = value.strength;
}
