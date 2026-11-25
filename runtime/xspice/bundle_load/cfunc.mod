void cm_sr_bundle_load(ARGS)
{
    if (INIT) {
        LOAD(state) = PARAM(capacitance);
        LOAD(sink_meta) = PARAM(sink_current);
        LOAD(source_meta) = PARAM(source_current);
    }
}
