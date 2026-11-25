#include "ngspice/inertial.h"
#include "../common/load_delay.h"
#include <string.h>

/* The staged event must retain drive strength as well as logic level:
 * a delayed 0->Z or Z->1 transition is not just a state transition. */
struct sr_lut_inertial {
    Digital_t previous;
    double when;
};

static int sr_digital_equal(Digital_t first, Digital_t second)
{
    return first.state == second.state && first.strength == second.strength;
}

void cm_sr_alb_lut_bit(ARGS)
{
    Digital_t value = {UNKNOWN, UNDETERMINED};
    const Digital_t passive_meta = {ZERO, HI_IMPEDANCE};
    Digital_t *last;
    int i;
    int width = PORT_SIZE(in);

    if (INIT) {
        size_t expected, length;
        const char *table = PARAM(table_values);
        cm_event_alloc(0, sizeof(Digital_t));
        last = (Digital_t *)cm_event_get_ptr(0, 0);
        last->state = (Digital_State_t)(UNKNOWN + 1);
        last->strength = UNDETERMINED;
        STATIC_VAR(is_inertial) = cm_is_inertial(
            PARAM_NULL(inertial_delay) ? Not_set : PARAM(inertial_delay));
        STATIC_VAR(transient_wake_queued) = FALSE;
        if (STATIC_VAR(is_inertial)) {
            struct sr_lut_inertial *pending;
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
        for (i = 0; i < width; i++)
            LOAD(in[i]) = PARAM(input_load);
    } else {
        last = (Digital_t *)cm_event_get_ptr(0, 0);
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
        /* The full SIMetrix 9.40d 12-state source matrix establishes that
         * ALB expressions use the level bit: HIGH -> 1, LOW/UNKNOWN -> 0,
         * independent of drive strength. Do not confuse expression coercion
         * with the digital net's 12-state electrical resolution.
         *
         * Keep the independent Boolean-consensus option for generic native
         * LUTs. Compiled source ALBs explicitly request vendor coercion.
         * The table includes 0, 1 and Z as distinct drive results.
         */
        unsigned known_ones = 0;
        int agreed = -1;
        const char *table = PARAM(table_values);

        for (i = 0; i < width; i++) {
            if (INPUT_STATE(in[i]) == ONE)
                known_ones |= 1u << i;
        }
        if (PARAM(unknown_as_zero)) {
            char symbol = table[known_ones];
            agreed = (symbol == 'z' || symbol == 'Z') ? 2 :
                     (symbol == '1') ? 1 : 0;
        } else {
            /* Generic Boolean-consensus envelope over every compatible
             * binary row, preserving strong known values under ambiguity. */
            unsigned unknown_mask = 0;
            unsigned subset;
            for (i = 0; i < width; i++) {
                if (INPUT_STATE(in[i]) == UNKNOWN)
                    unknown_mask |= 1u << i;
            }
            subset = unknown_mask;
            for (;;) {
                char symbol = table[known_ones | subset];
                int row_value = (symbol == 'z' || symbol == 'Z') ? 2 :
                                (symbol == '1') ? 1 : 0;
                if (agreed == -1)
                    agreed = row_value;
                else if (agreed != row_value) {
                    agreed = 3;
                    break;
                }
                if (subset == 0)
                    break;
                subset = (subset - 1u) & unknown_mask;
            }
        }
        if (agreed == 0 || agreed == 1) {
            value.state = agreed == 1 ? ONE : ZERO;
            value.strength = STRONG;
        } else if (agreed == 2) {
            value.state = UNKNOWN;
            value.strength = HI_IMPEDANCE;
        }
    }

    if ((value.state == ZERO && TOTAL_LOAD(sink_meta) < PARAM(min_sink)) ||
        (value.state == ONE && TOTAL_LOAD(source_meta) > PARAM(max_source))) {
        value.state = UNKNOWN;
        value.strength = UNDETERMINED;
    }

    if (sr_digital_equal(value, *last)) {
        OUTPUT_CHANGED(out) = FALSE;
        return;
    }
    OUTPUT_DELAY(out) = ANALYSIS == DC ? 0.0 :
        sr_effective_load_delay(PARAM(delay), PARAM(out_res), TOTAL_LOAD(out));

    if (STATIC_VAR(is_inertial) && ANALYSIS == TRANSIENT) {
        struct sr_lut_inertial *pending =
            (struct sr_lut_inertial *)cm_event_get_ptr(1, 0);
        if (pending->when <= TIME) {
            pending->previous = *last;
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
    *last = value;
    OUTPUT_STATE(out) = value.state;
    OUTPUT_STRENGTH(out) = value.strength;
}
