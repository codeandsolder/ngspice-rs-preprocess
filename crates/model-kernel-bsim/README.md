# Prepared Berkeley BSIM kernels

The BSIM1 artifact composes the independently validated model and intrinsic
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

BSIM2 has distinct prepared coefficients but is not yet an artifact.
No BSIM3 revision is substituted, and no existing native solver physics
is moved by this package.
The junction regularization is included exactly once. Noise uses the physical
current and gate derivative rather than the native state-slot indices.
Independent DC references and finite differences validate these decisions.
