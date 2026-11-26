/*
 * Test harness: invoke the actual ngspice BSIM1/2 parameter setters and
 * temperature routines on a standalone model and instance. Compile with a
 * configured ngspice checkout and its headers. No simulator code is bundled.
 * Input: whitespace-separated scope (m/i), keyword and scalar value.
 * Output: size-dependent fields from the original implementation.
 */
#include "ngspice/ngspice.h"
#include "ngspice/cktdefs.h"
#include "bsim1def.h"
#include "bsim2def.h"
#include "ngspice/trandefs.h"
#include "ngspice/const.h"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

extern IFparm B1mPTable[], B2mPTable[], B1pTable[], B2pTable[];
extern int B1mPTSize, B2mPTSize, B1pTSize, B2pTSize;
extern int B1mParam(int, IFvalue *, GENmodel *);
extern int B2mParam(int, IFvalue *, GENmodel *);
extern int B1param(int, IFvalue *, GENinstance *, IFvalue *);
extern int B2param(int, IFvalue *, GENinstance *, IFvalue *);
extern int B1temp(GENmodel *, CKTcircuit *);
extern int B2temp(GENmodel *, CKTcircuit *);

double CONSTvt0 = CONSTboltz * REFTEMP / CHARGE;
extern void B1evaluate(double, double, double, B1instance *, B1model *,
    double *, double *, double *, double *, double *, double *,
    double *, double *, double *, double *, double *, double *,
    double *, double *, double *, double *, double *, double *, CKTcircuit *);
IFfrontEnd *SPfrontEnd;
void *tmalloc(size_t size) { return calloc(1, size); }
void txfree(const void *p) { free((void *)p); }

static const IFparm *find(const IFparm *table, int size, const char *name) {
    for (int i = 0; i < size; ++i)
        if (strcmp(table[i].keyword, name) == 0) return &table[i];
    return NULL;
}

static int inputs(int family, GENmodel *model, GENinstance *instance) {
    char scope, keyword[128];
    double scalar;
    int read;
    while ((read = scanf(" %c %127s %lf", &scope, keyword, &scalar)) == 3) {
        const IFparm *spec = NULL;
        if (scope == 'm')
            spec = family == 1 ? find(B1mPTable, B1mPTSize, keyword)
                               : find(B2mPTable, B2mPTSize, keyword);
        else if (scope == 'i')
            spec = family == 1 ? find(B1pTable, B1pTSize, keyword)
                               : find(B2pTable, B2pTSize, keyword);
        if (!spec) { fprintf(stderr, "Unknown input: %c %s\n", scope, keyword); return 1; }
        IFvalue v = {0};
        switch (spec->dataType & IF_VARTYPES) {
            case IF_REAL: v.rValue = scalar; break;
            case IF_FLAG: v.iValue = scalar != 0.; break;
            default: fprintf(stderr, "Unsupported scalar input %s\n", keyword); return 1;
        }
        int result;
        if (scope == 'm')
            result = family == 1 ? B1mParam(spec->id, &v, model)
                                 : B2mParam(spec->id, &v, model);
        else
            result = family == 1 ? B1param(spec->id, &v, instance, NULL)
                                 : B2param(spec->id, &v, instance, NULL);
        if (result) { fprintf(stderr, "Setter failed: %s\n", keyword); return 1; }
    }
    return read == EOF ? 0 : 1;
}

#define OUT(name, value) printf("%s %.17g\n", name, (double)(value))

static int bsim1(int argc, char **argv) {
    B1model model = {0};
    B1instance device = {0};
    model.gen.GENinstances = (GENinstance *)&device;
    if (inputs(1, (GENmodel *)&model, (GENinstance *)&device)) return 1;
    if (B1temp((GENmodel *)&model, NULL)) return 1;
    if (argc == 5) {
        CKTcircuit ckt = {0};
        ckt.CKTmode = MODEAC;
        double drain = strtod(argv[2], NULL), gate = strtod(argv[3], NULL), bulk = strtod(argv[4], NULL);
        double gm, gds, gmbs, qg, qb, qd, caps[9], id, von, sat;
        B1evaluate(drain, bulk, gate, &device, &model, &gm, &gds, &gmbs,
            &qg, &qb, &qd, &caps[0], &caps[1], &caps[2], &caps[3], &caps[4],
            &caps[5], &caps[6], &caps[7], &caps[8], &id, &von, &sat, &ckt);
        printf("%.17g %.17g %.17g %.17g %.17g %.17g %.17g %.17g %.17g\n",
            drain, gate, bulk, id, qd, qg, qb, von, sat);
        return 0;
    }
    OUT("oxide_density", model.B1Cox * 1e4);
    OUT("oxide_charge", model.B1Cox * (device.B1l - model.B1deltaL * 1e-6) * (device.B1w - model.B1deltaW * 1e-6) * 1e4);
    OUT("threshold", device.B1vt0);
    OUT("drain_conductance", device.B1drainConductance);
    OUT("source_conductance", device.B1sourceConductance);
    OUT("overlap_drain", device.B1GDoverlapCap);
    OUT("overlap_source", device.B1GSoverlapCap);
    OUT("overlap_bulk", device.B1GBoverlapCap);
    OUT("bottom_potential", model.B1bulkJctPotential);
    OUT("sidewall_potential", model.B1sidewallJctPotential);
    OUT("vfb", device.B1vfb);
    OUT("phi", device.B1phi);
    OUT("k1", device.B1K1);
    OUT("k2", device.B1K2);
    OUT("eta0", device.B1eta);
    OUT("eta1", device.B1etaB);
    OUT("eta2", device.B1etaD);
    OUT("beta_zero0", device.B1betaZero);
    OUT("beta_zero1", device.B1betaZeroB);
    OUT("beta_vdd0", device.B1betaVdd);
    OUT("beta_vdd1", device.B1betaVddB);
    OUT("beta_vdd2", device.B1betaVddD);
    OUT("ugs0", device.B1ugs);
    OUT("ugs1", device.B1ugsB);
    OUT("uds0", device.B1uds);
    OUT("uds1", device.B1udsB);
    OUT("uds2", device.B1udsD);
    OUT("subthreshold0", device.B1subthSlope);
    OUT("subthreshold1", device.B1subthSlopeB);
    OUT("subthreshold2", device.B1subthSlopeD);
    OUT("vdd", model.B1vdd);
    OUT("partition", model.B1channelChargePartitionFlag);
    return 0;
}

static int bsim2(void) {
    B2model model = {0};
    B2instance device = {0};
    model.gen.GENinstances = (GENinstance *)&device;
    if (inputs(2, (GENmodel *)&model, (GENinstance *)&device)) return 1;
    if (B2temp((GENmodel *)&model, NULL)) return 1;
    struct bsim2SizeDependParam *p = device.pParam;
    OUT("oxide_density", model.B2Cox * 1e4);
    OUT("oxide_charge", p->CoxWL);
    OUT("threshold", p->B2vt0);
    OUT("drain_conductance", device.B2drainConductance);
    OUT("source_conductance", device.B2sourceConductance);
    OUT("overlap_drain", p->B2GDoverlapCap);
    OUT("overlap_source", p->B2GSoverlapCap);
    OUT("overlap_bulk", p->B2GBoverlapCap);
    OUT("bottom_potential", model.B2bulkJctPotential);
    OUT("sidewall_potential", model.B2sidewallJctPotential);
    OUT("vfb", p->B2vfb);
    OUT("phi", p->B2phi);
    OUT("k1", p->B2k1);
    OUT("k2", p->B2k2);
    OUT("eta0", p->B2eta0);
    OUT("eta1", p->B2etaB);
    OUT("beta_zero0", p->B2beta0);
    OUT("beta_zero1", p->B2beta0B);
    OUT("beta_saturation0", p->B2betas0);
    OUT("beta_saturation1", p->B2betasB);
    OUT("beta20", p->B2beta20);
    OUT("beta21", p->B2beta2B);
    OUT("beta22", p->B2beta2G);
    OUT("beta30", p->B2beta30);
    OUT("beta31", p->B2beta3B);
    OUT("beta32", p->B2beta3G);
    OUT("beta40", p->B2beta40);
    OUT("beta41", p->B2beta4B);
    OUT("beta42", p->B2beta4G);
    OUT("ua0", p->B2ua0);
    OUT("ua1", p->B2uaB);
    OUT("ub0", p->B2ub0);
    OUT("ub1", p->B2ubB);
    OUT("u10", p->B2u10);
    OUT("u11", p->B2u1B);
    OUT("u12", p->B2u1D);
    OUT("subthreshold0", p->B2n0);
    OUT("subthreshold1", p->B2nB);
    OUT("subthreshold2", p->B2nD);
    OUT("offset0", p->B2vof0);
    OUT("offset1", p->B2vofB);
    OUT("offset2", p->B2vofD);
    OUT("impact_a0", p->B2ai0);
    OUT("impact_a1", p->B2aiB);
    OUT("impact_b0", p->B2bi0);
    OUT("impact_b1", p->B2biB);
    OUT("gate_high", p->B2vghigh);
    OUT("gate_low", p->B2vglow);
    OUT("thermal_voltage", model.B2Vtm);
    OUT("vdd", model.B2vdd);
    OUT("vgg", model.B2vgg);
    OUT("vbb", model.B2vbb);
    OUT("sqrt_phi", p->SqrtPhi);
    OUT("phi_three_halves", p->Phis3);
    OUT("beta_body_argument", p->Arg);
    free(p);
    return 0;
}

int main(int argc, char **argv) {
    if (argc != 2 && argc != 5) return 2;
    if (strcmp(argv[1], "1") == 0) return bsim1(argc, argv);
    if (argc == 2 && strcmp(argv[1], "2") == 0) return bsim2();
    return 2;
}
