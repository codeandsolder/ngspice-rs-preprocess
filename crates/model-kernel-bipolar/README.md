# Native VBIC / HICUM2 compact-model subkernels

This standalone crate implements validated slices of two bipolar-model physics families.
The families have separate physical equations; no Gummel-Poon substitution.

## Prepared HICUM2 model artifact

The native source boundary exposes a solver-facing artifact in the
hicum2::artifact module for ngspice-rs-fast (#183). Model-card ingestion
accepts resolved ordered assignments from the existing SPICE parser and
produces a typed, immutable HICUM2 2.4.0 model with deterministic SHA-256
semantic identity over all 136 source parameters. Actual supplied
source-byte hashes are retained separately as optional provenance.

The instance binder applies AREA/M/TEMP/DT once, preserves OFF/IC solely
as solver initialization, reduces five nominal resistor short constraints
and reserves optional vertical NQS and thermal unknowns. Model/scaled
instance digests and a seven-bit structural key allow correct prepared
kernel reuse. Temperature-specific physical kernels accept borrowed
local trial vectors and return complete isothermal current/charge/
conductance/capacitance matrices, homogeneous caller-owned output
batches, and source-referred noise without per-trial string lookup,
allocation, dynamic dispatch or accepted-step mutation. All three NQS
states are read directly from reserved fixed local unknown indices.

**Capability boundary:** local I/Q/G/C and noise are available;
global DC/AC/transient solver analysis, sparse assembly, time-history,
noise transfer and final convergence remain simulator-owned. A live
self-heating node explicitly requires the remaining full electrothermal
coupling; it is never silently zero-filled. A stale thermal-prepared
kernel is rejected if the local thermal unknown changes. Interface
tests and an on-demand local benchmark are in tests/hicum2_artifact.rs.

## Implemented

- **VBIC 1.1.5 B-E/B-C junction currents:** intrinsic/extrinsic and parasitic
  ideal/nonideal diode laws, WBE branch splitting, optional reverse breakdown,
  and analytic conductances.
- **VBIC intrinsic transport:** forward/reverse minority carrier transfer
  (Ifi, Iri, Itzf, Itzr), high-injection rolloff, forward/reverse
  Early modulation through the qB factor, and both QBM formulations.
  Analytic voltage gradients include B-E/B-C cross derivatives.
- **VBIC normalized depletion integrals:** B-E/B-C qdbe/ qdbc,
  unsmoothed FC continuation, AJE/AJC smoothing, VRT reach-through, and
  ART double smoothing. These *normalized* quantities have units of volts
  and contribute to base-charge modulation even for CJE=CJC=0. They are
  **not** physical stored charge.
- **VBIC parasitic substrate transistor:** ISP/WSP/NFP/IKP-dependent Iccp
  with derivatives wrt Vbep, Vbci, and Vbcp and separate parasitic qB.
- **VBIC B-C avalanche:** native AVC1/AVC2/PC/MC impact-ionization
  generation from the B-C diode and coupled transfer currents, with both
  local current derivatives. The zero-AVC1 branch is exactly the native
  unmodified B-C diode, not a substitute BJT model.
- **VBIC physical charges:** all eight source-named Qbe/Qbex/Qbc/Qbcx/
  Qbep/Qbcp/Qbeo/Qbco branches with analytic self/cross incremental
  capacitances. Includes CJE/CJC/CJEP/CJCP/CCSO/CBEO/CBCO, QCO/GAMM
  epitaxial charge, TR reverse and parasitic forward diffusion, and
  TF/QTF/XTF/VTF/ITF injection-dependent transit charge. Stored charge
  is in coulombs and derivatives in farads; integration remains the
  solver's responsibility.
- **VBIC resistive branches:** external RCX/RBX, emitter RE, substrate RS,
  nonlinear epitaxial collector RCI with VO/GAMM/HRCF velocity saturation,
  intrinsic RBI modulated by qB, and peripheral RBP modulated by qBp.
  All nonlinear branch currents include analytic mixed conductances wrt
  their local voltage and both coupled junction voltages. Parser-level
  minimum resistances/topology reduction remain outside the kernel.
- **VBIC 1.1.5 complete temperature preparation:** all 108 VBIC source
  coefficient slots, mapped by a compile-time enum to typed junction, charge,
  resistance, transport, parasitic, avalanche, and local noise parameters.
  Includes source-exact nominal-ideality saturation scaling, resistance
  exponents, local model DTEMP, temperature-dependent junction potentials
  and depletion capacitances, impact-ionization coefficients, and reverse
  breakdown terms. One immutable preparation per effective temperature;
  no coefficient-name lookup inside nonlinear evaluation. The full
  model-card parser/default schema, device-level DTEMP and geometry/multiplicity
  remain a separate preparation/binding responsibility.
- **VBIC local noise:** all seven thermal-resistance sources, four shot
  sources (Itzf/Ibe/Ibep/Iccp), and two KFN/AFN/BFN flicker sources,
  using the ngspice analysis constants and instance M scaling. The output
  is local source-referred A-squared/Hz; circuit transfer functions and
  integration remain solver-owned.
- **HICUM/L2 2.4.0:** seven distinct diode and recombination branches, with
  reference HICDIO forward exponential continuation and reverse cutoff.
- **HICUM/L2 2.4.0 junction charges:** native `QJMODF` forward-limited and
  `HICJQ` punch-through-smoothed depletion laws for intrinsic/peripheral
  B-E, intrinsic B-C, both external B-C, internal S-C and perimeter S-C
  (including its exact linear-potential-zero mode). Outputs include the
  actual derivative `dQ/dV` and the separately published source-model
  capacitance `C`: the HICUM 2.4.0 `QJMOD` C expression is not always
  the derivative of its own Q expression around reach-through, and
  internal model calculations and solver charge stamps need their
  respective values. Capacitances and charges are physically dimensioned,
  unlike VBIC's normalized early-effect junction integrals.
- **HICUM/L2 2.4.0 native HICCR transport:** coupled positive total hole
  charge via source GICCR (including i0f/i0r, high-injection ICH),
  HICICK critical-current voltage knee, B-E/B-C charge-dependent Q0,
  the distinct unpunched Cjcit transit capacitance, and source T_f0
  collector-bias modulation. The HICQFF storage law includes forward,
  reverse, base-emitter and collector high-current charges, VCbar/ICbar
  barrier modulation, and both HICQFC lateral width/length formulas.
  Both historical and current FLcomp charge-weighting options are supported
  as physical model selectors. The bounded implicit hole-charge solve uses
  a fixed-size three-axis derivative for Vbiei, Vbici and Qp, computes
  complete cross-voltage current/charge derivatives, and reports explicit
  non-convergence. The model is not stamped as a complete transistor.
- **HICUM/L2 2.4.0 local noise:** all five enabled
  thermal-resistance sources, five non-transport shot sources,
  the separate intrinsic B-E and C-E transport shot sources,
  the *single* combined Ibei+Ibep B-E flicker spectrum with
  CFBE intrinsic/peripheral placement, and the independent RE
  flicker spectrum. Implements FLCONO-correlated base/collector
  shot noise from the source's two auxiliary noise nodes as an
  exact frequency-dependent Hermitian 2x2 source covariance:
  the imaginary cross-spectrum is retained rather than
  incorrectly summing independent PSDs. Independent tests check
  all source coefficients over temperature/frequency sweeps,
  the source's ALIT/ALQF/beta-dependent gain and positive
  semidefiniteness, disabled resistor gates, and all error paths.
  Solver-owned transfer functions and noise integration remain
  separate from these local source spectra.
- **HICUM/L2 2.4.0 vertical NQS/excess-phase:** source-exact
  three-unknown adjunct gyrator network (current stages xf1/xf2 and
  forward diffusion charge xf), with all DC residual currents,
  dynamic charges, independent auxiliary-state Jacobians, intrinsic
  B-E/B-C partial derivatives, and an explicit derivative with
  respect to *bias-dependent* Tf so the solver can chain dTf/dV.
  The inactive branch enforces the source's zero-auxiliary-state
  constraints and returns undelayed Itf and Qdei. Source activation
  includes the legacy FLcomp=0/2.1 override of FLNQS and the
  Tf!=0/ALIT/ALQF gates. Also provides exact analytic complex
  local AC transfer functions for Itxf/Itf and Qdeix/Qdei.
  **176 reproducible ngspice 47 complex AC samples:** all **160**
  in published-source-shared modes match within 3e-8 relative,
  including modern/legacy/disabled/charge-only/current-only
  and high-frequency rolloff. Native ngspice 47 treats
  FLcomp=2.2/FLNQS=0 as legacy NQS-active whereas published
  HICUM 2.4.0 does not; 16 divergence samples enforce explicit
  version-awareness, not false source conformance.
- **HICUM/L2 2.4.0 electrothermal branch:** source FLsh=0/1/2
  power models, including Vciei*It, B-C avalanche generation,
  the five exact diode power terms in full-electrical mode and four
  nonlinear/linear series resistor dissipations. Preserves signed
  I*V power and source 1 mOhm branch-presence thresholds, rather
  than applying all terminal losses to the thermal equation.
  Returns individual source-power derivatives, full thermal
  KCL (rise/Rth(Tdev)-P), its exact rise derivative at fixed
  electrical inputs, and CTH*rise thermal energy and capacitance.
  The shared HICUM nominal preparation now takes **explicit
  thermal-node trial rise**: all temperature-dependent current,
  charge, resistance and noise coefficients are re-prepared at
  the clamped local device temperature. No hidden accepted-step
  state or backward-compatible unheated preparation API.
  **54 native ngspice-47 self-heating DC cases:** 30 non-clamped
  active cases balance generated/cooling power within 2e-7,
  18 disabled cases have zero rise, and six cases reproduce the
  native 600 K saturation with nonzero power-balance residual.
  Those six are explicitly *not* claimed to be thermal-equilibrium
  solutions. Independent finite-difference tests check all
  resistor and temperature partials.
- **Full implicit HICCR electrothermal transport and storage:**
  The fixed-size HICCR Jet now includes a temperature derivative
  lane alongside intrinsic BE/BC and independent hole-charge lanes.
  Source nominal-to-effective preparation computes exact slopes
  for C10, QP0, AHJEI, HJEI0, HF0/HFE/HFC, TEF0, THCS,
  RCI0/VLIM/VCES and T0, across all four FLcomp compatibility
  branches (including legacy smoothing and modern bandgap laws).
  The source QJMODF/QJMOD collector/emitter charge slopes feed
  the same equilibrium minority-charge residual as the electrical
  solver; high injection derives dQp/dT=-dResidual/dT /
  dResidual/dQp from the *actual* implicit root, and low injection
  differentiates the same closed-form initialization. Native
  HICQFF/HICQFC source charge/collector spreading equations are
  reused without duplication. Exact d/dTdev now includes Itf,
  Itr, It, Ick, Q0, Qp, Qf, QfT, Qr, Qbf, Ibhrec, Tf0,
  and full actual/weighted forward transit times. The latter use
  a second-order mixed forward AD thermal axis to compute
  dTf/dT=d²Qf/(dItf dT), including implicit Itf(T), Ick(T),
  Tf0(T) and the explicit HICQFF coefficient temperature paths,
  while preserving distinct low-injection/zero-storage source
  weighting gates. The full prepared device reuses the resulting
  dItf/dT to complete the formerly missing source HICAVL
  generated-current electrothermal derivative. **8,064
  independent full temperature re-preparation comparisons** verify
  all fourteen current/charge/transit-time slopes over FLcomp
  0/2.1/2.2/2.3, four charge/geometry/storage variants, three
  temperatures and twelve source voltage points (0.5% relative
  tolerance with separate physical-unit absolute floors).
  Additional NPN/PNP full-device checks independently verify
  combined HICCR Itf/Qp/Qf and HICAVL gain/current slopes.
  Nonlinear RBI and remaining physical branch temperature
  coupling still need assembly into complete global thermal KCL.
- **Exact HICCR critical-current knee and low-current delay thermal
  derivatives:** HICICK now uses one static scalar source equation
  instantiated by its existing three-axis HICCR voltage/implicit-hole
  Jet and a first-order temperature Dual. Its exact dIck/dTdev
  includes native RCI0, VCES, VLIM/velocity rolloff, source knee
  smoothing and VT temperature dependence for both legacy
  FLcomp=0/2.1 smoothed velocity and modern FLcomp>=2.2 laws.
  Nominal-to-effective preparation computes source d(RCI0)/dT,
  d(VCES)/dT, d(T0)/dT and d(VLIM)/dT exactly once, including
  the original legacy softplus limit. HICCR's dTf0/dTdev
  separately differentiates *unpunched* source Cjcit(T) from the
  native QJMODF law, the Cjci0(T) reference ratio, T0(T), DT0H
  and TBVL terms (and exactly handles disabled collector Cj0).
  New tests verify 1,408 independent current/time/coefficient
  temperature derivatives across all four FLcomp modes,
  four collector-geometry/velocity variants, four temperatures,
  and six bias points; a device-level NPN/PNP binding test checks
  the same physical source quantities. The implicit transport
  Itf/Qf thermal sensitivities remain separate mandatory
  couplings, not zero-filled.
- **Exact source HICAVL avalanche electrothermal gain:**
  Shared one-lane source forward differentiation now supplies the
  generic scalar used by both QJMOD punch-through and native HICAVL
  source gain. HICAVL uses exactly the same f64 source gain/gradient
  equations as before and differentiates the complete weak/strong
  avalanche law with respect to effective Tdev, including FAVL,
  QAVL, KAVL source temperature exponentials and both the effective
  intrinsic collector Cjci0(T) and explicitly published Cjci(T)
  from QJMOD. The API exposes exact gain d/dT at fixed forward
  transport current and completes generated-current dIavl/dT
  *only* with caller-supplied dItf/dT from the transport kernel;
  missing coupling is not silently replaced by zero. 3,240
  gain/current temperature-derivative comparisons use independent
  full source temperature re-preparations, including an independently
  changed forward current and strong avalanche correction.
  Device-level NPN/PNP gain-temperature binding matches standalone
  prepared physical source kernels.
- **Complete seven-junction QJMODF/QJMOD electrothermal derivatives:**
  Source reach-through QJMOD collector depletion calculations now
  execute through one statically specialized scalar law: f64 at
  ordinary physical bias, or one-lane forward automatic
  differentiation for d/dTdev. The analytic derivative tracks
  prepared Cj0(Tdev), Vd(Tdev), VT(Tdev), forward/reverse smoothing,
  reach-through gain, the source integral and the two deliberately
  distinct capacitance outputs. Unpunched collector junctions
  instead reuse the existing exact QJMODF forward-junction
  sensitivity, while fully disabled/linear peripheral substrate
  branches remain exact zero or linear source terms. All seven
  junction charge/capacitance temperature derivatives are now
  exposed together through both junction and device prepared
  kernels with no runtime finite differences. The new independent
  2,880-value re-preparation regression covers five
  collector/substrate physical branches, six topology/reach-through
  variants, four temperatures and eight source voltage ranges,
  separately comparing dQ/dT, d(stamped farads)/dT and
  d(published source C)/dT. Tests explicitly require differences
  in the last two where native reach-through semantics demand it.
  Device integration for NPN and PNP is checked independently.
- **Exact source HICTUN tunneling electrothermal derivative:**
  Native source IBETS(Tdev) and ABET(Tdev) logarithmic slopes are
  derived once from their effective emitter Cj0/Vd, effective
  semiconductor bandgap and the source 2.4.0 tunneling preparation,
  retaining zero-saturation and zero-capacitance gates. The tunneling
  current's complete dIbet/dTdev uses these coefficients together
  with the exact QJMODF source-capacitance slope at the selected
  intrinsic or peripheral B-E junction. The outer reverse-bias gate
  still considers *both* junctions, even if the chosen tunneling
  branch is forward biased. Validation independently re-prepares
  all source families at perturbed temperatures for 1,024 HICTUN
  current-temperature comparisons, covering both tunneling
  locations, FLCOMP compatibility, zero IBETS/Cj0, zero and finite
  attenuation, four temperatures, and mixed-sign reverse/forward
  voltages. The prepared source device exposes matching
  NPN/PNP-normalized tunneling temperature sensitivity, checked
  against independent standalone physical kernels.
- **Exact TMPHICJ coefficient and QJMODF emitter thermal derivatives:**
  The one source TMPHICJ transformation now differentiates its own
  effective junction potentials, zero-bias capacitances and forward
  maximum-C/C0 ratios (or constant collector reach-through selectors).
  All seven effective junction partitions, including both separately
  apportioned external collector capacitors, inherit these once-prepared
  derivatives. QJMODF emitter current/charge preparation uses the exact
  forward smoothing and integral intermediates to derive both
  intrinsic and peripheral emitter dQj/dTdev and dCj/dTdev at
  fixed normalized junction voltage, rather than finite differences
  or temperature-independent capacitor approximations. A source
  junction with zero Cj0 has exactly zero derivatives. Comparison
  against independent model re-preparation covers 420 coefficient
  temperature slopes and 576 source charge/capacitance derivatives,
  including reduced effective max-cap ratios. Collector reach-through,
  avalanche, and tunneling full derivative chains remain separately
  required until their complete physical families are evaluated.
- **Exact substrate-transistor electrothermal current and charge:**
  Source HSI_Tsu and Qdsu share ITSS(Tdev), ideality MSF and
  TSF(Tdev). Their exact temperature slopes preserve the source
  legacy (FLcomp 0/2.1) and modern (2.2 and >=2.3) saturation laws,
  and dln(TSF)/dTdev=(ZETACX-1)/Tdev. At fixed normalized voltages,
  the current slope combines I*dln(ITSS)/dTdev with the two independent
  -(V/Tdev)*dI/dV terms; the physical diffusion-charge slope uses
  Q*(dln ITSS+dln TSF) minus its own forward branch voltage term.
  Native source limexp continuation at exponent 80, zero-current and
  zero-storage branches are preserved. The full device exposes
  substrate temperature sensitivity with correct NPN/PNP orientation.
  360 independent current plus 360 independently re-prepared charge
  slopes verify all FLcomp branches, three temperatures, varied
  current/storage factors and reverse/forward-limited voltage pairs.
  Constructor rejects nonfinite source temperature slopes.
- **Exact nonlinear intrinsic-base HICRBI electrothermal coupling:**
  Shared source-temperature preparation passes analytic dRBI0/dTdev
  from the ZETARBI/Tdev power law and dQP0/dTdev from the same
  temperature-dependent transport/junction HICCR source preparation.
  The HICRBI resistance, internal resistor current and HF Qrbi shunt
  preserve the coupled emitter/collector depletion charge and
  explicitly reported source capacitance slopes, forward HICCR
  diffusion charge and transfer currents, HICDIO intrinsic emitter
  current slope, and temperature dependence of the source thermal
  voltage. One source Jet calculation handles all physical nonlinear
  RBI crowding, FQI partition, HF shunt and source derivative lanes.
  The full prepared device exposes the coupled source-temperature
  sensitivity; fixed-voltage FLSH=2 resistor-power derivative follows
  Vbpbi*dIrbi/dTdev. **486 independent full-device temperature
  re-preparations** validate all five RBI output sensitivities plus
  its dissipated-power slope across NPN/PNP, FLcomp, injection bias,
  base crowding and three temperatures. Nominally shorted RBI
  branches still produce exactly zero current/charge/slopes.
- **Exact seven-diode HICDIO electrothermal sensitivity:**
  The source nominal-to-effective temperature layer now derives
  all seven logarithmic Is(Tdev) slopes for legacy FLcomp 0/2.1,
  intermediate FLcomp 2.2 and modern >=2.3, including IDEALITY,
  VGB/VGE/VGC/VGS, ZETABET/ZETACI/ZETACX, MCF and ALB.
  The diode kernel combines these slopes with the thermal-voltage
  term dI/dTdev=I*dlnIs/dTdev-(V/Tdev)*G for every physical diode,
  preserving native reverse cutoff and forward exponential
  continuation exactly. Independent source-temperature re-preparation
  verifies **1,008 branch slopes** across all modes, three temperatures,
  partial zero-current families and reverse/forward-extreme voltages.
  The corresponding FLSH=2 diode power derivative includes exactly
  IBEI, IBCI, IBEP, IJBCX and IJSC, excluding IREI/IREP as in published
  source pterm, and passes the same finite differences. The full
  prepared device exposes these exact derivatives with polarity
  normalization. Junction charge/transport/avalanche temperature
  derivatives remain separate required couplings, not zeroed.
- **Exact external-family electrothermal sensitivity:**
  Temperature-prepared HICUM resistor kernels now retain source
  dlnR/dTdev values (ZETARBX/Tdev, ZETARCX/Tdev, ZETARE/Tdev,
  and zero for source-T-independent RSU). At fixed source voltages
  this yields exact dI/dTdev=-I*dlnR/dTdev for *all* four external
  branches, with nominal 1mOhm absent-resistor gates still respected.
  A matching FLSH=2 external dissipation sensitivity produces
  dPext/dTdev from native V*I power, explicitly excluding substrate
  resistance. Device::Prepared exposes this independent external
  sensitivity from the same evaluated source operating point. 81
  independently temperature-reprepared cases across active/shorted
  topologies, 3 exponent sets, 3 temperatures and 3 voltage scalings
  verify all four branch derivatives and FLSH=2 power sensitivity;
  constructor rejects nonfinite source slopes. The other thermal
  power and electrical families remain explicit independent source
  couplings for the full circuit Jacobian, never fabricated zeros.
- **HICUM model-card and Q-instance semantic binding:** a thin
  compilation-time boundary accepts already-parsed ordered SPICE
  scalar/text/flag/initial-vector assignments from the ngspice-rs-fast
  AST, validates explicit Q LEVEL=8 and native NPN/PNP source model
  selectors (including textual TYPE and strict VERSION=2.4.0), and
  materializes the exact 136 source coefficient slots into a typed
  nominal model. TREF is the ngspice TNOM setter alias; duplicate
  source setters apply in original card order, and rejected setters
  return their original assignment index for upstream source-span
  diagnostics. Instance AREA, M, TEMP, DT, OFF and 1–3 IC vector
  components bind onto independently scaled immutable prepared
  instances. The existing netlist parser owns numeric suffixes,
  expression evaluation and positional Q tokens; the physics module
  owns semantic constraints and equations, not a duplicate lexer.
  Seven focused tests cover all 136 slots, real model selectors,
  valid/invalid versions and flags, 1-to-many instance binding,
  source ranges and wrong-kind/unknown-parameter handling.
- **Exact bias-dependent HICUM forward-transit-time Jacobians:**
  The source HICQFF/HICQFC charge implementation now shares one
  generic static forward-mode scalar abstraction between its usual
  first-order HICCR Jet and an on-demand mixed second-order derivative
  type. The latter evaluates exact dTf/dVbiei and dTf/dVbici, including
  implicit Itf transport, bias-dependent Ick, junction-capacitance-
  dependent low-current delay, barrier, emitter-storage and lateral
  current spreading terms, *without* finite differences at Newton
  runtime and *without* duplicated charge formulas. Dominant low-
  injection/no-storage cases directly return the analytic low-current
  delay derivative without second-order work. 528 transport model
  and bias/temperature cases cross-check both derivatives against
  independently perturbed evaluations. New device operating-point
  polarity metadata lets nodal Partials complete its source Tf
  derivative with the correct NPN/PNP normalized-bias orientation
  automatically. Full nodal finite differences check 1,944 electrical
  Jacobian row/column entries over both polarities, three geometry
  families and three temperatures. Explicit electrothermal derivative
  completion remains separately required when FLSH creates a live
  thermal node.
- **End-to-end reduced-node HICUM DC Newton integration:**
  An independent small test-only dense Newton solver consumes the
  actual complete physical HICUM KCL and conductance matrices, with
  source resistor node shorts and source HICCR transport evaluated
  at every trial. External terminals remain fixed while 0–5 internal
  physical nodes solve their actual coupled nonlinear KCL residuals.
  The harness independently checks **72 ngspice-47 LEVEL=8 operating
  points**: four series-resistor topologies plus two non-unit AREA/M
  instance geometry cases, two ambient temperatures, three B-E
  biases and two C-E biases. Verification compares native intrinsic
  Vbiei/Vbici, internal base resistance Vbpbi and externally reported
  collector/base currents, not merely individual source components.
  An additional 54 full DC solves verify that physical PNP solutions
  are exact polarity mirrors of NPN devices (internal node voltages,
  terminal currents and complete reduced conductance matrices)
  across topologies, temperatures and voltage sweeps. Native ngspice
  47 PNP reference decks with these parameters fail their own OP
  convergence (NaN), so PNP is independently cross-validated by
  the source-polarity identity rather than labelled an ngspice
  native-conformance pass. Numerical Newton/damping and dense
  matrix algebra are confined to the standalone test harness.
  Production global sparse/analysis algorithms remain solver-owned.
- **Reduced HICUM physical DC/AC/transient local nodal contract:**
  Source nodal Layout binds 18 oriented branches to immutable 4–9
  electrical node indices, 0/3 vertical NQS auxiliaries and 0/1
  thermal row without allocations or global solver indexing.
  Reduction sums independent source currents, stored charges and
  complete 9-node plus 3-state partial derivatives into fixed-size
  local KCL, dI/dV and dQ/dV matrices. Shorted named nodes are merged
  without stiff resistors; conservation and 32 resistor masks pass
  regression. The explicit complete operation applies the source
  forward transit-time chain rule and requires the electrothermal
  cross-Jacobian, including the thermal power derivative dP/dTdev
  missing from the independently prepared cooling derivative, before
  producing complete local matrices. Missing required couplings
  are errors, not silently treated as zero. Finite differences
  verify complete 12-unknown NQS electrical and 13-unknown thermal
  Jacobians. The actual assembled 3-unknown complex NQS submatrix
  G+jωC is solved independently as a 6x6 real linear system in a
  regression harness; its forward-current and diffusion-charge transfer
  ratios match the published source formula at 5 frequencies from
  DC to 100 GHz, 3 temperatures and FLcomp/FLNQS mode combinations. Unmodified model defaults reserve no idle NQS nodes
  because their five possible delay-generators are all zero;
  models with potentially active delay reserve all three before
  Newton. An active source vertical-NQS network with T0=0 is
  underdetermined even if Tf comes from other transit mechanisms;
  the evaluator now rejects that singular operating point directly,
  matching the existing frequency-domain invalid-domain behavior. Global sparse stamping and accepted-step time integration
  remain the circuit solver responsibility.
- **HICUM/L2 2.4.0 physical branch residual contract:** one
  fully composed source evaluation emits fixed-layout current/charge and
  analytic gradients for all 18 electrical branch incidences, all nine
  named physical node voltages and three vertical-NQS auxiliary states.
  The NQS filter's independent d/dTf chain coefficient is preserved,
  so bias-dependent transit time is not implicitly frozen.
  Source tunneling placement and NPN/PNP current/charge orientation are
  preserved; source Gmin remains the simulator's responsibility.
  Electrothermal output includes all three FLSH modes, the source
  thermal current, CTH charge, 9-node cross-Jacobian including nonlinear
  Rbi derivatives, and independent thermal-rise partial at fixed electrical
  parameters. The solver retains charge integration, thermal dI/dTdev
  coupling, matrix stamping, and accepted/trial state ownership.
  Independent finite differences exercise 18 source branches, the three
  NQS filter equations, thermal power, 9 node voltages, NPN/PNP,
  both tunneling placements and temperature sweeps. The 36 unmodified
  ngspice 47 default operating points are reconstructed by summing
  source physical terminal-branch currents plus solver-owned Gmin.
- **HICUM source physical topology:** source nominal RBX/RBI/RCX/RE/RSU below
  1 mOhm become exact node equalities, reducing nine named electrical
  nodes to 4–9 dense indices without stiff conductances. All 18 source
  branch orientations, the source thermal short, and three potential
  NQS auxiliary unknowns are explicit; all 32 nominal resistance-gate
  combinations are regression-tested. The circuit solver owns stamps.
- **HICUM/L2 2.4.0 complete physical device operating point:**
  `temperature::Prepared::device_kernel()` constructs one immutable
  composite evaluator for all physical source subkernels. It accepts
  absolute b/bp/bi/c/ci/ei/e/si/s node voltages and solver-owned
  three-state NQS unknowns, applies exact source TYPE NPN/PNP
  normalization only to semiconductor junction voltages, and uses
  unnormalized physical voltages for series resistors and
  isolation-capacitor charges. The result includes all HICDIO,
  junction charges, implicit HICCR currents, HICAVL generation,
  substrate/tunnel currents, internal and exterior resistors,
  three NQS residuals, and complete electrothermal generated
  power/KCL. Source-referred noise is evaluated separately at
  frequency using the *same* branch currents and source-defined
  conductances; correlated noise is preserved. Independent
  tests cover 36 unmodified native ngspice 47 OP references,
  mixed high-injection/thermal/NQS/noise operating points at
  three temperatures, exact PNP bias mirroring and current
  orientation, and borrowed batches. **Internal RBI0 topology
  also now uses the published *nominal* 1 mOhm threshold**, not
  merely RBI0=0; temperature-adjusted RBI may cross that threshold
  without changing physical node topology. Node reduction, accepted
  transient integration and solver stamps remain external.
- **HICUM ngspice LEVEL=8 instance preparation:** source 136
  model coefficients remain shared while the ngspice frontend AREA,
  M, device TEMP/DT, OFF and initial VBE/VCE/VCS bind independently.
  The per-instance typed nominal model scales intrinsic transport,
  diodes, junctions, avalanche, tunneling and thermal capacitance by
  AREA*M, low-field resistances and RTH inversely, and perimeter
  currents/charges/parasitic capacitors by M alone. Substrate
  parameters remain unscaled, and noise KF/KFRE scale as M^(1-AF).
  Model thermal DT and instance temperature offsets are additive;
  physical resistor short gates are recomputed after geometry scaling.
  Independent verification covers all 136 coefficients, immutability,
  nonlinear extremes and a **120-point native ngspice-47 AREA/M DC
  oracle** at three temperatures and multiple B-E/B-C biases.
  SPICE parsing and accepted initial state remain outside the kernel.
- **HICUM/L2 2.4.0 complete model-parameter binding schema:**
  all **136** published Verilog-A parameters (130 real, 6 integer)
  have source names, exact default values, numerical ranges with
  open/closed endpoints, integer-only and excluded-zero gates, and a
  stable dense typed slot. Case-insensitive parameter names resolve
  once during model-card binding to ID/value pairs, never at Newton
  evaluation; the external SPICE parser owns number suffixes and syntax.
  Source-specific fields such as TYPE NPN/PNP, CFBE flicker placement,
  TUNODE, FLNQS/FLCONO, FLSH, HJEI/ahjei paired initializers and raw
  unsplit CJCX0 map to the correct typed nominal families. Invalid
  multi-parameter updates are transactional and leave the nominal
  model unchanged; subsequent model preparation validates compound
  physical constraints. Tests assert the exact parity of every
  published default with the typed factory, all source numeric
  boundaries and integer selectors, 136 independent name/value
  round trips, and full PNP/electrothermal/NQS model preparation.
  The source's FLcomp domain is unbounded above (nonnegative), so
  the earlier artificial FLcomp <=3 constraint in vertical NQS
  preparation was removed. Complete deck syntax and per-instance
  geometry/multiplicity binding still belong to translation.
- **HICUM/L2 2.4.0 complete source nominal defaults:** a
  compile-time, typed `temperature::Nominal::source_defaults()`
  factory populates all **136** published Verilog-A model parameters
  across HICCR, diode/junction, temperature, noise/NQS, series,
  parasitic and electrothermal families. The original default
  `FLcomp=0` deliberately selects the source's legacy
  2.1-compatible physical branch; the original transistor `TYPE`
  is represented by explicit NPN/PNP polarity carried into the
  immutable prepared model (the solver binder still owns signed
  physical nodal mapping). Reproducible 36-point native ngspice
  47 `LEVEL=8` comparisons use *no model-card overrides* and
  enforce <1e-4 relative for reported collector terminal current,
  intrinsic B-E/B-C diode currents and HICICK critical current,
  including native `Gmin` branches when comparing terminal
  currents. The published HICUM 2.4.0 source defaults for
  disabled tunneling-source node selection and flicker placement
  differ from ngspice's reported defaults; their inactive
  physical contributions remain source-accurate.
- **HICUM/L2 2.4.0 external series and isolation network:**
  RBX, RCX, RE and RSU source resistor currents/conductances, using
  nominal 1 mOhm thresholds for exact physical short topology even
  when effective temperature-adjusted resistances cross the threshold.
  The RSU parallel Csu charge and capacitance are present only with
  a nominally present RSU. Source CBEpar is partitioned between b-e
  and bp-e, while the original FBCpar-dependent piecewise model
  initialization divides the *combined* CJCX0+CBCpar between
  independent collector depletion and isolation branches on b-ci
  and bp-ci. B-C junction pieces then share one TMPHICJ temperature
  scaling; isolation capacitors remain constant. Includes 576
  source-parameter partition combinations, finite-difference
  capacitor/current Jacobians, nominal threshold sweeps and effective-
  temperature integration with the same resistor conductances used
  in local thermal noise. Exact nodal short merging and stamping
  remain the simulator's responsibility.
- **HICUM/L2 2.4.0 HICAVL impact ionization:** native B-C
  generation source, both weak and strong (KAVL) avalanche laws,
  effective QAVL/FAVL/KAVL from temperature preparation, and full
  analytic B-E/B-C cross derivatives propagated from HICCR Itf
  and the *explicitly published* HICJQ collector capacitance.
  The latter has its own voltage derivative distinct from the
  stored charge second derivative around punch-through.
  The external-base-to-internal-collector charge `qjcx0_t_i`
  uses its own `V(b,ci)` bias, independent of intrinsic
  `V(bi,ci)`; both branches carry independent charge gradients.
  Source piecewise partitioning is evaluated in a
  cancellation-resistant form so zero and full allocations do
  not produce negative femtofarad residues. The QAVL=0
  limiting law is evaluated without undefined 0/0 arithmetic.
  **40 native ngspice 47 HICUM avalanche operating points** check
  generated current across weak/strong, reach-through and disabled
  branches, in addition to full-Jacobian finite differences.
- **HICUM/L2 2.4.0 substrate transistor:** native `HSI_Tsu`
  peripheral-base/substrate transfer and `Qdsu` diffusion storage,
  with independent Vbpci/Vsici derivatives, temperature-prepared
  ITSS/MSF/TSF and the source limexp continuation. Conserves
  the separate current/charge topology rather than folding it into
  the ordinary collector-substrate diode.
- **HICUM/L2 2.4.0 HICTUN:** source B-E tunneling current with
  capacitance-dependent `pocce` and attenuation, intrinsic/peripheral
  `tunode` selector, the source's reverse-bias OR gate over both
  junctions and analytic cross-voltage derivatives. Consumes published
  source QJMODF capacitance and its derivative; effective IBETS/ABET
  are prepared once at device temperature.
- **HICUM/L2 2.4.0 HICRBI intrinsic base resistance:**
  source nonlinear conductivity modulation from physical
  `Qjei + Qjci + Qf`, emitter-current crowding through
  `Ibei/FGEO` and the `FQI` forward minority-charge partition.
  Includes analytic derivatives of resistance and Vbpbi/Rbi
  current in both intrinsic junction voltages and separately
  exposes branch conductance. Source lateral NQS `Qrbi` uses
  `FCRBI` times the sum of published Cjei/Cjci and forward/reverse
  low-current diffusion capacitances, with independent Vbpbi
  capacitance and cross-charge gradients. Exact nominal RBI0 below 1 mOhm topology
  short is explicit rather than approximating a zero-ohm resistor.
  **35 independent native ngspice 47** operating-point comparisons
  for source RBI and CRBI, worst relative errors below 3.3e-5,
  across crowding/partition/modulation/shunt modes.
- **HICUM/L2 2.4.0 B-C barrier recombination:** exact
  `Ibh_rec = Q_bf / tbhrec` using the coupled HICCR excess-base
  charge, including both analytic bias derivatives and source
  zero-TBHREC disable mode.
- **HICUM/L2 2.4.0 nominal temperature preparation:** source TMPHICJ
  transforms all intrinsic, peripheral and substrate junction potentials,
  capacitances, B-E maxima and B-C/S-C punch-through voltages, including
  external Cj partitioning and linear peripheral-substrate mode. GICCR
  saturation, Qp0, T0, Tef0, Thcs, Rci0, Vlim, Vces, ICH-related
  prepared coefficients, HJEI/HF0/HFE/HFC and the seven HICDIO saturation
  currents follow their *three distinct* FLcomp thermal branches (2.1,
  intermediate 2.2 and 2.3+). Other source temperature laws cover
  substrate transfer, extrinsic/internal series resistors, TSF, avalanche,
  tunnel current/attenuation and RTH. Model TNOM, per-instance DT and
  the source's -100 to 326.85 °C device-temperature limits are applied
  once. All HICUM family kernels consume the same immutable prepared
  temperature; nominal model constraints and possibly lower transformed
  B-E cap maxima are validated separately.

Family-typed immutable prepared structs consume the temperature-prepared
parameters and, eventually, instance/geometry-adjusted values from the
translator/binder. Homogeneous evaluation batches borrow caller-provided
inputs/outputs, with no per-iteration allocation, identifier lookup or generic
dispatch. Numerical evaluation has no accepted-step state side effects.
The simulator still owns model topology binding, stamps and solver lifecycle.

**Complete transistor capability is not advertised.** Remaining device
assembly work includes source-validated model-card defaults, per-instance
geometry/multiplicity transformations, VBIC and HICUM internal/external
node binding and exact short reduction, integrated
electrothermal cross-Jacobians and accepted-step state. The circuit solver
still owns DC/AC/transient/noise stamping, correlated-source transfer and
integration, thermal-network convergence, complete PDK model coverage,
and oscillator differential gates. Unsupported devices must fail binding
rather than fall back to weaker approximations.

## Conformance tests

The native ngspice 47 oracle tests cover 164 distinct VBIC bias/model
points: 16 transport QBM=0/1 cases, 24 AJE/AJC/VRT/ART smoothing variants,
36 physical charge/capacitance cases across two QBM selectors and
three TF/ITF modulation configurations, 24 avalanche cases with
current/gmu measurements, 40 nonlinear epitaxial collector resistance
cases with the actual internal CI junction node voltage and external current,
and 24 temperature-dependent diode cases comparing Ibe and Gpi across
-40 to 125 degrees Celsius. Current, gm, go, Qbe, Qbc,
Cbe and Cbc are compared with independent ngspice outputs. The CSV fixtures preserve **both**
external source voltages and the actual **intrinsic** junction voltages
from ngspice: at high injection they can differ even when nominal series
resistances are zero. Only intrinsic values may be used for an isolated
physics-kernel comparison.

For reproducibility, run the fixtures/generate_vbic_oracle.sh script with
argument default or extended from this crate's directory.
The generator rejects unrecognized model parameters and simulator warnings.
The separate fixture generators for physical charge, collector resistance
and temperature are tests/fixtures/generate_vbic_charge_oracle.sh,
tests/fixtures/generate_vbic_resistance_oracle.sh and
tests/fixtures/generate_vbic_temperature_oracle.sh. All generated decks
reject unknown model parameters. The native ngspice 47 clamp of explicitly zero RCX
to 0.01 ohm is accounted for by reconstructing Vbcx/Vrci from its measured
collector source current and reported CI voltage; this is not a modeled
current correction. Near-zero currents need sufficient voltage resolution
to produce useful independent internal-node current tests.

The external model-card keyword is **VRT** (reverse reach-through voltage),
not VPT; **QBM** selects the base-charge expression. The fixture files
are reference simulator outputs, not replacement model equations.

Analytic first and cross derivatives are independently checked with finite
differences over forward/reverse modes and depletion-smoothing variants.
The parasitic substrate-law output additionally has a bounded native ngspice
47 operating-point anchor. Source example values from IHP SG13G2 are
tested without vendoring PDK model cards.

HICUM/L2 2.4.0 transport is independently exercised against the built-in
ngspice 47 HICUM level=8 model. The reproducible source fixture generator is
`tests/fixtures/generate_hicum2_transport_oracle.sh`: it records 144 native
operating points, retaining both terminal and intrinsic B-E/B-C voltages,
collector current, ICK, and forward transit time. All 144 native ICK results
are compared; 72 cases sharing the same source transport expression (barrier
disabled, lateral spreading disabled or collector high-current storage
disabled) additionally compare collector current and transit time. The
published VA-Models 2.4.0 HICQFC barrier-spreading coupling differs from the
native ngspice 47 generated implementation when both VCbar and lateral
geometry are nonzero. Those branches are checked by analytic finite
differences and charge conservation but **not claimed as native numerical
conformance**; the source-correct 2.4.0 equations are retained. Similarly,
the native HICQFC derivative in tiny geometries is documented as
approximate; its TF comparison is separately bounded. The HICCR test matrix
covers 480 bias/model combinations, checking 11 physical current/charge
outputs against both independent voltage derivatives, including implicit
Qp Jacobians. No alternate BJT or approximate solver fallback is used.

The HICUM 2.4.0 temperature fixture
`tests/fixtures/generate_hicum2_temperature_oracle.sh` reproduces
**192 native ngspice 47 operating points**: FLcomp 0/2.1/2.2/2.3,
-40/0/27/60/100/125 °C, local DT=0/15 K, with a low-injection and
three transport bias conditions. The **144** cases in the native/source
shared FLcomp modes 0/2.1/2.3 compare internal B-E/B-C diode
currents (removing native gmin*Vbc from its reported IBCI), ICK and
transit time; the **108** nonzero-forward-bias cases also compare collector
transfer current. **48** low-injection cases across all modes compare
both temperature-dependent intrinsic junction capacitances against
native CPIi/CMUi, whose diffusion contributions are negligible at
that bias. Published HICUM 2.4.0 Verilog-A has a separate
intermediate FLcomp 2.2 temperature law; the installed ngspice 47
level-8 implementation does **not** match that branch, so 2.2
current/TF samples are deliberately not counted as native source
conformance. Exact source equations are retained and the observed
discrepancy is explicitly regression-tested. The nominal AJEI/AJEP>=1
constraint is kept even when TMPHICJ makes an effective cap ratio
below one at high temperature.

Noise-source values are separately validated against the historical
noise-analysis constants from ngspice const.h: k=1.3806226e-23 and
q=1.6021918e-19, which are different from the VBIC transport constants.
The native VBIC noise routine calculates the ICCP shot source but appears
to omit it from its total accumulator; the prepared noise output returns
that distinct source rather than dropping it. Output-noise transfer gains
are not approximated by summing local spectra across different branches.

## Source provenance

- VBIC public pseudo-code, release 1.1.5:
  https://designers-guide.org/vbic/release1.1.5/vbic1.1.5_pseudoCode.html
- ngspice VBIC level-9 implementation (including reverse B-E breakdown):
  https://github.com/ngspice/ngspice/blob/master/src/spicelib/devices/vbic/vbicload.c
- HICUM/L2 2.4.0 reference Verilog-A, HICDIO macro:
  https://github.com/dwarning/VA-Models/blob/main/code/hicum2/vacode/hicumL2V2p4p0.va
- IHP SG13G2 public HBT LEVEL=9/VBIC parameter cards:
  https://github.com/IHP-GmbH/IHP-Open-PDK/blob/main/ihp-sg13g2/libs.tech/ngspice/models/sg13g2_hbt_mod.lib

Version specificity matters: HICUM/L2 3.0/3.1 and VBIC 1.3 must not silently
inherit the 2.4.0/1.1.5 equations. No vendor model sources are vendored.

## Reference thermal constants

Native ngspice VBIC level-9 equations use k=1.380662e-23 J/K and
q=1.602189e-19 C; the HICUM/L2 2.4.0 Verilog-A reference includes
NIST-1998 k=1.3806503e-23 J/K and q=1.602176462e-19 C by default.
The family kernels reproduce those numerical constants rather than silently
applying modern exact SI constants. The VBIC temperature-preparation stage
now implements all reference transforms and exposes its effective temperature
to each electrical subkernel. Complete source-aware .MODEL defaulting and
instance geometry and multiplier binding are still separate work.
