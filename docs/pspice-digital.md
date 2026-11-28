# PSpice digital and delayed-switch source declarations

This source-only contract is developed in
[preprocessor #65](https://github.com/codeandsolder/ngspice-rs-preprocess/issues/65).
It is not a claim that digital PSpice circuits can already be executed
by the native engine.

## Source and binding boundary

The PSpice A/D Reference Guide, product version 10.5, July 2005
([reference](https://studylib.net/doc/18584791/pspice-a-d-reference-guide)),
chapter 3, pp. 335–365, defines the qualified primitive and timing-model
syntax. Standard and compound gates, arrays, tristate gates, edge-triggered
and dual-edge flip-flops, gated latches, pull resistors and transport delay
lines have typed primitive layouts. Supply nodes, shared controls, input
groups and output banks are retained in source order. The lexer preserves
PSpice dollar-prefixed nodes while native/HSPICE dollar comments retain
their ordinary meaning.

`model_normalize_ngspice::pspice::digital_source::capture_linked`
captures both digital and delayed-threshold switch declarations in the
admitted source graph. The declaration, its include visit, lexical
subcircuit scope, and every base-to-derived model layer have individual
source identities. Names are resolved lexically, case-insensitively;
ambiguous or missing definitions, incompatible model families and
conditional device topology are rejected rather than guessed. No
vendor-library model is copied into the runtime.

Digital timing binding evaluates only effective setters, in their own
declaration scopes. Propagation and timing-check paths remain distinct.
The reference guide pp. 338–340 specifies the separate omitted-corner
rules. Propagation extrapolation uses DIGMNTYSCALE and DIGTYMXSCALE
(default 0.4 and 1.6); timing constraints use the documented zero,
specified-typical/minimum, and midpoint rules. Explicit zero differs
from omission. Partial UDLY corners require independent qualification:
the documented TP rule does not establish their behavior. Worst-case
selection retains the minimum and maximum endpoints, not a nominal
delay. The occurrence selector MNTYMXDLY delegates to DIGMNTYMX when
absent or zero.

I/O binding retains source units: ohms, farads and seconds. IO_LEVEL
selects AtoD/DtoA references (delegating to DIGIOLVL when absent or zero).
The effective UIO fields are resolved in model declaration scopes and
use the defaults from pp. 417–419. Omitted TPWRT remains dependent on
the actual propagation path. Explicit zero is a value, not a default
marker. The selected interface and supply subcircuits remain references
with source locations, not instantiated bridges. Unselected interface
fields need not be evaluated.

The consumer must elaborate occurrences, bind physical interfaces and
supply nodes, apply circuit options, derive drive strengths and charge
storage from the electrical topology, account for output loading,
and compensate interface-node output timing using the applicable
switching time. None of those operations is replaced by a single
logic threshold or an arbitrary voltage source. Sequential initialization,
unknown-state handling, timing checks, transport versus inertial
scheduling and rollback belong to the transactional event engine.
See [solver #255](https://github.com/codeandsolder/ngspice-rs-fast/issues/255).

## Delayed short-transition switches

The same capture pass preserves short-transition switches with nonzero
or expression-valued TD. Voltage control retains positive and negative
control nodes and requires the source GMIN shunt; current control
retains the signed voltage-source branch reference. RON/ROFF,
VT/VH or IT/IH and TD bind from their effective lexical setters with
the documented defaults. Positive resistance and nonnegative delay
are validated without clamping the signed control values.

The reference guide pp. 295–299 and 311–316 distinguishes a short
transition shifted by TD from continuous log-resistance interpolation.
Threshold comparisons are strict; equality retains state. TD is not
the duration of an analog ramp. The engine must bind the native
instantaneous delayed law with transactional state and qualified
pending-event behavior. The reference does not establish cancellation
on recrossing, and an independent vendor witness is required for it.
See [solver #254](https://github.com/codeandsolder/ngspice-rs-fast/issues/254).

Source normalization rejects unbound digital devices and delayed
switches explicitly. It neither drops these declarations nor emits
a nominally similar native primitive. The existing qualified static
and continuous switch lowering is independent of this extension.

## Evidence

Unit tests exercise primitive layouts and malformed pin counts, timing
and I/O field recognition, inheritance and shadowing, repeated include
identities, lexical expression binding, corner defaults, explicit zero,
worst-case intervals, differential and branch control orientation, and
dialect-sensitive source lexing. These are independently authored
grammar/binding tests, not independent vendor execution traces. Native
execution acceptance requires the consumer issues above and numerical
or event-trace qualification against an independent simulator.
