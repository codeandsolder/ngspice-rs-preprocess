# ngspice 47 VDMOS native prepared kernels

A reusable typed VDMOS model package for ngspice-rs-preprocess. Derived from ngspice 47 tag `a80f6e3`, particularly `vdmosload.c`, `vdmostemp.c`, `vdmosset.c`, `vdmosnoi.c` and `devsup.c:DevCapVDMOS`.

## Physics and state interface

- NMOS/PMOS weak inversion, threshold/gain/slope temperature dependence, forward/inverse channel operation, transconductance, nonlinear triode/saturation, quasi-saturation drain resistance, temperature-dependent ohmic branches and power derivatives.
- Gate Cgd follows ngspice's tanh/atan law, integrated into a continuous charge law with `dQgd/dVgd=Cgd`; Cgs is constant. Upstream's `DevCapVDMOS` returns *half* Cgd for its old Meyer-style integration, whereas the kernel reports the **full physical capacitance**.
- Diode forward/reverse conduction with native BV/IBV/NBV avalanche and temperature-dependent transition voltage. Implicit differentiation of the converged avalanche condition supplies `dXBV/dT`; junction depletion and diffusion charges include voltage and temperature derivatives, including the `m=1` limit.
- Ngspice 47 soft recovery uses the independent normalized node coordinate `qp` and **stored charge** `Qs(T,qp)=TT(T)*qp`. Its residual is `dQs/dt+(qp-TT*Id)/VP=0`; the source-to-drain physical body-diode current includes `+polarity*(1-VP)/TT*dQs/dt` (the drain-to-source channel-current convention is opposite). The kernel reports the charge, its node/temperature partials, the algebraic residual and derivatives, and the temperature-sensitive diode coupling. This preserves the charge derivative when transit time changes with self-heating.
- Thermal RC: junction capacitance, junction-to-case and case-to-ambient conductances, channel/diode Joule power and per-ohmic-branch heating with derivatives. Unconnected zero-ohm branches are topology shorts, not artificially large conductances.
- Branch-local source-current noise densities from ngspice's RD/RS Johnson, 2/3-channel thermal and KF/AF flicker laws; noise is evaluated at the temperature retained in the same device `Evaluation`, avoiding a separate mismatched temperature argument. The simulator applies the correct AC transfer functions to produce observation noise.

`Model::prepare` validates a model and computes nominal constants once; `PreparedModel::bind` binds independent device multiplicity and heating; `PreparedInstance::evaluate` returns immutable current/charge/heat and Jacobians with no allocations, no accepted/trial-state mutation, no topology reconstruction, and no string lookup. `evaluate_batch` uses borrowed input/output buffers. `compile_ngspice` maps already parsed semantic field/value pairs and their aliases (`VTO/VTH0`, `VTOTC/TCVTH`, `BEX/MU`, etc.) to the same prepared representation, including Celsius-to-Kelvin TNOM and grouped RQ/VQ or BV/IBV/NBV activation. Unknown source fields, distortion-only PHI and unsupported SOA annotations **fail closed** rather than disappearing.

**Plaintext source compiler boundary:** `model-normalize-ngspice::model_card` lexes generic `.model` statements, SPICE numeric suffixes, comments, physical line locations and `+` continuations. `param_expression::SpiceExpression` parses source-level arithmetic, ngspice operator precedence and builtin scalar functions (including lazy `?:`, logical short circuit, `ternary_fcn`). `parameter_scope::ModelLibrary` collects `.param` definitions and nested `.subckt` lexical scopes, resolves forward references/case-insensitive symbols and detects cycles/undefined names. Its `resolve_vdmos(model_index, overrides)` evaluates each VDMOS model's source expressions at preparation time using per-scope **numeric instance overrides**, preserving annotated `mfg`, `Vds`, `Ron`, `Qg` metadata separately. The linker supplies instantiated X-line parameters in their calling context; this model package does not expand circuit topology. The parser rejects builtin/reserved parameter names (ngspice 47 silently mishandles some, e.g. `.param pow=...`), undefined functions, cycles and non-finite calculations. Source-defined `.func` macros are compiled into the same expression AST, and receive unevaluated arguments so unused conditional branches stay lazy. Functions are located along the lexical scope chain, but **free variables in a function body resolve in the caller's instance scope**, exactly as ngspice 47 does; this differs from `.param` definitions, whose expression symbols are bound in the declaration's own scope. `ModelLibrary::evaluate_scalar(caller_scope, expression, overrides)` evaluates instance override expressions before binding callee parameters.

There is **no runtime parsing dependency** on the kernel: normalizer and source compiler are `model-kernel-vdmos` *dev-dependencies* for integration/oracle tests, and the standalone numerical kernel consumes the already prepared typed model. All local C fixtures compile from their actual source declarations; additional ngspice-47 golden tests exercise forward references, last-wins `.param` assignments, function/conditional expressions, independent subcircuit overrides and caller-scope `X` parameters. `examples/vdmos_corpus.rs` applies the same linked-file and source compiler to full libraries. `model-normalize-ngspice::linked_source::SourceLinker` resolves native ngspice `.include`/`.inc` and `.lib path section` references, with the including file's directory taking precedence over explicit search paths, canonical-file/section cycle detection, named section selection and origin mapping back to physical file and line. Ordinary `.lib section` definitions do not leak into the calling model context unless selected. Unknown/malformed references fail explicitly, without environment-variable or implicit search-path fallback. The caller then feeds `LinkedSource.bytes` into `ModelLibrary::parse`; passing unresolved `.include`/`.lib` directives directly to the model parser is an explicit error, not a silently incomplete model. Circuit X-line expansion and its topology remain separate solver/compiler work.
The caller owns instantiated X-line call-site scopes, topology binding, current and capacitance stamping, junction voltage limiting, timestep integration, historical states and rollback, and all circuit-level analyses.

## Reproducible checks

```sh
cargo test --locked -p model-kernel-vdmos
cargo clippy --locked -p model-kernel-vdmos --all-targets -- -D warnings
cargo test --locked -p model-kernel-vdmos --test ngspice47 -- --ignored
cargo test --locked -p model-kernel-vdmos --release --lib throughput_prepared_batch -- --ignored --nocapture
```

Focused unit tests exercise polarity, independent instances/batching, numerical channel/diode/gate/recovery/thermal Jacobians, avalanche matching and its implicit temperature derivative, charge asymptotes, source parameter preparation and noise laws. The **thirty-nine opt-in ngspice-47 C oracles** cover NMOS/PMOS DC Id/gm/gds, NMOS/PMOS avalanche, elevated-temperature avalanche, NMOS/PMOS gate/diode capacitances, hot capacitance, VP soft-recovery depletion and independently solved NMOS/PMOS electrothermal DC equilibria (channel current, case temperature and junction temperature), weak-inversion-to-triode sweeps including hot channel and gm/gds, voltage/temperature quasi-saturation drain-resistance sweeps, and a synthetic PMOS with reversed `Cgdmin`/`Cgdmax` endpoints. Full transient cases add NMOS/PMOS reverse recovery, ordinary diode diffusion without VP, NMOS/PMOS electrothermal on/off cycling, and NMOS/PMOS nonlinear gate switching/charge history. Transient oracles read ngspice 47 adaptive-step waveforms and independently integrate scalar state equations using test-only RK4 between known PWL forcing breakpoints; no integration/stamping is added to the runtime kernel. Charge/current comparisons distinguish physical constitutive laws from ngspice's integrator-dependent displacement during sharp ramp transitions. C reference fixtures disable solver gmin regularization where the bare nonlinear model is compared. Eleven additional source-compiler C oracle fixtures exercise function nesting, local function definitions, global functions reading *caller* parameters, global `.param` values retaining *declaration* scope, forward and cyclic source parameters, as well as independent subcircuit instance overrides and real linked `.include`/`.lib` multi-file operating points with ID, gm and gds comparison, including a named library section importing another section from its own file.


**Full-catalog compatibility:** An independently downloaded copy of the publicly indexed LTspice `standard.mos` device library was checked (reproduce with `cargo run --locked -p model-kernel-vdmos --example vdmos_corpus -- /path/to/standard.mos`). All **773/773 VDMOS model cards** parse, resolve PCHAN/NCHAN polarity and manufacturer/rating annotations (`mfg`, `Vds`, `Ron`, `Qg`), prepare, bind and evaluate. Manufacturer libraries are external inputs. One catalog exception uncovered in the first run had positive but reversed capacitance endpoints; the mathematical capacitance law supports this inverse ordering. Validation now rejects negative endpoints but does not invent an ordering restriction. The synthetic PMOS ngspice 47 fixture checks `Cgd` across eleven *intrinsic*, rather than external-series, bias points.

**Independent catalog hardening (2026-11-24):** A separate publicly indexed LTspice `standard.mos` catalog from `mpkopec/ltspice-lib` contains **905 distinct VDMOS cards**. Before any dialect-specific normalization, **841/905** are supported by the unchanged ngspice-47 parameter grammar; remaining rejects are 41 `Cgso` (LTspice-specific gate overlap semantics), 21 legacy `subthres` (not a native ngspice-47 VDMOS field), one incomplete numeric expression and one malformed bare `*`. The six additional NXP models with declared `Is=0` are correctly prepared after observing that **ngspice47 `vdmosset.c` floors source `Is` to `CKTepsmin=1e−28 A`** (`cktinit.c`), before instance preparation. This input-source normalization is explicit in `compile_ngspice`; the physical kernel still requires positive `Is`. Neither unsupported dialect fields nor corrupted source declarations are silently interpreted as other physics. The 171,482-byte catalog and C differential report are external research inputs, not distributed fixtures.

**Known ngspice 47 C `gm` discrepancy:** The upstream weak-inversion implementation applies the derivative of `1/(1+theta*max(Vgs-Vth,0))` below threshold as if `max()` were active there. This adds an unphysical small transconductance term even though that factor is constant below threshold. The kernel retains the correct derivative; the C oracle explicitly reconstructs that spurious C term when comparing `@m1[gm]`, while checking `Id` and `gds` tightly. Do not copy the inaccurate derivative into the production kernel.

The release-mode smoke benchmark runs 4096 prepared biases for 256 iterations. It measures kernel evaluation only, not the circuit solver, transistor integration or topology binding. The benchmark exercises both ordinary and avalanche-configured models. Recent single-host release runs yielded roughly 1.2–1.6 million evaluations/s across configurations; per-iteration times vary with system contention, so these are smoke throughput observations, not controlled cross-architecture comparisons.

## Remaining integration gates

- Expand actual X-line instance topology with caller-evaluated parameter overrides, extend linking to dialect-specific library formats only with explicit source semantics, broaden beyond the 773-card LTspice catalog to other manufacturer and behavioral dialects, and implement optional PHI distortion and SOA handling in their appropriate owners.
- Circuit-coupled buck and switching-converter references (not only prescribed external terminal voltages), transient temperature-varying VP/TT recovery history, and wider device geometry/vendor-model sweeps.
- Generic artifact #183 and solver binding in ngspice-rs-fast #189, with topology/state lifecycle and charge/stamping agreement.
- Profile homogeneous SoA/AoSoA evaluation and dependency-driven re-preparation; keep all solver-specific policy out of this crate.

### Randomized external catalog differential campaign

Use the opt-in `catalog_oracle` example with an external plaintext VDMOS
catalog and ngspice 47 on PATH:

```sh
cargo run --locked -p model-kernel-vdmos --example catalog_oracle -- /path/to/standard.mos 185
cargo run --locked -p model-kernel-vdmos --example catalog_oracle -- /path/to/flat-catalog.mos 185 --isolated
cargo run --locked -p model-kernel-vdmos --example catalog_oracle -- /path/to/flat-catalog.mos 185 --isolated --diode
```

Each model gets two independently selected bias/temperature points (−25 to
125 °C), with the second also perturbing explicitly supplied KP, series
resistances, gate and diode capacitances and TT by independent ±20% factors.
Compare intrinsic channel current and Cgd/Cgs to ngspice 47 at its observed
intrinsic Vgs/Vds. The separate `--diode` campaign compares forward/reverse
intrinsic body-diode current, stored charge and differential capacitance,
using the actual internal diode node when Rb is positive. These are DC
constitutive-law tests, not transient recovery or full circuit-integration
tests. The seed and exact model name select a
reproducible case; append the model name to either command to debug a single
card. The normal mode retains linked source scopes; `--isolated` is only for
flat independent model-card catalogs and reports malformed records without
discarding the rest. Any failed card or comparison makes the run unsuccessful.

External catalog results for seed 185 (sources are not vendored):

| Catalog | Cards | Channel pass / fail | Diode pass / fail | Source rejects |
| --- | ---: | ---: | ---: | ---: |
| [evenator/LTSpice-Libraries standard.mos](https://github.com/evenator/LTSpice-Libraries/blob/master/cmp/standard.mos) | 773 | 1546 / 0 | 1546 / 0 | 0 |
| [HenniePeters/LTSpice standard.mos](https://github.com/HenniePeters/LTSpice/blob/e8f4f38f69bfcbbfe18772fb02dde875f02ebcaf/_STANDARD/standard.mos) | 1496 | 2568 / 360 | 2573 / 355 | 32 |

The second catalog SHA-256 is
`7678727fd23e308e0a97c4400cb758f2a8a7dd34d08604f6bbfec515637f395c`.
No finite differential comparison exceeded tolerance in the final run.
Remaining failures are unsupported source fields, malformed source cards,
invalid physical bias/temperature domains and nonfinite C observations.
Catalogs overlap; counts are declarations and sampled cases, not unique parts.

**Catalog-driven diode and series parameter corrections:** C's setup bounds
explicit EG to at least 0.1 eV and FC to at most 0.95. Nonpositive
source RD/RS/RG/RB do not create internal resistor branches in ngspice 47;
typed physical model input remains nonnegative. Negative fitted M is
supported when the actual depletion capacitance remains passive. The
temperature-adjusted VJ limiter activates when FC*VJ > 2.5 V: C derives
zero-bias capacitance before replacing VJ with 2.5/N. The kernel matches
this valid low-forward/reverse-bias law, but recomputes a continuous
depletion continuation at a fraction min(N, 0.95) of the limited potential,
rather than using the stale C coefficients at its potentially singular
2.5 V cutoff. All 52 observed high-VJ charge/capacitance discrepancies
were resolved on focused reruns, with independent voltage/temperature
Jacobian and continuation tests. This is an intentional physical
correction beyond C's valid domain, not a claim of identical behavior
at its defective cutoff.

**Known C reverse-diode conductance discrepancy:** in the reverse
(non-avalanche) current law C uses `arg^3` but its reported conductance
uses `arg`. Consequently its reported diffusion capacitance (TT*gdb)
is not the derivative of its stored charge. The catalog oracle accounts
for exactly that factor when the reverse-law branch is identified,
while independently checking current and stored charge; the native
kernel retains the mathematical derivative. No comparison tolerance
was widened.

### LTspice electrical dialect and extreme weak inversion

The native kernel uses a stable scaled softplus, including the exact zero-width
limit and analytical derivatives. Independent LTspice XVII and 24.0.12 probes
confirmed the Cgso/Cgs alias (last declaration wins), the legacy Subthres
conversion, and finite channel values for eight catalog models with C47
overflow or zero-width failures. The regression uses 40 independently
specified synthetic channel
points with a closed-form oracle, plus 11 measured synthetic legacy transfer
points in tests/ltspice_vdmos.rs. Catalog parameters and per-device results
are external research inputs.

The explicit model-normalize-ngspice vdmos_ltspice::translate_electrical
entry point translates these measured source fields; native ngspice
compilation remains strict. Positive explicit Ksubthres overrides Subthres.
The legacy calibration matches the old exponential continuation at normalized
Vds=1 V and Vgs-Vto=-1 V: x0=min(sqrt(2*Subthres/Kp),1), and the target
normalized current is x0^2/2 * exp(-2*(1+1/x0)). Solve for the softplus
width at that reference bias, preserving the measured rounding of
log(1+exp(-1/s)) in calibration only. The runtime law is numerically stable.
The calibration measurements include the quantization floor and underflow.
RthetaJA and its associated ladder/observation fields are returned separately;
they must not be mapped to native electrical self-heating. DC and 50 ms
transient LTspice probes found identical electrical current with these fields
varied. This does not implement the SOA thermal observer or claim equivalence
of all implicit LTspice defaults.

The catalog oracle accepts --ltspice-electrical for explicit field translation
before comparing with ngspice 47. Hennie catalog seed 185: 154 translated cards
(including 24 with observation metadata), 2875/53 channel and 2884/44 diode
passing/failing comparisons, with 32 source rejects per campaign. All remaining
comparison failures were invalid parameter domains, other unrecognized
fields, or nonfinite C observations; no finite mismatch. The untranslated
results above remain the native-source baseline.

Independent circuit verification: solver commit e1d93bc with kernel 0080059
passed all three published power-stage reconstruction tests in
ngspice-rs-fast #189. The Bramble LTC3854 synchronous buck, Bramble LTC3872
boost, and ADI ADALM-SR1 boost use published component values with fixed PWM
and explicit device models, compared against C47 on the same decks.
These are power-stage reconstructions, not full controller-IC replicas.

## Shared prepared artifact

`artifact::artifact` binds the existing immutable prepared model to
`model-kernel-api` ABI v3. The descriptor exposes three electrical terminals
and, when enabled, junction and case thermal terminals in absolute kelvin.
Positive series resistances add local electrical coordinates, while exact
zero-ohm branches coalesce at preparation. The recovery coordinate, when
present, is the polarity-normalized Qp variable already used by the kernel.
Model parameter changes rebuild the artifact; parallel multiplicity remains
a numeric candidate slot. The content fingerprint must identify the compiled
model and structural options.

The artifact assembles conservative residuals and stored quantities from the
same native model laws and analytic derivatives; it neither duplicates those
laws nor owns integration history. In particular, a temperature-dependent
diode recovery gain `g(T)` multiplying `dQs/dt` cannot be represented by
differentiating `g(T)*Qs`. Instead, use the recovery row
`Fq + dQs/dt = 0` to substitute `-g(T)*Fq` for that electrical current.
This exact row transformation preserves the DAE solution, including variable
temperature; its Jacobian includes both the recovery-residual and gain
temperature derivatives.

The generalized residual tests verify every Jacobian entry by finite
differences, both polarities, thermal and nonthermal configurations, common
electrical reference shifts, candidate isolation, native noise densities
and contiguous homogeneous batches. Circuit-coupled engine validation
remains separate from the producer tests.
