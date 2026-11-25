#ifndef NGSPICE_PREPROCESS_LOAD_DELAY_H
#define NGSPICE_PREPROCESS_LOAD_DELAY_H

/* A bridge can carry a negative digital load to compensate for its own
 * analog edge time. That compensation may exceed the driver's intrinsic
 * propagation delay; an XSPICE digital event cannot be scheduled in the
 * past, so saturate the effective delay at the earliest legal instant.
 * Preserve the signed load in the calculation rather than discarding it. */
static double sr_effective_load_delay(double delay, double out_res, double load)
{
    double effective = delay + out_res * load;
    return effective < 0.0 ? 0.0 : effective;
}

#endif
