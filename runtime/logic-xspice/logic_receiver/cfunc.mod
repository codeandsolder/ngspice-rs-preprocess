#include <math.h>

struct logic_rx_sample {
    double time;
    double voltage;
};

static Digital_State_t logic_rx_value(double input, double low, double high,
                                      int inverted,
                                      Digital_State_t previous)
{
    if (input > high)
        return inverted ? ZERO : ONE;
    if (input < low)
        return inverted ? ONE : ZERO;
    return previous;
}

void cm_preprocess_logic_receiver(ARGS)
{
    Digital_State_t *previous, desired;
    struct logic_rx_sample *current, *old;
    double value = INPUT(in);
    double low = PARAM(low), high = PARAM(high);
    int inverted = PARAM(inverted);
    Digital_State_t initial = (Digital_State_t)PARAM(ic);
    if (INIT) {
        cm_event_alloc(0, sizeof(Digital_State_t));
        previous = (Digital_State_t *)cm_event_get_ptr(0, 0);
        *previous = (Digital_State_t)(UNKNOWN + 1);
        cm_analog_alloc(0, sizeof(struct logic_rx_sample));
        current = (struct logic_rx_sample *)cm_analog_get_ptr(0, 0);
        current->time = TIME;
        current->voltage = value;
        return;
    }
    previous = (Digital_State_t *)cm_event_get_ptr(0, 0);
    desired = logic_rx_value(value, low, high, inverted,
                             ANALYSIS == DC || *previous > ONE ? initial : *previous);
    if (CALL_TYPE == EVENT) {
        OUTPUT_CHANGED(out) = desired != *previous;
        if (desired != *previous) {
            OUTPUT_STATE(out) = desired;
            OUTPUT_STRENGTH(out) = STRONG;
            OUTPUT_DELAY(out) = 0.0;
            *previous = desired;
        }
        return;
    }
    if (CALL_TYPE != ANALOG)
        return;

    current = (struct logic_rx_sample *)cm_analog_get_ptr(0, 0);
    old = (struct logic_rx_sample *)cm_analog_get_ptr(0, 1);
    current->time = TIME;
    current->voltage = value;
    if (ANALYSIS == TRANSIENT && TIME > old->time && desired != *previous &&
        *previous <= ONE) {
        /* Locate the active threshold in the accepted analog interval.
         * A strict equality does not transition unless the trajectory
         * really passes beyond it. No artificial fixed receiver delay. */
        double threshold = ((*previous == ONE) != inverted) ? low : high;
        if ((old->voltage < threshold && value > threshold) ||
            (old->voltage > threshold && value < threshold)) {
            double fraction = (threshold - old->voltage) / (value - old->voltage);
            double crossing = old->time + fraction * (TIME - old->time);
            crossing = nextafter(crossing, TIME);
            if (crossing > old->time && crossing < TIME &&
                cm_analog_set_temp_bkpt(crossing) == 0)
                return;
        }
    }
    if (desired != *previous)
        cm_event_queue(TIME);
}
