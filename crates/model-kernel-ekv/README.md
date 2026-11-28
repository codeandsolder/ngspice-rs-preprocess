# EKV 2.6 physical kernel

The producer provides the charge-conserving quasistatic intrinsic revision-15
EKV equations and an explicitly composed four-terminal device. The device
artifact adds only the positive series resistances as hidden electrical
nodes; junction and overlap storage, channel and junction residuals, their
analytic Jacobians, trial voltage limits and noise use the shared kernel ABI.

## Source selection and preparation

PSpice level 5 is lowered to `pspice_ekv26_nmos` or
`pspice_ekv26_pmos`. Native ngspice NMOS/PMOS level 5 is BSIM2 and never
selects this kernel. The source normalizer retains parameter expressions;
`ekv_card::ekv_resolved` accepts a caller's lexical scalar resolver.
`PspiceModel::compile` consumes those numerical model parameters and circuit
defaults, then `bind` resolves one occurrence's dimensions, multipliers,
junction geometry, sheet and contact resistances and operating temperature.
`BoundDevice::artifact` creates the shared `ekv26` artifact; `slots` gives
its four geometry parameters. Supply the analysis ambient temperature to
the artifact, not a preadjusted model temperature: the artifact itself
applies absolute or relative temperature once. Geometry is structural
because its dependent parasitics are already resolved; rebind on a change.

The source defaults and process derivations follow the PSpice Reference Guide,
pp. 174–189 and the EPFL EKV 2.6 revision-II report, sections 5.2–5.5.
Doping is in cm^-3 and mobility in cm²/Vs; temperatures on model cards are
in Celsius, with physical preparation in kelvin. Explicit physical
parameters override their process-derived defaults. PMOS threshold,
flatband and threshold-temperature values retain signed physical polarity.
HDIF-based diffusion defaults preserve explicit zero and use finger
parity when geometry is absent. SATLIM is solely an operating-point
classification threshold.

## Qualification boundaries

The publicly available Verilog-A reference is revision 15, with its own
defaults, smooth transitions and an extended junction law. It is not a
PSpice binary. The numerical fixtures match explicit reference parameters,
and compare the full device with independent ngspice diode, resistance and
capacitance primitives. They do not prove bit-for-bit proprietary PSpice
version equivalence. The documented PSpice quasistatic model is supported;
nonquasistatic and non-charge-conserving capacitance-only selectors, and
statistical matching without a realization policy, fail explicitly.
NLEV is unused for EKV's own noise law; SATLIM cannot alter residuals.
Higher-order full-device Taylor coefficients are not advertised.

Sources:

- [EPFL EKV 2.6 report](https://www.wrcad.com/xictools/docs/model_docs/ekv-2.6/ekv_v262.pdf)
- [PSpice Reference Guide](https://rincon-mora.gatech.edu/spice/pspice.pdf)
- [Pinned Verilog-A source](https://github.com/ekv26/model/tree/137e7779c66d282113ce5802843f546869cb8c05)

See the fixture README for independently executable reference decks and
reproduction instructions, and NOTICE for attribution.
