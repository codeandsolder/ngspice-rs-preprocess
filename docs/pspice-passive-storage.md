# PSpice polynomial storage

Source-selected CAP and IND models with nonzero or expression-valued VC1/VC2
or IL1/IL2 coefficients emit explicit native-engine model families
`pspice_cap` and `pspice_ind`. They retain the original device name, two
terminal nodes, positional value and model name. An explicit device IC is
retained separately in the typed initialization policy. The multiplier and
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
initial coordinate. Except for an active initialization constraint, capacitors are open at
static equilibrium and inductors short; AC uses D at the equilibrium (j*omega*D admittance/impedance).
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

## Device initial conditions

The [Cadence PSpice User's Guide](https://engineering.unt.edu/ee/files/Cadence_PSpice.pdf),
appendix A, p.582, specifies finite initialization sources for device IC
properties. For a signed capacitor terminal voltage target V0 and oriented
inductor current I0, the bias equations are

    i_cap = (V - V0) / 0.002
    i_ind = I0 + 1e-9 * V

They are active only during the applicable small-signal or transient bias
calculation, not the DC sweep, AC perturbation, or transient integration.
The latter uses the integrated physical storage law above. The
[PSpice Reference Guide](https://rincon-mora.gatech.edu/spice/pspice.pdf),
.OPTIONS and .IC, documents NOICTRANSLATE and the distinction from a DC
sweep. NOICTRANSLATE suppresses device bias equations, not skipped-bias
initial coordinates. SKIPBP is normalized to UIC.

Scalar and polynomial C/L use the same typed declaration keyed by emitted
artifact, lexical definition and device name; IC expressions are evaluated
per occurrence. Explicit zero and omission differ: omitted devices have no bias clamp,
but skipped-bias startup uses zero rather than native node hints. The original
IC property is not duplicated as a native parser hint. The policy does not
create permanent helper components. This device-property contract does not
define the separate PSpice .IC command's node-pair and inductor references.
