# Intrinsic HiSIM/SOI compact-model package — preparation scaffold

This standalone crate provides source-backed compact-model preparation and physical subkernels.
The package includes HiSIM-HV source-derived physical preparation and therefore
carries the upstream Hiroshima University/STARC terms as well as the original
Rust MIT license. See the package [`LICENSE`](LICENSE) and the repository's
[`THIRD_PARTY_NOTICES.md`](../../THIRD_PARTY_NOTICES.md).

## Exact scope

The crate binds **source-defined model and instance parameters**, without
running the physical compact-model equations. It **does not** yet provide nonlinear
currents, terminal charges, capacitance Jacobians, noise, thermal dynamics,
solver topology or solver-ready native device kernels. Source-derived
**BSIMSOI 4.x and HiSIM-HV1/HV2 model setup defaults** are implemented.
HiSIM-HV1/HV2 additionally have source-derived **instance defaults, binning,
metallurgical channel geometry and leading temperature-dependent bandgap,
thermal-voltage and carrier-density inputs**. Other families still
need dedicated setup and per-instance preparation, except HiSIM2's
source-derived setup and binning described below.
`ModelVariant::KERNEL_STATUS` explicitly advertises `NotImplemented`. Downstream
must not substitute generic MOS laws for these devices or interpret successfully
bound parameters as physical model support.

All parameter names, types, alias identities and terminal positions come from the
ngspice reference checkout, commit
`972dbf45db1e47d18bc4fda5d8bb829aff2d0e05`
(`codeandsolder/ngspice`, tracking `imr/ngspice`). This is a reference source
snapshot, not an independent claim that the model implementation is complete or
numerically validated.

| ngspice model table | Level | C source under `src/spicelib/devices/` | Model names | Instance names |
| --- | --- | --- | ---: | ---: |
| HiSIM2 | 68 | `hisim2/hsm2.c` | 546 | 32 |
| HiSIM-HV v1 | 73, version 1.1/1.2 | `hisimhv1/hsmhv.c` | 610 | 38 |
| HiSIM-HV v2 | 73, version 2.0/2.1/2.2 or default | `hisimhv2/hsmhv2.c` | 730 | 38 |
| BSIMSOI | 10/58 | `bsimsoi/b4soi.c` | 915 | 38 |
| BSIM3SOI FD | 55 | `bsim3soi_fd/b3soifd.c` | 393 | 16 |
| BSIM3SOI DD | 56 | `bsim3soi_dd/b3soidd.c` | 393 | 16 |
| BSIM3SOI PD | 57 | `bsim3soi_pd/b3soipd.c` | 470 | 25 |
| SOI3 | 60 | `soi3/soi3.c` | 87 | 25 |

`src/spicelib/parser/inpdomod.c` defines the level and HiSIM-HV version dispatch.
The C `IFparm` parameter IDs define canonical slots, so two names with one ID are
aliases. Slots are valid only within a given `(variant, model-or-instance)` pair,
not as cross-family stable identifiers. The tables are sorted lexicographically to
support preparation-time binary search. A `BoundParameters` value is a dense slot
block containing only explicitly supplied parameters; the absence of an assignment
must **not** be mistaken for a physical default of zero. Model and instance scopes
are independent; version/level selection occurs before binding.

The terminal arrays preserve the upstream positions rather than collapsing to four
MOS terminals. HiSIM-HV adds substrate and temperature; BSIMSOI/BSIM3SOI have
additional backgate, body, thermal and charge terminals with a reserved blank
fifth position; SOI3 has separate front/back gates and a thermal terminal.

## HiSIM2 2.80 setup, geometry and topology

The `hisim2` module implements the source-derived setup for level 68, version
280. It reuses the existing registration catalog, covering 528 constant
defaults, dependent polarity/mode defaults, original source-given flags,
depletion/reciprocity adjustments, forced noise enable and upstream range
resets. Invalid explicit versions and checked mode selectors are rejected
instead of silently falling back to another physical configuration.

- `hisim2::prepare_model` returns prepared model-card values in their original
  units, including Celsius TNOM.
- `hisim2::prepare_instance` takes a bound instance and the circuit temperature
  in Kelvin. It resolves model-inherited values, source IC ordering, effective
  device temperature, and all 72 source binning terms with the VFBC length
  correction. Values remain in source units.
- `hisim2::prepare_physical` checks model/instance identity and applies the
  source temperature-routine geometry corrections, evaluator-unit conversion
  of binned coefficients, and drain/source, gate and body parasitics. The
  external series conductances are checked against native DC internal-node
  voltage drops and branch currents.
- `hisim2::prepare_temperature` computes nominal and actual band gap, inverse
  thermal voltage and intrinsic carrier density with the original constants.
- `hisim2::prepare_junctions` binds saturation, reverse leakage, linear
  continuation, and area/sidewall/gate-sidewall charge coefficients once per
  prepared instance. Evaluation accepts normalized drain/source junction
  voltages and circuit GMIN; returns multiplicity-scaled current, charge and
  analytic voltage derivatives. The source uses instance TEMP for saturation
  preparation and circuit temperature plus DTEMP for capacitance scaling.
  The logarithmic limit at grading=1 is evaluated analytically, avoiding the
  original expression's removable singularity.
- The topology identifies distinct drain, source, gate and body-network nodes
  and NQS state selection without allocating solver nodes. It does not imply a
  complete numerical kernel.

Source-backed setup tests and a live native ngspice 47 reference matrix:

```sh
cargo +nightly test --locked -p model-compact-intrinsic --test hisim2_setup
cargo +nightly test --locked -p model-compact-intrinsic --test hisim2_setup --test hisim2_junction -- --include-ignored --nocapture
```

The native reference compares all 544 numeric model-table fields and 31
numeric instance inputs. The native display truncates two groups of parameter
names to eleven characters; the test maps their occurrences using the source
registration order. A second native matrix checks 936 junction current,
conductance and capacitance values across 156 bias points (NMOS and PMOS,
-20 to 100 C, explicit TEMP/DTEMP, geometry and multiplicity). Charge
derivatives are independently checked by finite differences. The original HiSIM2 Distribution Statement and Copyright
Notice (Copyright (C) 2014 Hiroshima University & STARC) is reproduced in
the package LICENSE.

## BSIMSOI 4.x source-derived model preparation

`prepare_bsim_soi_model(&BoundParameters, nominal_temperature_kelvin)` evaluates
**891 ordered defaults** from ngspice 47 `B4SOIsetup` in `b4soiset.c`, using the
`b4soimpar.c` setter mapping to identify the same canonical parameter slots.
These comprise 770 real constants, 18 integer constants, 80 copy dependencies,
11 polarity-dependent expressions, 2 `mobmod=3` dependencies, 8 legacy alias
precedence rules, the nominal temperature and the fringe capacitance formula.
The result is immutable `PreparedBsimSoiModel` data with O(1) slot reads; its
values are not a ready-to-evaluate physical kernel.

- `TNOM` card values are converted from degrees Celsius to Kelvin during
  preparation, while absent `TNOM` uses the caller's nominal Kelvin temperature.
- The C model's NMOS/PMOS flags choose polarity-dependent defaults. Conflicting
  true flags are rejected rather than silently selecting the last flag.
- Explicit model values override defaults. Historical `NLX` and `NGIDL` aliases
  influence renamed parameters only when the corresponding newer parameter was
  not given; likewise for the L/W/P-binned forms.
- Out-of-range `SOIMOD`, `RGATEMOD`, `FNOIMOD`, `TNOIMOD`, `RDSMOD` and `RBODYMOD`
  selectors produce explicit errors. Upstream C emits warnings and silently
  substitutes its defaults; we intentionally reject implicit changes of physics.
- Parameters without a registered, source-derived default remain absent in the
  prepared buffer rather than being assigned an invented zero.
- This represents **setup-stage defaults**, not the later size-dependent
  `b4soitemp.c` preparation, instance topology, temperature laws, or numerical
  current/charge/noise kernels.

Independent reference test: on Linux with ngspice 47 installed, run:

```sh
cargo test -p model-compact-intrinsic --offline --locked --test ngspice_oracle -- --ignored --nocapture
```

The three live native-oracle cases (default NMOS, default PMOS, and a parameter
batch overriding mobility, oxide thickness and legacy values) each compare
**883 numerical model values** against native ngspice 47 `showmod` output with
six-significant-digit reference tolerance. They all pass (2,649 comparisons).
An isolated temporary working directory is used for every oracle invocation,
so ngspice's parameter-check logs are removed automatically. These explicit
native tests are ignored in ordinary Cargo runs because ngspice is an optional
external executable; source-free unit tests cover the same default dependencies.

## HiSIM-HV1 / HiSIM-HV2 source-derived model preparation

`prepare_hisim_hv_model(&BoundParameters)` selects the actual HV1 or HV2
source-backed default program. The model versions are **not** merged, even where
model names overlap. The Rust tables encode **613 source-order rules for HV1**
and **738 for HV2**, backed by a **shared interned pool of 772 unique rules**
(579 identical definitions shared across the two source programs). The rules
live as a source-derived, version-independent `hv/rules.tsv` data catalog,
parsed **once per process** into typed rule values through `OnceLock`.
A second, separate one-time compilation resolves each family execution step to
its own canonical numeric destination/source slots; subsequent model/candidate
preparations use no rule-name lookup. The runtime Rust file contains only the
independent source-order execution programs. This eliminates duplicated
per-family Rust rule-constructor sequences instead
of suppressing the clone-regression quality gate. Each version retains its
own 613/738 rule index order, source `*Given` branching, copies, and 11
voltage-limit defaults. The data comes from
ngspice 47 `hsmhvset.c` and `hsmhv2set.c` with setter ID-to-slot mapping from
`hsmhvmpar.c` and `hsmhv2mpar.c` (same upstream commit as above).

- Canonical `version=1.24` for HV1 and `version=2.20` for HV2; explicit
  unsupported physical versions are rejected instead of silently resetting
  to a different model. The source dispatcher still recognizes the wider
  version-prefix family selection, independently of physical-version validity.
- Source **`*Given` bits** are preserved through setup: `COSYM=1` copies drain
  parameters to the source side even if earlier defaults populated those
  slots; explicitly given source-side values take precedence. The HV1 legacy
  `LDRIFT` and `RD26` overrides keep their original behavior.
- HV2 `CODEP=1` overrides the applicable earlier defaults; `CORDRIFT=1`
  disables `CORSRD` regardless of its explicit value. Forced model switches
  (including `NOISE`, `COLDRIFT`, obsolete `DLYOV` and `RTH0R`) are handled
  separately from conditional defaults.
- Model-level `LEVEL=73` comes from the parser dispatch, not a fabricated
  physics default. Invalid `CODEP`, `CODDLT`, `CORBNET`, oxide thickness and
  physically nonsensical HV1 `XQY` values are rejected. Missing values with no
  explicit upstream setup rule stay absent, not a made-up zero.
- Outputs are immutable, independent per candidate, and directly indexed by
  canonical numeric slot; the real device equations, temperature-dependent
  operating-point calculation and Jacobians remain separate tasks.

The six independent native reference tests run full `op` and `showmod` on
10 µm × 50 µm HiSIM-HV devices (avoiding invalid effective geometry). They
cover HV1/HV2 NMOS/PMOS, symmetry, depletion, and non-default source/overlap
parameters. They check the prepared values against hundreds of native ngspice
47 C-model outputs, not against generated Rust expectations:

```sh
cargo test -p model-compact-intrinsic --offline --locked --test hisim_hv_oracle -- --ignored --nocapture
```

The tests run ngspice in isolated temporary directories to avoid stray
`*check.log` outputs; the five source-free HV preparation unit tests run in
normal Cargo test CI.

## HiSIM-HV1 / HiSIM-HV2 instance setup and size binning

`prepare_hisim_hv_instance(&PreparedHiSimHvModel, &BoundParameters)` implements
the per-device defaulting in `hsmhvset.c` / `hsmhv2set.c`. Its immutable
`PreparedHiSimHvInstance` holds exact C-source instance values, SPICE IC
voltages `[VDS, VGS, VBS]` (0 to 3 components provided), a binning geometry,
and a model-slot-indexed buffer of the size-dependent parameter values.

- Explicit instance values override model-derived values; the latter include
  the independent body/gate resistance, electrothermal/substrate selectors,
  source/drain drift lengths and overlap lengths. The original `*Given` bits
  are retained on the prepared model to distinguish an explicit model value
  from a filled source default when applying per-instance symmetry.
- HV1 and HV2 **do not share symmetry semantics**. HV1 conditionally
  overwrites `LOVERS` from `LOVERLD` then unconditionally copies `LOVERS`
  to `LOVER`; HV2 conditionally fills `LOVER` from `LOVERLD` and `LOVERS`
  from `LOVER`. Explicit model or instance source-side overrides suppress
  the appropriate symmetric drift copies. The HV1 instance `LOVER` alias
  override remains source-ordered; HV2 instance `SUBLD1/SUBLD2` values are
  rejected because upstream silently inactivates them.
- All remaining unassigned **scalar** instance fields use the zero values
  of the C device allocation, including `OFF`, `NSUBCDFM`, and the unused
  HV2 substrate-length fields. The `IC` parameter stays a genuine optional
  vector and is separately normalized to three initialized voltages.
- Invalid L/W/NF/M, nonpositive corrected binning dimensions, overflowing
  binning divisors/results, invalid per-instance `CORG/CORBNET` selectors and
  ill-formed IC vectors produce explicit errors rather than silent changes.
- Binning follows ngspice's `BINNING(name)` macro exactly for **92 HV1 and
  95 HV2** model parameters enumerated in `hv/binning.tsv`. These are the
  *only* parameters stored in the size-dependent buffer. Other model params
  retain their ordinary prepared-model values:

  `Lgate = L + XL`, `Wgate = W/NF + XW`,
  `Lbin = (Lgate * 10^6)^LBINN`, `Wbin = (Wgate * 10^6)^WBINN`,
  `Pbin = P + LP/Lbin + WP/Wbin + PP/(Lbin*Wbin)`.

  The symbols `LP`, `WP` and `PP` here denote the parameter-specific
  coefficients prefixed with `L`, `W` and `P`, not the particular named
  model parameter `LP`. All coefficients are evaluated in native C `f64`
  precision and looked up by a compiled numeric slot table created once
  per family. The gate dimensions are **not** the later model's effective
  electrostatic channel dimensions.

Six optional independent ngspice 47 `op`/`show` instance tests validate **all
37 numeric scalar fields** exposed by the C instance model for each case:
HV1/HV2 defaults, each version's symmetry and overridden geometry, and
explicit model-`*Given` precedence. That is **222 native instance parameter
comparisons**. These extend the six native model tests (3,979 checked model
values) and three BSIMSOI model tests (2,649 values) to **6,850 comparisons**
across 15 live C reference cases. Seven non-native instance tests validate every
source `BINNING` entry, all four coefficient terms, variant differences,
IC-vector lengths, invalid geometry and candidate independence. The C
`BINNING` arithmetic itself is checked against source-macro algebra and
nontrivial numerical fixtures, **not yet by directly reading ngspice's private
`pParam` structures**.

```sh
cargo test -p model-compact-intrinsic --offline --locked --test hisim_hv_oracle -- --ignored --nocapture
```

These native tests use separate temporary directories; `ngspice` is optional
and the default Cargo suite covers the source-independent requirements.

### Prepared model, instance and temperature provenance

The prepared HV1/HV2 model is the *source* for an instance's size-binning,
effective geometry, transport geometry and inherited instance defaults;
the prepared instance is likewise the source for its current temperature
candidate. **Pairing arbitrary same-family objects was insufficient**: it
could silently combine different valid device equations even though their
`ModelVariant` matched.

Preparation now embeds **private, clone-stable `Arc<()>` identities** at
model and instance creation. Every model+instance entrypoint checks exact
model provenance, and junction diode/charge thermal preparation additionally
checks that the `PreparedHiSimHvTransport` was constructed from that *same
instance*. A mismatched pair returns `ModelError::MismatchedPreparedContext`,
while an actual wrong family still returns `WrongModelScope`. Derived value
`PartialEq` remains unchanged for separate equal-valued cards, and cloning
a prepared model/instance/transport retains provenance correctly. These
private identities require one token allocation per model and instance,
plus a cheap reference-counted handle on each thermal candidate, and no
per-Newton lookups or allocations. Transport is `Clone`, not `Copy`, so its
reference-counted provenance is preserved safely.

Four new source-free tests in `tests/hisim_hv_prepared_context.rs` cover
HV1/HV2 cross-model rejection in all six preparation entrypoints, cross-
instance temperature-state rejection, full valid cloned-context roundtrips,
and rejection of equal-valued **independently prepared** model cards.

## HiSIM-HV effective channel geometry and leading temperature laws

`PreparedHiSimHvInstance::effective_geometry()` exposes source-faithful
metallurgical **channel length, width, lateral-diffusion and capacitance widths**,
as well as total per-finger widths, small-device gate correction, STI width and
channel-length-modulation factor. These values come from the beginning of
`hsmhvtemp.c` / `hsmhv2temp.c`; the earlier binning geometry is a **different**
input expressed in microns. Source `modelMKS` conversion matters:

- `ll_mks = LL / 100^LLN`, `wl_mks = WL / 100^WLN`;
  `length_delta = ll_mks / (Lgate+LLD)^LLN` and
  `width_delta = wl_mks / (Wgate+WLD)^WLN`, with meters as dimensional input.
- `dL = XLD + length_delta`, `dLLD = XLDLD + length_delta`;
  `Leff = Lgate-(dL+dLLD)`. Similarly `dW = XWD+width_delta`,
  `dWLD = XWDLD+width_delta`, `dWCV = XWDC+width_delta`;
  `Weff = Wgate-2*dW`, with separate lateral-diffusion and capacitance widths.
  C rejects nonpositive **Leff/Weff** as fatal; Rust returns an error and
  rejects nonfinite dependent geometry instead of emitting NaN.
- The short-device `WL1/WL1P`, `WL2/WL2P`, `WSTI/WSTIL/WSTIW` and `CLM5/CLM6`
  expressions are calculated using the source **centimeter-scale LG/WG** values.
  This geometry stage is temperature-independent despite living in `*temp.c`.

`prepare_hisim_hv_temperature(model, instance, circuit_temperature_kelvin,
local_self_heating_kelvin)` then supplies a separate immutable
`PreparedHiSimHvTemperature` for the **shared leading subset** of HV1/HV2
`hsmhvtemp_eval.h` / `hsmhv2temp_eval.h`. It preserves the source C constants
(`QE=1.6021918e-19`, `KB=1.3806226e-23`, intrinsic `Nin0=1.04e16 m^-3`) and
keeps *three* temperatures distinct: nominal `TNOM[°C]+273.15`, ambient device
`CKTtemp + DTEMP`, and the latter plus local self-heating. The former two
are never silently replaced by the self-heating value.

- Model/instance-size-binned `EG0`, `BGTMP1`, `BGTMP2` determine the nominal
  bandgap and its temperature-dependent value and derivative, with an explicit
  invalid-domain check before the square root.
- The `EGIG`, `IGTEMP2`, `IGTEMP3` source expressions determine gate-current
  bandgap roots; `QE/(KB*T)` supplies inverse thermal voltage and its derivative.
- `Nin0 * (T/Tnom)^(3/2) * exp(-Eg*beta/2 + Egtnom*betatnom/2)` and its
  source analytic derivative supply the intrinsic carrier concentration.
  Parameter-name resolution is compiled once per model family into an
  eight-slot numeric lookup table with `OnceLock`; subsequent temperature
  evaluations (including per-iteration local thermal rise and trial states)
  perform only direct numeric slot accesses, without string lookup or setup
  allocation. Inputs that produce nonphysical temperatures, negative bandgaps,
  or nonfinite concentrations fail explicitly.
- These are **not** the full physical compact models: mobility temperature
  terms, velocity/current laws, charges, capacitance Jacobians, depletion,
  self-heating state integration, and noise are still outstanding.

Two additional native ngspice-47 integration tests run **six physical-geometry
subcases** across HV1/HV2: fatal negative effective length, fatal negative
effective width, and accepted corrected dimensions, using the actual C error
messages (note that ngspice sometimes returns process exit status 0 even when
OP analysis aborts). They extend the **15 native numeric-comparison cases** to
**17 native tests**, retaining **6,850 direct numerical comparisons**. The
source-math geometry equations are checked with nontrivial model-MKS/binned
parameter fixtures, while **five temperature tests** check nominal bandgap,
separate `DTEMP`/self-heating, source coefficients, physical-domain rejection,
analytic derivatives against independent central differences, and repeated
candidate isolation over 256 temperature evaluations. There are now **29
source-free tests**, plus the 17 optional native oracles.

```sh
cargo test -p model-compact-intrinsic --offline --locked --test hisim_hv_instance
cargo test -p model-compact-intrinsic --offline --locked --test hisim_hv_temperature
cargo test -p model-compact-intrinsic --offline --locked --test hisim_hv_oracle -- --ignored --nocapture
```

## HiSIM-HV mobility, velocity and electrothermal precomputation

`PreparedHiSimHvInstance::transport_geometry()` evaluates the
**temperature-independent** sections of ngspice 47 `hsmhvtemp.c` and
`hsmhv2temp.c`: source/drain STI layout half lengths, DFM (only `CODFM=1`
with an instance `NSUBCDFM` explicitly supplied), phonon mobility with
width/length/area and layout multipliers, surface roughness, `VMAX0`,
`NINVD0`, and thermal-resistance geometry `RTHtemp0`.

- C loops over an **integer** `i < NF` even though NF is real: fractional NF
  uses `ceil(NF)` harmonic-sum terms divided by the original real NF. The
  Rust implementation preserves that behavior up to native C integer bounds.
- `MUESTI1` and binned `VMAX` are converted from the source's centimeter
  values (`/100`), and DFM `NSUBCDFM`/`NSUBC` use cm^-3 to m^-3 conversion
  before logarithms. Source C Given flags control DFM activation.

`prepare_hisim_hv_transport(model, instance, circuit_temperature_kelvin,
local_self_heating_kelvin)` returns a fresh immutable
`PreparedHiSimHvTransport` containing the leading `PreparedHiSimHvTemperature`
and the shared HV1/HV2 transport expressions. One call computes both stages,
preventing stale thermal inputs from being reused with a different instance.

- `MPHN0`, `MPHN1` and their analytic derivatives use binned `MUETMP`
  and the already prepared phonon-scattering coefficient.
- Saturation velocity and its derivative preserve `COTEMP`: all modes use
  heated temperature in the denominator; mode **2** also uses it in the
  numerator, while other modes use ambient `CKTtemp+DTEMP` there.
- `NINVD` has the same numerator branch; its local self-heating derivative
  is zero when `COTEMP != 2`.
- `RTH` and `POWRATIO` use ambient device temperature regardless of local
  heating. `POWRATIO` uses `Fn_SL` then `Fn_SU` smoothing rather than a hard
  clamp. Binned `RTH0` is converted to MKS (`/100`).
- HV2 `CODEP=1` adds depletion phonon/velocity coefficients and derivatives.
  HV1 has no depletion branch.
- Canonical slots are compiled per family with `OnceLock`. Layout powers,
  DFM logs and finger sums run during instance preparation, not each Newton
  temperature evaluation.
- A genuine **passive thermal branch**, `HiSimHvThermalBranch`, is present
  when the prepared instance's `COSELFHEAT` is enabled. Native source
  `hsmhv[2]ld.c` defines `Gth = 1/max(Rth,1e-4)` W/K, `Ith=Gth*deltemp`,
  `Qth=Cth*deltemp`; `hsmhv[2]set.c` and `*temp.c` give
  `Cth=CTH0*100*(M*Weff*NF)` J/K. The branch returns conductance,
  heat capacity, dissipated heat and stored heat; these are the passive
  thermal residual/Jacobian and energy-storage coefficients. The separate
  `evaluate_hisim_hv_generated_heat` function now couples **actual supplied
  drain-current and analytic current-derivative results** to this branch,
  but generating the physical drain current remains the outstanding kernel. A nonzero trial local-temperature rise
  requires an enabled instance `COSELFHEAT`; when disabled, ngspice never
  has a thermal node or a corresponding rise, so the transport API rejects
  that inconsistent input rather than silently evaluating fictitious heating.

### Source-faithful HiSIM-HV generated heat and thermal-row Jacobian

`evaluate_hisim_hv_generated_heat(&PreparedHiSimHvTransport,
HiSimHvDrainBias, HiSimHvDrainCurrent)` consumes a **physical drain current**
`Ids`, its six explicit intrinsic/external voltage derivatives (`gds`, `gm`,
`gmbs`, `gds_ext`, `gm_ext`, `gmbs_ext`) and its self-heating derivative
`gmT`. The model kernel must supply those values in native HiSIM-HV C
coordinates; there is deliberately **no fake or simplified MOS current
model**. The returned `HiSimHvGeneratedHeat` contains the limited power,
seven corresponding chain-rule derivatives, passive-minus-generated DC
thermal residual, and source-exact seven-column thermal Jacobian row.

Both upstream `hsmhvld.c` and `hsmhv2ld.c` use the following rules:

1. If `vds * (vdse-vds) < 0`, use internal `vds` for heat generation.
   Otherwise, if the **binned** `POWRAT == 1` exactly, use external `vdse`
   regardless of temperature-smoothed `POWRATIO`. Otherwise,
   `Veffpower = vds + POWRATIO * (vdse-vds)`.
2. Raw power is `Praw = Ids * Veffpower`. Smooth ceiling is the native
   `Fn_SU(Praw, xmax=SHEMAX*Gth, delta=0.1*Gth)`, with
   `arg=xmax-Praw-delta`, `root=sqrt(arg²+abs(4*xmax*delta))`,
   `P=xmax-(arg+root)/2`, and `dP/dPraw=(1+arg/root)/2`.
   All seven derivative terms use that exact same limiter derivative.
3. The temperature equation is `Ith-P` with dynamic `Qth=Cth*deltemp`.
   The electrical thermal-row columns are source `ydc_t` entries
   `(-dP_dVdse, -dP_dVds, -dP_dVgs-dP_dVgse,
   dP_dVdse+dP_dVgse+dP_dVbse,
   dP_dVds+dP_dVgs+dP_dVbs, -dP_dVbs-dP_dVbse)` for external drain,
   internal drain, internal gate, external source, internal source and
   internal body respectively. Native PMOS multiplies **only the electrical
   columns** by `model->type=-1`; the thermal diagonal is `Gth-dP_dT`
   without that polarity multiplier. Electrical row entries sum to zero
   by voltage-reference invariance. The thermal dynamic diagonal is `Cth`
   and is **not** part of the DC Jacobian.

All geometry, temperature, source parameters and original polarity mode are
resolved before evaluation; per-current power calculation performs no
parameter-name lookups and allocates no buffers. Disabled `COSELFHEAT`
returns no generated-heat branch. Finite input/output domains are checked
explicitly. Six additional source-free tests independently check the
voltage-partition cases, exact `POWRAT=1` with a non-unity smoothed ratio,
`SHEMAX` smooth limit and derivative, complete NMOS/PMOS nodal stamps,
all seven analytic derivatives against finite differences, active ceiling
threshold and invalid-input behavior. These are **source-algebra/chain-rule
regressions**; directly comparing native simulator DC/AC/transient outputs
still requires the real nonlinear HiSIM-HV I/Q kernel and solver integration.

A separate **opt-in native ngspice C oracle**
`tests/hisim_hv_heat_oracle.rs` locates the pinned ngspice 47 source checkout
(commit `972dbf45`), extracts the *verbatim* `Fn_SU` macro and entire native
self-heating power calculation from both `hsmhvld.c` and `hsmhv2ld.c`, then
compiles them independently through the **existing managed `sccache cc`**
into an auto-cleaned temporary executable. It now additionally extracts the *verbatim native C thermal Jacobian-row
assignments* `ydc_t` and preserves source polarity multiplication on the six
electrical columns (not the temperature diagonal). It compares **1,440
source-code cases × 23 native C scalar outputs = 33,120 values** against
Rust: effective power voltage, limited power, six electrical/one thermal
power sensitivities, limiter slope, thermal conductance, heat flow/capacity
and stored heat, **plus all seven native C Jacobian-row entries**. Cases
span four POWRAT/SHEMAX/RTH configurations, three local temperatures, five
internal/external drain-voltage pairs (including reverse and zero crossing),
six current values including zero, negative and power-limited, and both
NMOS and PMOS polarity (720 cases per HV family). It passed
in both C source families independently; source extraction also fails if
the source structure/revision unexpectedly changes. The checkout is located
in `projects/ngspice` or `ngspice` beneath an ancestor of the crate and does
not need an environment variable. Run explicitly:

```sh
cargo test -p model-compact-intrinsic --offline --locked \
  --test hisim_hv_heat_oracle -- --ignored --nocapture
```

This oracle tests **actual upstream C power and derivative statements**,
not a second hand-coded approximation, but does not claim that the full
SPICE device currents or solver thermal integration are implemented.

### Physical HiSIM-HV gate-to-source/drain edge tunneling currents

`prepare_hisim_hv_edge_tunneling(model,instance)` now compiles the **original
independent gate-edge leakage `Igs`/`Igd` subbranches** from ngspice 47 HV1
`hsmhveval.c` and HV2 `hsmhv2eval.c`, gated by the original `COIIGS != 0`
model selector. These are *direct* gate-to-source/drain terms, not the
additional inversion-induced `Igate`, Fowler-Nordheim `Ifn`, gate/body
`Igb`, or complete external terminal gate current (whose native stamping
combines and signs them according to channel mode and `GLPART1`).

The original preparation sources `hsmhvset.c` and `hsmhv2set.c` convert
size-binned `pParam.GLKSD1 = bin(GLKSD1)/100`, `pParam.GLKSD2 =
100*bin(GLKSD2)` and `modelMKS.GLKSD3 = 100*model.GLKSD3`. Native
source bias equations are `A = (GLKSD1_MKS/1e6)*Weff*NF`,
`Igs_raw = A*exp(TOX*(GLKSD3_MKS-GLKSD2_MKS*Vgs))*Vgs²/TOX²`
and `Igd_raw` with `Vgs-Vds` substituted for `Vgs`. Original C
**reverses the sign for nonnegative respective voltage**. Both have
analytic `dI/dVgs`, and the drain-side `dIgd/dVds = -dIgd/dVgs`;
body/temperature slopes are exactly zero. The returned
`HiSimHvEdgeTunnelingCurrents` contains source and drain direct terms with
instance `M` scaling. Their original sign is retained as `Igs`/`Igd`;
solver coupling will add the native terminal partition signs later.

The original binned-current densities and exponent coefficients are
compiled once per fixed device geometry/model, leaving a trial fast path
with no parameter lookup, binning, geometry processing or allocation. Three
Rust tests cover HV1/HV2, derivative verification by central differences,
selector gating, native `M` scaling, positive/negative/zero biases, and
nonfinite-domain rejection. The optional original-source C oracle
`tests/hisim_hv_edge_tunneling_oracle.rs` extracts the **literal complete
`Igs` and `Igd` branches** and the corresponding 3 source unit-conversion
statements and `COIIGS` guard from pinned ngspice 47. Its managed `sccache
cc` C harness streams inputs concurrently with stdout draining to prevent
pipe backpressure, cleans temporary compilation files and validates **4,320
independent native-C cases × 10 current/conductance/body/thermal fields =
43,200 numeric comparisons**. Cases span both source families, three
GLKSD model sets, two oxides, three widths, two M multipliers, both COIIGS
modes, six signed internal gate biases and five internal drain biases.

```sh
cargo test -p model-compact-intrinsic --offline --locked \
  --test hisim_hv_edge_tunneling_oracle -- --ignored --nocapture
```

Overall reference coverage is now **65 source-free Rust tests**, **23 native
reference tests**, and **95,806 original-source/executable numeric
comparisons**, covering multiple *physical subcomponents* rather than full
self-consistent nonlinear MOSFET I/Q or solver-level DC/AC/tran parity.

### Additive HiSIM-HV body/lateral-field terminal charge

`prepare_hisim_hv_lateral_field_charge(model, instance)` now prepares the
**complete independent `XQY1/XQY2` body-bias-induced lateral-field charge
increment** from the pinned original ngspice HV1/HV2 source files. This is
one additive part of the larger `Qy` physical charge equation, *not* a
replacement for the remaining nonlinear channel/drift charge laws.

Original HV1 `hsmhvtemp.c` and HV2 `hsmhv2temp.c` prepare
`cqyb0 = 1e6 * Weff*NF * XQY1 / pow((L+XL)*1e6, XQY2)` using the
original micrometer gate-length conversion; this correction uses
**internal `Vbs`** rather than junction `vbs_jct` or external body bias.
Native `hsmhveval.c`/`hsmhv2eval.c` add `Qy += cqyb0*Vbs` and
`dQy/dVbs += cqyb0` only if `XQY1!=0`. Native `COADOV==1` then adds
`M*Qy` to the gate and `-M*Qy` to the drain, with equal-and-opposite
body-voltage derivatives. The prepared result exposes the raw coefficient
and correctly `COADOV`-gated, `M`-scaled coefficient; its `evaluate(&self,
internal_vbs)` performs one multiply and returns both conservative charge
increments plus the analytical gate-body derivative. The preparation has
no per-Newton model lookup or dynamic power law.

Three new source-free Rust tests cover both original versions,
positive/negative XQY1, exact charge conservation, body-capacitance
finite differences, disabled `COADOV`/zero XQY1, and nonfinite voltage
rejection. The **independent original-C oracle**
`tests/hisim_hv_lateral_field_oracle.rs` extracts actual `cqyb0` geometry,
`Qy`-increment and **gate/drain charge/Jacobian stamp statements** from
both original ngspice 47 model files. Compiled using managed `sccache cc`
into autocleaning temporary files, it passes **864 original C cases × 7
coefficient, charge and Jacobian values = 6,048 numerical comparisons**:
two model versions, three XQY1 values (zero/positive/negative), three
XQY2 exponents, three geometries, two M multipliers, both COADOV modes,
and four internal body biases. Run explicitly:

```sh
cargo test -p model-compact-intrinsic --offline --locked \
  --test hisim_hv_lateral_field_oracle -- --ignored --nocapture
```

The crate now has **65 source-free Rust tests**, **23 opt-in native reference
tests**, and **95,806 independent source/executable numerical comparisons**.
These are physical additive component checks rather than complete intrinsic
MOS channel I/Q or operating-point/transient solver conformance.

### Physical HiSIM-HV1/HV2 external fringing capacitance

`prepare_hisim_hv_fringing(model, instance)` now prepares a real, original
HiSIM-HV source/drain fringing capacitor pair once per stable device. The
result `PreparedHiSimHvFringing` contains C's unscaled `cfrng`, the
instance-multiplied and model-`COADOV`-gated capacitance, and the original
output-gate status. Its `evaluate(Vgse,Vdse)` computes native
`Qfd=Cfrng*(Vgse-Vdse)` and `Qfs=Cfrng*Vgse`, including `M`. Both use
**external** gate/source and drain/source voltage coordinates. In C, these
positive gate-oriented charges are added to the drain/source-prime rows with
*negative sign*, and the Jacobian contributions are `+C` on the drain row's
external drain voltage and `-C` on both drain/source-prime rows' external gate
voltage. Both local thermal derivatives vanish. Model `COADOV!=1` disables
the native output *stamping* without deleting the geometrically prepared
coefficient, preserving the original output path.

The static coefficient is taken exactly from ngspice 47 HV1 `hsmhvtemp.c`
and HV2 `hsmhv2temp.c`: `cfrng = C_EOX / (C_Pi/2) * Weff*NF *
ln(1 + TPOLY/TOX)`, with **original `C_EOX=3.453133e-11 F/m`** and
original π precision. This is distinct from nonlinear `CGSO`/`CGDO` overlap
charge, which remains part of the pending intrinsic physical kernel. Model
`TOX/TPOLY`, computed `Weff*NF`, instance `M`, and `COADOV` are resolved at
preparation, with only two scalar multiplies on the trial-voltage path.

Three source-free Rust tests cover both variants, finite-difference analytic
bias derivatives, zero stamped charge with `COADOV=0`, multiplicity, and
nonfinite terminal voltage rejection. The **verbatim source-C oracle**
`tests/hisim_hv_fringing_oracle.rs` extracts both original capacitance and
charge statements (plus source output-gate condition), compiles with managed
`sccache cc` into auto-cleaned temporary files and verifies **432 C cases ×
six raw/stamped coefficients and charges/derivatives = 2,592 numeric matches**
for two families, three oxide/poly configurations (including zero poly),
three widths, three multipliers, both output modes and four bias pairs.
Run `cargo test -p model-compact-intrinsic --offline --locked --test
hisim_hv_fringing_oracle -- --ignored --nocapture` against the pinned ngspice
47 source checkout.

The crate now has **65 source-free Rust tests**, **23 opt-in native reference
tests** and **95,806 numeric reference comparisons** spanning the original
model preparation, electrothermal power/Jacobian, substrate junction Q/C,
physical diode I/G, and external fringing components. This component-level
coverage does not imply full MOS channel I/Q or circuit-solver parity.

### Physical HiSIM-HV source/drain junction diode currents

`prepare_hisim_hv_junction_diodes(model, instance, transport)` now compiles
all temperature-dependent saturation/leakage, forward-transition and bias-
independent coefficients once for the current candidate temperature. The
returned `PreparedHiSimHvJunctionDiodes::evaluate(bias)` then computes the
**original physical substrate-drain and substrate-source diode
I/V laws** for both HiSIM-HV families, returning `Ibs`/`Ibd`, analytic
`dI/dV` (`Gbs`/`Gbd`) and local `dI/dT` in
`HiSimHvJunctionCurrents`. Source files are the full upstream ngspice 47
HV1 `hsmhvtemp_eval.h` and `hsmhveval.c` PART-4 diode branches, and HV2
`hsmhv2temp_eval_dio.h` and `hsmhv2eval_dio.c`. These diode currents are
physically real source components, distinct from the still-pending complete
intrinsic MOSFET Ids and terminal charges.

- Separate original source/drain area × junction current density plus full
  perimeter × sidewall density, and **second** saturation set with `XTI2`.
  Current densities use the source's exact nominal/device bandgap, inverse
  thermal voltages and historical electron-charge/Boltzmann constants:
  `J(T)=JS0*exp((Egtnom*betatnom-Eg(T)*beta(T)+XTI*ln(T/Tnom))/NJ)`.
  HV1 uses shared JS0/NJ/VDIFFJ and reverse coefficients with independent
  junction areas/perimeters; HV2 preserves independent drain/source binned
  `JS0D/JS0S`, `NJD/NJS`, `VDIFFJD/VDIFFJS`, `CISBKD/CISBKS`, and original
  per-side `XTI`, `CTEMP`, `CVB`, `CISB`, `DIVX` parameters.
- The forward-voltage continuation is the native
  `Vt=(NJ/beta)*ln(1+VDIFFJ*(T/Tnom)^2/(Is+1e-50))`. Below Vt, the diode
  exponential has exactly the original extreme reverse exponent cutoff:
  `-34` for HV1 and `-102` for HV2. Above Vt the source linear tangent
  continues the exponential with a temperature-dependent tangent and
  transition voltage; the mathematical derivatives account for movement of
  the boundary with local temperature.
- Reverse leakage is the original temperature-corrected secondary
  saturation term `Is2*CISB*exp(CTEMP*(T/Tnom-1))`, exponential voltage
  tails involving `CVB` and (HV1) `CVBK`, the direct `CISBK` term, and
  additional `DIVX*Is2*V` continuation. HV1 and HV2 have different source
  reverse-exponential limiting rules, retained individually.
- The analytic `dI/dT` is nonzero only for an enabled thermal node **and**
  nonzero binned `RTH0`, exactly as the source's setup/evaluation split.
  When inactive, ambient `CKTtemp+DTEMP` is used and local dI/dT is zero.
  Native `M` multiplication is applied to both current and conductance and
  temperature sensitivity. Extra **solver `gmin` is not a diode property**,
  so it is deliberately excluded.
- A numeric slot table is compiled once per family; **all temperature and
  instance-dependent coefficients, including `exp`/`log` terms**, are prepared
  once for a fixed temperature candidate. The per-Newton voltage-only
  `evaluate` call touches no parameter slots, model/instance input objects or
  temperature-exponential prelude. Reprepare when self-heating changes
  temperature; reuse indefinitely for repeated ambient-temperature DC bias
  evaluations. Real physical input-domain
  violations and nonfinite results propagate as explicit model errors.

Four additional source-free diode tests exercise both original source/drain
families, zero-area default behavior, analytic voltage and local temperature
derivatives checked independently via central differences across forward and
reverse biases, the no-selfheat and binned RTH0=0 branches, HV2 source/drain
asymmetry, and multiplier scaling. The **opt-in verbatim upstream C oracle**
`tests/hisim_hv_diode_oracle.rs` extracts both full temperature-dependent
junction-saturation prelude blocks and the entire original C diode I/V
sections, then compiles them with **managed `sccache cc`** in a self-cleaning
`tempfile` directory. It directly matches **378 actual C cases × six native
outputs = 2,268 numerical comparisons**: source/drain current, conductance,
and local temperature sensitivity. Coverage includes three parameter sets,
three heated temperatures, three source/drain geometries including zero,
seven signed voltage combinations, and both versions. Run:

```sh
cargo test -p model-compact-intrinsic --offline --locked \
  --test hisim_hv_diode_oracle -- --ignored --nocapture
```

The crate now has **65 source-free Rust tests**, **23 opt-in native
reference cases**, and **95,806 direct numeric reference comparisons** across
three disjoint physical components and original setup/reference data. These
are *component numerical conformance*, not yet full self-consistent MOSFET
DC/AC/transient solver parity.

### Physical HiSIM-HV source/drain junction depletion charge

`prepare_hisim_hv_junction_charges(model, instance, transport)` now
compiles area, sidewall/gate-edge geometry, C0, MJ, PB, TCJ and temperature
corrections once for a fixed thermal state. The returned immutable
`PreparedHiSimHvJunctionCharges::evaluate(bias)` computes both **real** bulk-source/bulk-drain junction *depletion* charge
branches from the full original HV1 `hsmhveval.c` PART-4 source and HV2
`hsmhv2eval_dio.c` charge section. `HiSimHvJunctionCharges` returns source
and drain `Q`, the analytic local `dQ/dV` (`Capbs`/`Capbd`) and `dQ/dT`
(`Qbs_dT`/`Qbd_dT`). These are genuine physical charge components; the
independent diode I/V currents and intrinsic MOS channel charges remain to
be implemented, not substituted by a generic approximate MOS equation.

The source and drain have separate area (`AS/AD`), perimeter (`PS/PD`),
multiplier (`M`), and temperature coefficients. The original geometry
branch is exact: if junction perimeter exceeds `Weff*NF`, ordinary sidewall
uses `perimeter-Weff*NF` and gate-sidewall uses `Weff*NF`; otherwise ordinary
sidewall disappears and gate-sidewall uses the entire perimeter. The
model-catalog parameters differ between HV1 (shared `CJ/MJ/PB` families)
and HV2 (separate source/drain `CJS/CJD`, `MJS/MJD`, `PBS/PBD` families).
The temperature-dependent zero-bias capacitances are
`C0*(1+TCJ*(TTEMP-KTNOM))` with `dC0/dT=C0*TCJ`. Negative junction voltage
uses source `Pb*C0*(1-(1-V/Pb)^(1-Mj))/(1-Mj)`, with the source's special
`Mj==0.5` square-root branch and its original *nonpositive capacitance*
behavior. Nonnegative junction voltage uses the exact quadratic continuation
`Q=V*(C0+V*C0*Mj/(2*Pb))`. Both branches return analytic `dQ/dV` and
`dQ/dT`; all outputs are multiplied by the native instance `M`.

For active self-heating, the junction charge evaluator uses heated device
temperature only if the original size-binned `RTH0 != 0`; otherwise it uses
`CKTtemp+DTEMP`, matching the upstream `hsmhv[2]temp_eval_dio` selection.
Family-specific numeric parameter slots are compiled once and reused for
repeated candidate evaluations, with no string lookup in the physical math.

Three source-free `hisim_hv_junction.rs` tests verify zero-capacitance
geometry and nontrivial asymmetric junctions with source/drain voltage
and thermal derivatives against independent finite differences, for both
versions, including immutable-state reuse across 256 bias evaluations.
The **opt-in verbatim-native-C reference**
`tests/hisim_hv_junction_oracle.rs` extracts the original, complete
~9.5-kB junction charge branches from both C source files, builds them with
managed `sccache cc` into auto-cleaning `tempfile` locations, and compares
**288 C cases × 6 charge/capacitance/thermal fields = 1,728 direct values**.
It varies the source families, zero versus nonzero `RTH0`, three local
thermal rises, four geometries/multiplicities and six reverse/forward
voltage pairs. This independent C reference passes. Run explicitly:

```sh
cargo test -p model-compact-intrinsic --offline --locked \
  --test hisim_hv_junction_oracle -- --ignored --nocapture
```

The crate now has **65 source-free integration tests**, and 23 opt-in
native-reference tests: 17 full ngspice executable tests, one native-C
power/Jacobian oracle and one native-C junction Q oracle. The comparisons
establish fidelity for *these components*, not full nonlinear MOS I/Q or
DC/AC/transient solver parity. Both source-based oracles read the pinned
ngspice 47 C revision, reuse managed sccache and leave no source-tree
build artifacts.

Nine source-free transport tests verify DFM and layout formulas, fractional NF,
all COTEMP branches, finite-difference derivatives, HV2 depletion, RTH and
power smoothing, the native `1e-4` RTH conductance floor, exact `CTH0` geometric
MKS scaling, enabled/disabled passive thermal branches, positive/negative
thermal rises, physical-domain rejection and trial-candidate isolation.
The crate now has **65 source-free integration tests** and retains **17
independent native ngspice 47 tests** with **6,850 direct numerical
model/instance comparisons** and physical-geometry acceptance/rejection.
Native oracles do **not** inspect private mobility/velocity/RTH fields;
full current/charge/thermal fidelity awaits physical nonlinear kernels.

## Reference test coverage

`tests/fixtures/*.names` contains **only parameter names** extracted from eight
public ngspice reference cards or QA parameter sets:

- `examples/soi/bsim4soi/{nmos4p0,pmos4p0}.mod`
- `tests/bsim3soifd/nmosfd.mod`, `tests/bsim3soidd/nmosdd.mod`,
  `tests/bsim3soipd/nmospd.mod`
- `tests/hisim/nmos/parameters/nmosParameters`
- `tests/hisimhv{1,2}/nmos/parameters/nmosParameters`

The parser's `LEVEL` selection attribute is not a `B4SOI` model-parameter slot.
The upstream HiSIM-HV v1 QA parameter list also contains **17 names absent** from
its own C parameter table but registered by v2: `coerrrep`, `gdl`, `gdld`, `gdlp`,
`ibpc1l`, `ibpc1lp`, `pt2`, `pt4`, `pt4p`, `ptl`, `ptlp`, `ptp`, `subld1l`,
`subld1lp`, `xpdv`, `xpvdth`, and `xpvdthg`. The test asserts this exact mismatch
rather than expanding HV1 with unregistered names.

Run the focused gates from the workspace root with the managed sccache toolchain:

```sh
cargo test -p model-compact-intrinsic --offline --locked
cargo clippy -p model-compact-intrinsic --offline --all-targets -- -D warnings
cargo fmt --all -- --check
```

## Implementation sequence

1. Expand representative *actual PDK* model-card/mode/selector coverage, then
   prioritize HiSIM-HV and BSIMSOI numerical packages against measured usage.
2. Extend defaults and selector checks to HiSIM2, BSIM3SOI and SOI3;
   implement BSIMSOI per-instance preparation, and **temperature-dependent,
   remaining electrothermal and topology/state preparation** for both
   source-backed HiSIM-HV families. Verify real selector/parameter batches
   before building the numerical kernels; HV1/HV2 instance defaults, binning,
   effective geometry and common leading temperature laws are implemented.
3. Port and validate each family's **real physical I/Q/thermal/noise laws** under
   its upstream license, with analytic/autodiff derivatives and conservation checks.
   Do not create approximation subcircuits or map to a weaker MOS family.
4. Compare DC, AC, transient and noise outputs against independent ngspice 47
   reference models. Test Jacobians numerically under physically meaningful biases,
   charge conservation, electrothermal feedback and accepted/trial state isolation.
5. Export batch-friendly typed kernel views through the common artifact API in
   `model-kernel-api` for the generic
   solver adapter in
   [ngspice-rs-fast #189](https://github.com/codeandsolder/ngspice-rs-fast/issues/189).
   No string lookup, parameter preparation or topology reconstruction in Newton loops.

The upstream `DEVICES` manifest marks HiSIM2 and HiSIM-HV as *to be tested*,
BSIM3SOI DD/PD and SOI3 as obsolete, BSIM3SOI FD as *to be tested*, and
BSIMSOI 4.x as active. These are upstream statuses, not our integration status.

### Metadata deduplication

The ngspice snapshot's `B3SOIFD` and `B3SOIDD` registered parameter tables are
byte-identical after stripping their source-file comments, including canonical
slot/alias identity; they therefore share static model and instance schemas.
Likewise the v1/v2 HiSIM-HV **instance** tables are identical and shared, while
their **model** tables remain version-specific. Physical equations, defaults and
state structures are **not** assumed interchangeable just because the registration
metadata matches; future kernels must preserve their exact source-family behavior.


### BSIM3SOI FD/DD/PD setup and instance preparation

`bsim3soi::prepare_model` evaluates the ordered default programs shared by FD
and DD (2.1), or the distinct PD 2.2.3 program. It preserves original given
flags for later size-dependent derivation. In FD/DD the registered XDIF name
aliases XBJT, whereas the internal diffusion exponent defaults to 2. PD has
a separate XDIF input with an XBJT-dependent default.

`bsim3soi::prepare_instance` supplies source defaults, five-component initial
conditions, effective IV and CV dimensions, and the raw length/width/cross
bin program. PD retains its independent CV corrections, body-contact widths,
segment geometry and body/backgate CV lengths. Deferred physical coefficients
remain distinguishable from explicit zero. These are preparation APIs, not
a complete current/charge kernel.

The ignored native setup test compares 2,422 model and 100 instance getter
values across FD/DD/PD NMOS and PMOS cases against ngspice 47. Six FD/DD native
mask getters (LDVBD0/1, WDVBD0/1, PDVBD0/1) return the corresponding base DVBD
rather than the stored coefficient. The test checks the actual getter
mapping separately from the source default of 1 for each bin coefficient.
Eight size-derived model fields are intentionally deferred. Native showmod
values are compared only where the source exposes a usable getter.

Source references: `bsim3soi_{fd,dd,pd}/b3soi{fd,dd,pd}{set,mpar,par,temp,mask}.c`.
