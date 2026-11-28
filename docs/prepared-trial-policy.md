# Prepared-model trial policy (ABI 4)

A prepared artifact declares separate initialization, limiting and convergence
policies. Each explicitly selects engine or kernel ownership. A declared
kernel hook without an implementation is an error. Engine-owned policy does
not assert that the physical equations are zero or that a solve converged.

## Engine lifecycle

The engine owns all time integration, accepted time-step history, raw Newton
iterates, iteration rollback, and global convergence. The kernel owns no
mutable iteration state.

* **Exact:** evaluate the supplied coordinates without initialization or
  limiting (including explicit initial conditions, residual verification,
  AC/noise linearization, and calls with no valid reference history).
* **ColdStart:** for an unconstrained initial operating point, allow the family
  to choose a physically appropriate initial local linearization point.
* **Newton:** the reference is the previous successfully evaluated local
  point, including any limiting already applied. It is not the previous raw
  proposal.
* **Predictor:** the reference is the last accepted step. A rejected time
  step never modifies this reference.

Supply the descriptor-ordered coordinates and a separate caller-owned output
slice. Electrical potentials are volts; thermal coordinates are absolute
kelvin and evaluated temperatures must be positive; internal coordinates
follow the family's physical coordinate definition. Initialization or limiting
may modify the local coordinates but not the raw solver iterate. Failure
invalidates the output. Successful scalar and tile operations allocate no
memory and share the same semantics.

Evaluate F, Q, and their physical Jacobians at the returned local point.
For F the Newton affine stamp is J*x_local - F(x_local), not
J*x_proposed - F(x_local). The integration method supplies the analogous
charge contribution. Never differentiate the limiter itself or change the
physical model to enforce a numerical bound.

A modified point (Initialized or Limited) cannot certify convergence.
Re-evaluate at the exact proposed solution before acceptance. For the optional
family-specific convergence hook, supply the actual previous Newton sample
and exact candidate sample (static F and J, not time-integrated residual).
Absolute tolerances are descriptor-ordered in the residual units (A, W, or
family-defined internal units); relative tolerance is dimensionless. A family
Satisfied result is only one necessary condition: global residual, update,
integration and acceptance checks remain mandatory. Rejected candidates and
failed tile outputs must be discarded by the engine.

## Diode policy

The SIMetrix diode derives logarithmic limiting scales from its prepared
transport, recombination and avalanche exponential terms. Disabled terms do
not constrain the trial. For a term with effective voltage scale a and
current scale I, the forward critical voltage is max(0, a*ln(a/(sqrt(2)*I))),
evaluated using log(I) to avoid underflow. Cold initialization uses the
smallest finite forward critical voltage (or zero if no term is active).
For a large step beyond the critical voltage, use the reference in the
conducting half-plane and compress the signed voltage step using
a*log1p(abs(delta)/a). Choose the most restrictive active term.
For reverse avalanche, apply the same principle to the voltage reflected
about the prepared breakdown knee. This is a numerical policy, inspired by
classical SPICE PN-junction limiting, not a claim of identical ngspice
DEVpnjlim behavior.

The cathode potential and the external anode (when a separate series
resistor exists) are retained; only the intrinsic anode potential changes.
Current, charge and derivatives remain the exact physical model values at
the local point. The additional convergence test compares the cathode
current predicted by the previous static Jacobian with its actual value at
the exact candidate. This row contains only the intrinsic junction, even
when a series resistor is present. The comparison scales both quantities to
avoid floating-point overflow and rejects nonrepresentable predictions.

Individual tile lanes use the same policy and prepared data as scalar and
full-tile evaluation, including after selective replacement. Other families
without specialized hooks explicitly select engine ownership.

The tests use an independent monotone load-line bisection of a Shockley
junction with external resistor and capacitor, including extreme forward
steps and backward-Euler transients. They also exercise reverse avalanche,
series topology, rejected trials, and scalar/tile equivalence.

An opt-in C-reference test uses ngspice on the tracked
`trial-load-line.cir` and `trial-transient.cir` fixtures (1000 V extreme DC
bias; ramp from -2 to 1000 V and back with external capacitance). It
compares the accepted DC point and six transient observations. DC tolerance
accounts for the historical physical constants; transient tolerance also
accounts for fixed backward-Euler versus adaptive Gear integration.


## VDMOS policy

VDMOS declares kernel-owned limiting; cold initialization and the additional
convergence test remain engine-owned. The trial reference must be the actual
successfully evaluated point (previous Newton or last accepted step). The
kernel first bounds junction-temperature changes with the ngspice 47
`DEVlimitlog` 10 K threshold. Where the proposed temperature crosses the
physical junction-potential domain, the step is contracted toward the valid
reference until the potential is positive and finite. A nonpositive case
temperature is similarly moved toward the previous valid case coordinate.
The exact model still rejects invalid physical inputs; no final temperature
is clamped and a limited point never certifies convergence.

The FET gate and drain limiting regions follow `DEVfetlim` and `DEVlimvds`,
selecting the conducting end from the previous polarity-normalized drain
bias. Forward and reflected avalanche limiting share the diode's log-domain
exponential compression and use the VDMOS temperature-adjusted saturation
and breakdown laws. Rather than independently assign incompatible branch
voltages when terminal coordinates coalesce, select the most restrictive
fraction along the proposed electrical direction. Keep the proposed intrinsic
source common-mode potential and interpolate all electrical differences
relative to it. This preserves coherent voltages for arbitrary RD/RS/RG/RB
alias combinations. The internal recovery coordinate remains untouched.

The producer tests cover both polarities, all 16 series-node combinations,
with and without self-heating, extreme forward and reverse proposals,
near-boundary thermal steps, unchanged exact steps, common-mode invariance,
rejected predictor retries, and bit-identical scalar and tile evaluation.
The independent full converter is the solver conformance fixture in
ngspice-rs-fast#259; qualification requires the accepted ABI4 consumer #246.
