# PSpice polynomial storage

Source-selected CAP and IND models with nonzero or expression-valued VC1/VC2
or IL1/IL2 coefficients emit explicit native-engine model families
`pspice_cap` and `pspice_ind`. They retain the original device name, two
terminal nodes, positional value, IC and model name. The multiplier and
all nonlinear coefficients remain declaration-scoped model parameters;
temperature attributes follow the existing AKO normalization
(T_MEASURED -> TNOM, T_ABS -> TEMP, T_REL_GLOBAL -> DTEMP). The engine
resolves all scalar expressions using the elaborated occurrence and
declaration contexts, then constructs the shared immutable polynomial law.
Only proven literal-zero nonlinear coefficients take the existing ordinary
linear model path. Native SPICE cards are unaffected.

References: Cadence PSpice A/D Reference Guide, capacitor and inductor
model parameters and value formulas; Cadence application note
[Modeling Voltage-Controlled Resistors and Capacitors in PSpice](https://resources.pcb.cadence.com/pspice-application-notes/pspice-modeling-voltage-controlled-resistors-and-capacitors-in-pspice)
explicitly defines C(V) as dQ/dV and identifies the polynomial as the
standard VC1/VC2 model. The same differential inductance convention gives
V = dPhi/dt for the current-dependent inductor.

For signed x = V(p)-V(n) or current from p to n, and effective base B:

    B = value * model_multiplier * (1 + TC1*dT + TC2*dT^2)
    D(x) = B * (1 + a*x + b*x^2)
    S(x) = B * (x + a*x^2/2 + b*x^3/3)
    E(x) = B * (x^2/2 + a*x^3/3 + b*x^4/4)

S is charge or flux linkage, D its derivative, E stored energy relative to
zero. Do not substitute D(x)*x for storage; the derivative would be wrong.
Use S(IC) for initial storage; retain the signed voltage/current as the
initial coordinate. Capacitors are open at static equilibrium, inductors
short; AC uses D at the equilibrium (j*omega*D admittance/impedance).
Transient integration uses differences in S and its exact tangent. Negative
effective values and zero crossings are retained rather than clamped;
nonfinite values and raw zero positional values are invalid. No noise
generator is introduced. This covers the uncoupled polynomial model;
magnetic-core/hysteresis coupling requires its own source-selected law.

The shared `passive_storage::PolynomialStorage` implements S, D and E,
independent of solver stamping or numerical integration. It takes the
effective base from the existing passive temperature/geometry preparation.
The engine must register these explicit model families and use its
physical branch/charge state rather than lower to a behavioral coefficient.
The typed prepared deck requires that native engine support; legacy C
ngspice is not an oracle for these source models.
