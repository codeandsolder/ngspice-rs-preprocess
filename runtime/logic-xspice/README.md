# Event-based logic runtime for ngspice 47

The SIMPLIS translator emits XSPICE models for numbered AND/OR/NAND/NOR
gates, DIGITAL1 AND/OR and S-R latches, and classic SRFF. This runtime is
independent of the experimental ALB runtime. Translation results advertise
the required code-model library through `required_code_models`. The unified
preparation API and version-2 JSON manifest report the per-artifact requirements
and their bundle-wide union. A native engine can bind the same schemas to
typed kernels rather than loading the C module. The module is the independent
C ngspice reference backend; it does not by itself provide native-engine
model binding.

Build against the configured in-tree **matching** ngspice source tree, including
its CIDER/KLU/XSPICE configuration and built cmpp. Code-model ABI mismatches
can crash the simulator. Use the managed compiler wrapper:

    bash runtime/logic-xspice/build.sh /path/to/ngspice-build /path/to/output

Load the resulting module before parsing any translated deck (for example,
put the following command in the simulation working directory's .spiceinit):

    codemodel /path/to/output/ngspice_preprocess_logic.cm

The source CI builds both the pinned simulator and this module together
using the same managed compiler wrapper, including the cmpp scanner restart
fix for consecutive interface specifications. The reference regression
examples accept the module path (or its directory for a linked bundle)
explicitly:

    cargo +nightly run --locked -p model-translate-simplis --example logic_ngspice -- /path/to/output/ngspice_preprocess_logic.cm
    cargo +nightly run --locked -p ngspice-rs-preprocess --example logic_bundle_ngspice -- /path/to/output

The hybrid gate detects threshold crossings from converged analog samples
and requests a simulator breakpoint. Inertial deadlines are analog
breakpoints: a due transition is qualified against converged inputs before
committing, including a pulse whose width equals its propagation delay.
Event scheduling and state storage use the simulator's rollback-aware
interfaces. Classic gates and SRFF propagate
transport events; DIGITAL1 devices cancel pending transitions if the input
condition ceases to hold. Inertial latches evaluate against committed Q.
The output bridge is ideal, with the specified output resistance represented
by a separate resistor. Complemented outputs use the same scheduled state.
No artificial minimum propagation delay is imposed. Analog crossing
precision remains subject to the integrator's timestep and breakpoint
resolution.

The event-model operation identifiers are 0 AND, 1 OR, 2 NAND, 3 NOR,
4 reset-dominant latch, 5 set-dominant latch, and 6 hold-on-equal latch.

The same event model accepts a single receiver for strict hysteretic
SIMPLIS switch state. The open/closed initial hint is used only within the
threshold band; control values outside it determine the initial state.
The analog voltage bridge represents the state as an exact 0 or 1 for
the branch current law. Simple switches preserve their specified RON
and ROFF, while transistor switches retain their LEVEL=1 or LEVEL=2
current law and support current as well as voltage control.

Timed SIMetrix VSWITCH (the VT form) uses the separate
`preprocess_timed_switch` model. Its differential voltage input has no
implicit loading; the translator emits the specified GIN conductance.
The branch has conductance

    G(x) = exp(-log(ROFF) + x*x*(3 - 2*x)*(log(ROFF) - log(RON)))

for phase x in [0,1], with exact reciprocal resistance at either endpoint.
The phase moves at 1/TON or -1/TOFF. A stable endpoint starts moving only
after the corresponding TD_ON or TD_OFF deadline; a threshold reversal
during the ramp reverses it immediately without another delay. Equal
pulse width and delay cancels the pending transition. A reversal exactly
at ramp completion still belongs to the active ramp. DC initializes from
the control and IC hint, and small-signal analysis freezes that state.
Accepted analog history holds the phase anchor, requested direction and
pending deadline so rejected solver trials cannot commit transitions.

TIMETOL defaults to 10 ps and accepts values of at least 1 fs. It controls
crossing localization, not the propagation delay or slew duration.
Threshold crossings and ramp endpoints request solver breakpoints;
actual localization remains limited by solver resolution. RON, ROFF,
TON and TOFF must be positive, and delays nonnegative. Subcircuit
parameter expressions are retained in the emitted model parameters.
