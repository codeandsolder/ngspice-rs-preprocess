# Legacy bulk BSIM preparation and kernel ownership

## Scope and evidence

This audit is pinned to ngspice source revision
`fa6c2a236507fd440c671de237cad4f49a4e6b68` and the accepted Rust solver revision
`89570506ea17d9b2cde2252749730201bd86dd7d`.
The upstream dispatch is in `src/spicelib/parser/inpdomod.c` and
`inpfindv.c`; device tables, setup, temperature, load, AC, pole-zero
and noise routines are in the individual directories under
`src/spicelib/devices`. The solver's existing
[BSIM port document](https://github.com/codeandsolder/ngspice-rs-fast/blob/89570506ea17d9b2cde2252749730201bd86dd7d/docs/port/BSIM.md)
is the starting point for preserving numerical behavior, not a claim
that every listed mode is implemented.

## Distinct device identities

| Level | Version | Source directory | Model inputs | Instance inputs (ask-only excluded) | Charge model selection |
| --- | --- | --- | ---: | ---: | --- |
| 4 | independent BSIM1 family | bsim1 | 81 | 14 | Own equations, no capMod selector |
| 5 | independent BSIM2 family | bsim2 | 137 | 14 | Own equations, no capMod selector |
| 8/49 | 3.0.x | bsim3v0 | 354 | 12 | capMod=1 branch versus alternate branch; default 1 |
| 8/49 | 3.1.x | bsim3v1 | 367 | 12 | capMod 0/1/2; default 2 |
| 8/49 | 3.2.x | bsim3v32 | 428 | 15 | capMod 0/1/2/3; default 3 |
| 8/49 | 3.3.x/default | bsim3 | 429 | 19 | capMod 0/1/2/3; default 3 |

The exact input catalogs are generated from the six IFparm tables. Every
alias uses the same canonical slot as its upstream setter ID; OP-only rows
are not inputs. BSIM1/2 IC uses a real vector as confirmed by their
instance setters. The BSIM3 revisions differ even in the type and presence
of the version setter: absent in v0, real in v1, and string in v32/v33.
Source dispatch precedes applying the ordered setter stream. The selector
recognizes the upstream 3.x version prefixes and an absent or literal
`default`, rather than accepting arbitrary strings containing `default`.
No unknown revision is silently mapped to the newest kernel.

All four BSIM3 revisions declare NQS selectors; v33 adds separate
`acnqsmod` model and instance control. The corresponding charge node,
state equations and AC behavior cannot be reduced to a capMod change.
The source branches identify work to audit, not a license to accept every
integer selector: effective defaults, admissible values, geometry and
temperature prerequisites must be established before kernel preparation.

## Existing numerical ownership

At the audited solver revision, native BSIM3 v3.3 implements DC, OP,
capMod=3 quasistatic terminal charge, transient integration, AC, PZ, TF,
and noise. Distortion is linearized consistently with the C device.
The native device explicitly rejects the other charge modes, NQS,
nonzero ACM and older BSIM revisions; sensitivity is not implemented.
Existing v3.3 numerical evidence includes `c_bsim3_dc_reference`,
`c_bsim3_charge_reference`, `c_bsim3_small_signal_reference`,
`c_bsim3_device_reference`, `c_bsim3_setup_reference` and the native
BSIM3 unit and integration suites. Native BSIM4 remains a separate,
extensively tested 4.8.x device and is not an alias for BSIM3.

The catalogs are an input schema only. BSIM1 and BSIM2 additionally have immutable model and size-dependent instance preparation (see below). BSIM1 and BSIM2 have distinct constitutive laws in a shared generalized physical artifact in `model-kernel-bsim`. Neither is registered in the consuming solver yet. In particular the
new v33 input metadata does not transfer ownership of the existing
working solver implementation. The producer should eventually own
immutable physical preparation, while the consuming solver continues
to own topology, Newton limiting, sparse stamping, integration history,
and analysis orchestration.

## Migration sequence and acceptance

1. Coordinate with the active BSIM3 charge, NQS, sensitivity and arithmetic
   campaigns before extracting physical routines. Reuse the validated v33
   preparation and quasistatic constitutive implementation; do not create
   a parallel independent implementation. Retire the old owner atomically
   when the replacement is accepted.
2. Resolve version, aliases, given flags, ordered setters and binning once.
   Separate model preparation from per-instance geometry and temperature.
   The schema does not supply defaults; missing assignments must remain
   distinct from zero until actual family setup rules have run.
3. Bind prepared quantities and the physical terminal/hidden-equation
   layout to the shared model-kernel API. Current residuals, charge and
   their Jacobians must share coordinates and preserve orientation,
   conservation and derivative consistency. Keep solver history and
   limiting out of the producer.
4. Validate v33 capMod=3 against the existing solver/C reference evidence
   before switching the native owner. Then add other capMod modes and NQS
   with their own source equations and charge-state references. Coordinate
   sensitivity with the common derivative contract, rather than inventing
   an alternate device API. Check prepared-artifact identity and batch
   equality for model, revision, mode, temperature and geometry changes.
5. Implement each older BSIM3 revision and BSIM1/2 with its own setup and
   physical semantics. Reuse algebra and storage only when equivalent.
   Require independent C OP/DC, current and charge derivatives, AC,
   transient and noise comparisons wherever the corresponding reference
   supports them, including both polarities, reverse orientation,
   geometry and temperature corners, and mode-dependent topology.
   Keep unsupported analyses explicit until validated.

Until these steps are complete, recognizing a source card is not evidence
of physical simulation support.

## Reproducing the catalog

From this repository's root, using a checkout of the pinned reference:

```sh
uv run --no-project python .github/scripts/generate_legacy_bsim_catalog.py /path/to/ngspice | diff - crates/model-compact-intrinsic/src/catalog/legacy_bsim.rs
```

The generator copies only interface facts (name, type, access and alias
identity), not formulas or source descriptions. The schema tests check
revision selection, counts, sorted unique names, dense canonical slots,
ordered aliases, rejected ask-only inputs and version-specific types.


## BSIM1/2 immutable preparation

`prepare_legacy_bsim_model` resolves every model scalar and polarity, retaining
both the original given value and the effective defaulted/clamped value.
`PreparedLegacyBsimModel::prepare_instance` produces a typed
`PreparedLegacyBsimInstance` with a distinct BSIM1 or BSIM2 coefficient block.
It validates effective dimensions and positive finite oxide thickness,
applies DL/DW in micrometres, calculates all size-dependent mobility,
threshold and weak-inversion coefficients, and derives junction saturation
currents, capacitances and series conductances. Geometry input is in SI
units and multiplicity remains separate. No global scale is applied:
neither legacy C instance setter scales its dimensions. Initial-condition
assignment precedence and OFF remain solver concerns.

The two families differ materially: BSIM1 clamps phi/K1/K2, interpolates
mobility at zero and VDD, and bounds the VDD drain slope; BSIM2 has a
separate saturation-mobility body constraint, unscaled beta2, a distinct
model TEMP thermal voltage and low-field/impact-ionization coefficients.
BSIM1's source-default oxide thickness is zero and must be explicitly
supplied before physical instance preparation. Both source temperature
routines ignore circuit ambient, and BSIM2 computes its thermal voltage
from model TEMP + 273 rather than ambient Kelvin. Noise temperature and
junction thermal voltage are separate from this model preparation.

The prepared overlap values reproduce the C temperature-stage outputs.
There is an upstream inconsistency: the AC/PZ routines use these prepared
values, while the transient load recomputes overlap using drawn width
and corrected length. A single generalized charge law must not switch
geometry based on analysis kind. Resolve that explicitly at kernel
integration and qualify AC and transient against independent physical
derivatives, distinguishing the native reference discrepancy. BSIM2 also declares XPART as a real but reads the integer member of IFvalue. The physical kernel accepts the documented boolean values 0 and 1, which use the same conservative charge law; arbitrary bit-pattern interpretations are rejected.

Verification: 3 nontrivial parameter/geometry cases per family compare
261 size-dependent values against the actual C setters and temperature
routines; 214 scalar default values come from independent ngspice 47
showmod output. Candidate changes, wrong family, nonphysical dimensions
and repeated preparation are exercised by source-free tests. The C
harness and numeric witnesses are in the intrinsic crate's tools and
tests/fixtures/legacy-bsim directories.

## BSIM1 artifact and reference discrepancies

The BSIM1 intrinsic law differentiates the current and conservative channel
charge with a three-coordinate stack-only forward derivative. Both charge
partitions are covered by 216 direct C channel points and 180 independent
five-point derivative probes. The generalized artifact assembles these
physical terms with junction current/charge, overlap capacitance, optional
series branches, and thermal/flicker noise. Its tests cover both polarities,
both intrinsic orientations, hidden-node topology changes, and immutable
candidate reevaluation. Sixteen native ngspice operating points validate
external currents and hidden-node residuals.

The artifact intentionally does not reproduce these upstream inconsistencies:

- The original load adds gmin to the junction law and again in its equivalent
  source stamping. A generalized residual uses it exactly once; its Jacobian
  matches the residual. Native OP comparisons isolate the physical law with
  gmin=0, and a separate test verifies the full nonzero-gmin difference.
- Transient and AC/PZ overlap geometry differs in the C device. The artifact
  uses the temperature-prepared overlap for one analysis-independent
  conservative charge law.
- Both original noise routines read the state-slot *indices* of gm and cd
  rather than the actual state values. For the first BSIM1 instance the
  channel conductance input is 7 times its multiplicity, not its actual gm,
  and the flicker expression uses 4 instead of the drain current. This
  produces a spurious dependence on device ordering. The physical source
  densities instead use the true current and analytic gm. The fixture
  noise-probe.cir reproduces the native discrepancy; noise-dc.cir measures
  the physical current and gate derivative independently, which the
  source-density tests use at three frequencies.

## BSIM2 artifact

BSIM2 retains its separate model-temperature law, voltage-domain bounds,
cubic-Hermite weak-inversion transition, mobility/interpolation function,
impact ionization and channel charge equations. The cubic is evaluated in
dimensionless endpoint coordinates, and derivatives follow the actual
constitutive function, including its voltage clamps. The model's VBB
bound is retained even when the source default is positive. The native
positive drain-current numerical floor is not a physical source and is
not included in the zero-current limit.

The same stack-only differentiation and generalized branch assembly used
for BSIM1 support BSIM2 without substituting its channel equations.
There are 448 additional direct C operating-point/charge witnesses,
another 180 independent derivative probes, and 16 additional native
circuit operating points. The shared artifact tests exercise both model
families, both orientations and polarities and optional series nodes.
Native solver registration remains separate consumer work. The ABI 4 family
policy supplies a cold start using the zero-bias threshold, normalized
VDS=0 and VBS=-1. During Newton/predictor evaluation it selects orientation
and the exact bias-dependent threshold from the evaluated reference,
restricts the gate/drain step with the shared FET limiters, and bounds both
body junction voltages using the actual saturation currents and the fixed
legacy thermal voltage. All intrinsic voltage differences are contracted
along one common step; the exact constitutive equations and derivatives
remain unchanged. Additional convergence compares the nonlinear residual
rows to their previous exact Jacobian predictions. Source OFF and explicit
IC handling, exact-load phase selection, history and acceptance remain
consumer responsibilities.

Series-node selection is resolved by the same resistance law as full
preparation, independently of channel coefficients. Artifacts take this
structural choice rather than a representative numeric instance; numeric
geometry and topology edits are checked at actual instance preparation.
Tests cover all four series shapes, both orientations and polarities,
Newton and predictor references, exact loads and rejected limited trials. Older BSIM3 revisions and
additional v3.3 modes remain separate, coordinated physical-port work.
