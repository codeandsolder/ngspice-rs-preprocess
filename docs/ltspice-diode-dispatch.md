# LTspice diode source dispatch

Every LTspice `.model ... D` declaration is emitted with the family marker
`ltspice_diode`. This replaces the previous ideal-only marker. The model
name, instance references, ordered parameter setters, source expressions,
continuation mapping, and lexical scope are retained. Native SPICE input
does not receive this rewrite.

The marker describes the source dialect, not a physical constitutive law.
Independent LTspice 24.0.12 AC witnesses show that the last resolved LEVEL
setter selects a Fowler–Nordheim diode for LEVEL in [1.5, 2.5), regardless
of the presence of the ideal-family selectors Ron, Roff, Vfwd, Vrev, Rrev.
Outside that interval the base family is selected by those selectors.
A single local model with LEVEL={sel} instantiated with sel=1,2,3 selects
ideal/Fowler–Nordheim/ideal respectively when Ron is present. At 2 V and
1 kHz, the measured terminal admittance is 0.500000000000001 S for the
ideal family (Ron=2, Roff=100, Vfwd=1), or
2.0058998418824741e-14 + j21.696674529336967 S for the default FN family.
Duplicate LEVEL setters in opposite order select different families.

Consequently, parsing a literal LEVEL or recognizing ideal selectors during
normalization is insufficient. The consumer must resolve the actual model
occurrence, then dispatch to its physical family and validate its setters.
Limit/epsilon-only cards likewise retain their source setters; normalization
does not assert that their ordinary-diode electrical semantics are supported.

The solver pins the producer revision and changes its family parser and
dispatch together with [solver #258](https://github.com/codeandsolder/ngspice-rs-fast/issues/258).
This marker is not a native ngspice model name, nor a compatibility alias.
