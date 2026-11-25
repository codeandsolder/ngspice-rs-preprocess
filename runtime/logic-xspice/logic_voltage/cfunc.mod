void cm_preprocess_logic_voltage(ARGS)
{
    if (INIT)
        LOAD(in) = 0.0;
    if (CALL_TYPE == ANALOG) {
        Digital_State_t bit = INPUT_STATE(in);
        OUTPUT(out) = bit == ONE ? PARAM(out_high) : bit == ZERO ? PARAM(out_low) : (PARAM(out_low) + PARAM(out_high)) / 2.0;
    } else if (CALL_TYPE == EVENT && ANALYSIS == TRANSIENT) {
        cm_analog_set_perm_bkpt(TIME);
    }
}
