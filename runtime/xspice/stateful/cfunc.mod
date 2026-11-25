#include <math.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include "ngspice/cktdefs.h"

/* Repr(C) layout from crates/model-translate-simplis/src/ffi.rs */
struct sr_alb_event {
    double time_s;
    size_t pin;
    int state;
    int strength;
};

/* Matches the Rust repr(C) PhysicalOutputLoad. */
struct sr_alb_output_load {
    double capacitance_f;
    double sink_a;
    double source_a;
};

extern void *sr_alb_runtime_new(const char *source, size_t n_in, size_t n_out,
                               double reg_delay, double port_delay,
                               double setup_time, double hold_time, double min_clock,
                               double out_res_pos, double out_res_neg,
                               double min_sink, double max_source,
                               const double *user_values, size_t user_count,
                               const double *device_values, size_t device_count,
                               double user_scale,
                               char *error, size_t error_len);
extern int sr_alb_runtime_advance(void *runtime, double time,
                                  const uint8_t *inputs, size_t n_in,
                                  const struct sr_alb_output_load *output_loads, uint8_t transient,
                                  const struct sr_alb_event **events, size_t *count,
                                  double *next_time, char *error, size_t error_len);
extern int sr_alb_runtime_accept(void *runtime, double accepted_time,
                                 char *error, size_t error_len);
extern void sr_alb_runtime_free(void *runtime);

static void cm_sr_alb_stateful_destroy(ARGS, Mif_Callback_Reason_t reason)
{
    if (reason == MIF_CB_DESTROY) {
        void *runtime = STATIC_VAR(runtime);
        if (runtime)
            sr_alb_runtime_free(runtime);
        STATIC_VAR(runtime) = NULL;
    }
}

void cm_sr_alb_stateful(ARGS)
{
    void *runtime;
    char error[512];
    const struct sr_alb_event *events = NULL;
    size_t count = 0, n_in, n_out, j;
    uint8_t inputs[32];
    struct sr_alb_output_load output_loads[32];
    double next_time = INFINITY;
    int i, status;

    n_in = (size_t)PORT_SIZE(in);
    n_out = (size_t)PORT_SIZE(out);
    if (INIT) {
        CALLBACK = cm_sr_alb_stateful_destroy;
        STATIC_VAR(transient_wake_queued) = FALSE;
        STATIC_VAR(runtime) = NULL;
        if (n_in == 0 || n_in > 32 || n_out == 0 || n_out > 32) {
            cm_message_send("ERROR: sr_alb_stateful needs 1..32 input/output pins");
            return;
        }
        if ((size_t)PORT_SIZE(sink_meta) != n_out ||
            (size_t)PORT_SIZE(source_meta) != n_out) {
            cm_message_send("ERROR: sr_alb_stateful output metadata vectors must match output width");
            return;
        }
        {
            size_t n_user = PARAM_NULL(user_values) ? 0 : (size_t)PARAM_SIZE(user_values);
            size_t n_device = PARAM_NULL(device_values) ? 0 : (size_t)PARAM_SIZE(device_values);
            double *user_params = NULL, *device_params = NULL;
            if (n_user > SIZE_MAX / sizeof(double) ||
                n_device > SIZE_MAX / sizeof(double)) {
                cm_message_send("ERROR: sr_alb_stateful parameter vector size overflow");
                return;
            }
            if (n_user)
                user_params = malloc(n_user * sizeof(double));
            if (n_device)
                device_params = malloc(n_device * sizeof(double));
            if ((n_user && !user_params) || (n_device && !device_params)) {
                free(user_params);
                free(device_params);
                cm_message_send("ERROR: sr_alb_stateful parameter vector allocation failed");
                return;
            }
            for (i = 0; i < (int)n_user; ++i)
                user_params[i] = PARAM(user_values[i]);
            for (i = 0; i < (int)n_device; ++i)
                device_params[i] = PARAM(device_values[i]);
            runtime = sr_alb_runtime_new(
                PARAM(source_hex), n_in, n_out, PARAM(reg_delay), PARAM(port_delay),
                PARAM(setup_time), PARAM(hold_time), PARAM(min_clock),
                PARAM(out_res_pos), PARAM(out_res_neg), PARAM(min_sink), PARAM(max_source),
                user_params, n_user, device_params, n_device, PARAM(user_scale),
                error, sizeof(error));
            free(user_params);
            free(device_params);
        }
        STATIC_VAR(runtime) = runtime;
        if (!runtime) {
            cm_message_printf("ERROR: sr_alb_stateful: %s", error);
            return;
        }
        for (i = 0; i < (int)n_in; ++i)
            LOAD(in[i]) = PARAM(input_load);
    }
    runtime = STATIC_VAR(runtime);
    if (!runtime)
        return;

    if (CALL_TYPE == ANALOG) {
        if (ANALYSIS == TRANSIENT && !STATIC_VAR(transient_wake_queued)) {
            cm_event_queue(TIME);
            STATIC_VAR(transient_wake_queued) = TRUE;
        }
        return;
    }
    if (CALL_TYPE != EVENT)
        return;

    if (ANALYSIS == TRANSIENT) {
        /*
         * During a tentative analog step, ngspice 47 uses
         * CKTtime - CKTdelta for the last accepted analog time.
         * Future EVENT calls may backtrack within that unaccepted
         * interval, but no rollback can cross the accepted boundary.
         * Do not discard digital history based on tentative TIME.
         */
        CKTcircuit *ckt = cm_get_circuit();
        if (ckt && isfinite(ckt->CKTtime) && isfinite(ckt->CKTdelta) &&
            ckt->CKTdelta >= 0.0 && ckt->CKTtime >= ckt->CKTdelta) {
            double accepted = ckt->CKTtime - ckt->CKTdelta;
            if (accepted <= TIME &&
                sr_alb_runtime_accept(runtime, accepted, error, sizeof(error)) != 0) {
                cm_message_printf("ERROR: sr_alb_stateful acceptance: %s", error);
                return;
            }
        }
    }

    for (i = 0; i < (int)n_in; ++i) {
        Digital_State_t value = INPUT_STATE(in[i]);
        inputs[i] = value == ONE ? 1 : (value == ZERO ? 0 : 2);
    }
    for (i = 0; i < (int)n_out; ++i) {
        OUTPUT_CHANGED(out[i]) = FALSE;
        /* Metadata nets carry downstream electrical loads, not logic.
         * The Rust engine owns output timing and inertial cancellation.
         * XSPICE load is constant after initialization. */
        output_loads[i].capacitance_f = TOTAL_LOAD(out[i]);
        output_loads[i].sink_a = TOTAL_LOAD(sink_meta[i]);
        output_loads[i].source_a = TOTAL_LOAD(source_meta[i]);
        OUTPUT_STATE(sink_meta[i]) = ZERO;
        OUTPUT_STRENGTH(sink_meta[i]) = HI_IMPEDANCE;
        OUTPUT_DELAY(sink_meta[i]) = 0.0;
        OUTPUT_STATE(source_meta[i]) = ZERO;
        OUTPUT_STRENGTH(source_meta[i]) = HI_IMPEDANCE;
        OUTPUT_DELAY(source_meta[i]) = 0.0;
    }

    status = sr_alb_runtime_advance(
        runtime, TIME, inputs, n_in,
        output_loads, ANALYSIS == TRANSIENT ? 1 : 0,
        &events, &count, &next_time,
        error, sizeof(error));
    if (status != 0) {
        cm_message_printf("ERROR: sr_alb_stateful: %s", error);
        return;
    }
    for (j = 0; j < count; ++j) {
        const struct sr_alb_event *event = &events[j];
        Digital_State_t state;
        Digital_Strength_t strength;
        if (event->pin >= n_out) {
            cm_message_send("ERROR: sr_alb_stateful returned out-of-range output pin");
            return;
        }
        if (event->time_s < TIME - 1e-14) {
            cm_message_printf(
                "ERROR: sr_alb_stateful missed event %.12g at %.12g",
                event->time_s, TIME);
            return;
        }
        state = event->state == 1 ? ONE : event->state == 0 ? ZERO : UNKNOWN;
        strength = event->strength == 0 ? STRONG :
                   event->strength == 1 ? HI_IMPEDANCE : UNDETERMINED;
        OUTPUT_STATE(out[event->pin]) = state;
        OUTPUT_STRENGTH(out[event->pin]) = strength;
        OUTPUT_DELAY(out[event->pin]) = 0.0;
        OUTPUT_CHANGED(out[event->pin]) = TRUE;
    }
    if (isfinite(next_time) && next_time > TIME)
        cm_event_queue(next_time);
}
