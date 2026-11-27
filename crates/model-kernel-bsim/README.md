# Prepared Berkeley BSIM kernels

The BSIM1/2 artifact composes the independently validated model and intrinsic
channel in `model-compact-intrinsic` with body junction current and charge,
overlap storage, drain/source sheet resistance, and the four independent
noise contributions defined by ngspice 47. See [legacy BSIM ownership and
validation](../../docs/legacy-bsim.md).

The consumer supplies the content fingerprint of the model and topology,
physical numeric instance slots (L/W, multiplicity, diffusion geometry,
sheet-resistance squares, gmin), and ambient Kelvin. All model and instance
preparation occurs outside evaluation. The artifact binds a fixed 4–6 row
layout: public drain, gate, source, bulk and optional local drain/source
series nodes. A candidate crossing a series-resistance zero requires a new
artifact and row binding; all other candidate preparation is immutable.
Current/charge values and Jacobians are in SI units, independent of circuit
row numbers and accepted/trial history. The caller owns limiting and
integration.

BSIM1's original constant thermal voltage is preserved for channel and
junction physics; ambient Kelvin is used for thermal noise. Both source
charge partition modes are supported. Jacobians differentiate the actual
constitutive equations, not the original clipped Newton derivatives.
A single conservative charge law serves DC/AC/transient: overlap values
use the source temperature-stage AC/PZ geometry. Historical ngspice
transient overlap stamping uses a different drawn/corrected geometry;
it is deliberately not reproduced as an analysis-dependent storage law.

BSIM2 uses a distinct intrinsic law with the same generalized branch assembly. Its channel temperature follows the historical model TEMP convention while junction current uses the fixed legacy reference thermal voltage.
No BSIM3 revision is substituted, and no existing native solver physics
is moved by this package.
The junction regularization is included exactly once. Noise uses the physical
current and gate derivative rather than the native state-slot indices.
Independent DC references and finite differences validate these decisions.

## BSIMSOI 4.4 prepared assembly

The `soi::BsimSoiGroup` API binds ordered source instances to an immutable
prepared model. Each selected artifact binds the supplied terminals in source
order and allocates only the active hidden roles. Every candidate replays the
entire ordered model group, including the shared model mutations and CV oxide
cache; instance parameter names use `i{index}_{source_name}`. Numeric
geometry slots are expressed in physical SI units. Source flags, initial
condition vectors and original parameter presence remain intact. Structural
changes require a new artifact. A consumer changing a group candidate must
prepare all affected instances from the same parameter vector.

The residual and storage assembly uses physical electrical coordinate
differences and absolute-Kelvin thermal coordinates. Channel, neutral-body,
junction, leakage, gate tunneling, contact, access and RF paths have explicit
conservative incidence. Intrinsic and overlap charge derivatives use their
respective gate coordinates; distributed junctions use their own local body
nodes. Heat balance and stored thermal energy occupy the thermal row. Noise
sources are independent PSDs with explicit terminal-pair injections. No
integration history or heap allocation belongs to the Newton evaluation.

Executable tests qualify candidate replay and rollback, conservative Jacobians
across 576 topology/gradient cases, and a 336-record ngspice 47 clamped DC/AC
reference. The raw reference is retained. For AC entries affected by the
native omitted intrinsic gate/backgate capacitance or CAPMOD2 bulk thermal
derivative, the independent stored-charge finite differences qualify the
physical Jacobian at the original tolerance. The separate 120-record
intrinsic gradient fixture and generator document that correction. Native
DC distributed-body and reverse gate-current Jacobian omissions are likewise
not copied into the physical conservative assembly.
