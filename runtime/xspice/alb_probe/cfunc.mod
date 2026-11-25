#include "ngspice/inertial.h"
#include "../common/load_delay.h"

void cm_sr_alb_probe(ARGS)
{
    Digital_State_t val, *out;

    if (INIT) {
        cm_event_alloc(0, sizeof(Digital_State_t));
        STATIC_VAR(is_inertial) = cm_is_inertial(
            PARAM_NULL(inertial_delay) ? Not_set : PARAM(inertial_delay));
        STATIC_VAR(transient_wake_queued) = FALSE;
        if (STATIC_VAR(is_inertial)) {
            cm_event_alloc(1, sizeof(struct idata));
            ((struct idata *)cm_event_get_ptr(1, 0))->when = -1.0;
        }
        out = (Digital_State_t *)cm_event_get_ptr(0, 0);
        *out = (Digital_State_t)(UNKNOWN + 1);
        LOAD(in) = PARAM(input_load);
    } else {
        out = (Digital_State_t *)cm_event_get_ptr(0, 0);
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

    val = ANALYSIS == DC ? (Digital_State_t)PARAM(ic) : INPUT_STATE(in);
    if (val == *out) {
        OUTPUT_CHANGED(out) = FALSE;
        return;
    }

    OUTPUT_DELAY(out) = ANALYSIS == DC ? 0.0 : sr_effective_load_delay(PARAM(delay), PARAM(out_res), TOTAL_LOAD(out));

    if (STATIC_VAR(is_inertial) && ANALYSIS == TRANSIENT) {
        struct idata *idp = (struct idata *)cm_event_get_ptr(1, 0);
        if (idp->when <= TIME) {
            idp->prev = *out;
            idp->when = TIME + OUTPUT_DELAY(out);
        } else if (val != idp->prev) {
            Digital_t ov = {idp->prev, STRONG};
            cm_schedule_output(1, 0, (idp->when - TIME) / 2.0, &ov);
            idp->when = TIME + OUTPUT_DELAY(out);
        } else {
            OUTPUT_DELAY(out) = (idp->when - TIME) / 2.0;
            idp->when = -1.0;
        }
    }

    *out = val;
    OUTPUT_STATE(out) = val;
    OUTPUT_STRENGTH(out) = STRONG;
}
