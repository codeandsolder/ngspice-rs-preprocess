# ngspice-rs-preprocess

Standalone SPICE normalization, dialect lowering, source linking,
parameter compilation and reusable prepared model kernels.

The workspace contains the ngspice normalizer, SIMPLIS source preprocessor and
translator, and prepared diode, VDMOS, bipolar and intrinsic compact-model
packages. Experimental ALB/XSPICE
compilation lives in the separate `model-alb-experimental` crate and is
excluded from default workspace builds. The production translator does not
link it and rejects unsupported source semantics explicitly.

The simulator owns numerical circuit orchestration. Source admission is
handled by `model-preprocess-command` before normalization, linking or model
preparation. The default does not launch any external process.

## Distribution

See [standalone package qualification](docs/package-release.md) for release
metadata, licenses, and registry-format verification. The tracked workspace
remains publication-disabled until a reviewed and authorized release.

## Unified library and CLI

Use the `ngspice-rs-preprocess` package for source preparation. The library
entry point `prepare(&Request, &Options, &Cancellation)` returns a
`PreparedBundle` with exact artifact bytes, companion identities and
provenance. It does not create files or execute a numerical analysis.
`SourceInput::File` and `SourceInput::Bytes` both carry an original path;
in-memory sources require an existing parent directory. Each call owns one
source-admission session. A frontend that already has a native parser can
instead use the re-exported `SourceSession` for admission alone.

The CLI invokes the same function:

```text
ngspice-rs-preprocess FORMAT INPUT OUTPUT_DIRECTORY [OPTIONS]
```

| Format | Meaning | Primary artifact |
| --- | --- | --- |
| `native-deck` | Native-compatible circuit; link static includes and preserve title/analysis/terminator | `deck.cir` |
| `native-library` | Native-compatible source library; link static includes and make include-safe | `model.lib` |
| `hspice-deck` | Link standalone HSPICE sources and bind qualified parameter/model occurrences | `deck.cir` |
| `hspice-library` | Compile a neutral HSPICE source graph for later deck binding | `library.json` |
| `pspice-deck` | PSpice plaintext circuit; link libraries and lower qualified syntax | `deck.cir` |
| `pspice-library` | PSpice plaintext models; include-safe native source | `model.lib` |
| `qspice-deck` | QSPICE plaintext circuit; link libraries and normalize qualified source semantics | `deck.cir` |
| `qspice-library` | QSPICE plaintext models; include-safe native source | `model.lib` |
| `tina-deck` | TINA/TINA-TI plaintext SPICE export, with local parameter normalization | `deck.cir` |
| `tina-library` | TINA/TINA-TI plaintext SPICE macro source | `model.lib` |
| `ltspice-deck` | Explicit LTspice source normalization (partial dialect coverage) | `deck.cir` |
| `ltspice-library` | Explicit LTspice library normalization (partial dialect coverage) | `model.lib` |
| `ltspice-schematic` | Headless ASC/ASY schematic and hierarchical symbol import through LTspice normalization | `deck.cir` |
| `simplis-deck` | Select SIMPLIS projection, specialize hierarchy and lower electrical primitives | `deck.cir` plus referenced companions |
| `simetrix-deck` | Select SIMetrix projection, specialize hierarchy and lower electrical primitives | `deck.cir` plus referenced companions |
| `expanded-deck` | Lower already expanded dialect source without hierarchy specialization | `deck.cir` |
| `expanded-library` | Lower already expanded dialect source as an include | `model.lib` |
| `simplis-library` | Compile a SIMPLIS library projection | `library.json` |
| `simetrix-library` | Compile a SIMetrix library projection | `library.json` |

QSPICE preparation supports linked plaintext sources, scoped scalar
declarations, numeric spellings, explicitly ideal passives, and native or
occurrence-bound PWL/PULSE waveforms. The typed waveform metadata is mandatory
when present; the source card may contain a placeholder DC bias. Known
unsupported source laws and topology directives have physical-source
diagnostics. See [the QSPICE adapter contract](docs/qspice.md) for the
qualified behavior, boundaries and reference evidence.

TINA support targets plaintext SPICE exports, not TINA schematic/project
or mixed-language macro containers. Export the electrical netlist first.
The documented PSpice-style syntax uses the same source linker and lowering pipeline as the explicit PSpice dialect;
a local `.PARAM` appended to a `.SUBCKT` header is split into a scoped
directive without changing source location. Formal `PARAMS:` defaults are
kept separate. Source metadata in comments remains intact. See
[TI SLVA527](https://www.ti.com/lit/an/slva527/slva527.pdf) and
[DesignSoft TINA manual](https://www.tina.com/docs/v12/Tina-v12-manual.pdf).
The inline declaration is observed in the public ISO224X export discussion
([reference](https://electronics.stackexchange.com/questions/518473/error-importing-third-party-model-to-ltspice-from-tina));
the regression fixtures are independently authored and contain no vendor model text.

The LTspice formats normalize whole-file library imports, relative Windows-style paths, `noiseless` resistor flags, `.backanno`, scalar intrinsics and operator precedence, runtime versus parameter-phase evaluation, endpoint-clamped numeric tables (including phase-specialized macros), explicit capacitor/inductor series resistance, the uncoupled inductor default series resistance, closed nonnegative voltage-source series resistance, and explicit transient timing (including omitted or zero plotting step). PULSE, PWL, SINE and explicitly timed EXP share occurrence-bound prepared waveform declarations with the native source binder. Relative PWL times are accumulated, signed scales applied, and duplicate-time ordinates averaged before interpolation. Closed waveforms with exact native representations are emitted directly; symbolic coefficients or finite sine trains require the typed waveform manifest, which separates explicit DC bias from transient startup. These passes preserve source provenance and do not require a simulator-wide compatibility mode.

The source-specific transformations have native ngspice OP/DC/AC/transient/noise regressions based on independent LTspice XVII and 24.0.12 measurements. Successful normalization alone does not establish equivalence for arbitrary decks. All LTspice diode models retain an explicit `ltspice_diode` dialect marker and their ordered setters, including lexical LEVEL expressions; the consumer selects the constitutive law after occurrence-specific resolution. Voltage-controlled switch models select an explicit native physical family. The diode marker is a breaking producer/consumer contract coordinated with [solver #258](https://github.com/codeandsolder/ngspice-rs-fast/issues/258); it is not raw ngspice syntax. See [diode dispatch](docs/ltspice-diode-dispatch.md). Passive IC and richer parasitic networks, structured/triggered waveforms, analysis-dependent EXP timing defaults, source shunt capacitance, negative or occurrence-parametric voltage-source series resistance, and unsupported expression forms require additional typed preparation or solver support. End-to-end Rust solver acceptance of the generated scalar functions and mandatory waveform declarations depends on its native parameter-expression, macro-expansion and prepared-waveform binding support.

For schematic import, supply the root ASC and any symbol/model search roots.
The headless importer resolves electrical geometry and symbol metadata,
including hierarchical blocks, without invoking the graphical editor.
Installation-specific symbols and component libraries remain external
resources. See [the schematic import contract](docs/ltspice-schematic.md).

No extension-based format guessing or implicit transformation is performed.
For example:

```sh
ngspice-rs-preprocess native-deck input.cir prepared --search ./models
ngspice-rs-preprocess simplis-library parts.lb compiled
ngspice-rs-preprocess simplis-deck circuit.net translated --parsed-library compiled/library.json
ngspice-rs-preprocess native-library model.lib normalized \
  --command /absolute/path/to/adapter --command-arg --mode --command-arg raw
```

Use repeatable `--library PATH` for raw catalog members, or
`--parsed-library PATH` for previously compiled JSON artifacts. Explicit
source paths resolve relative to the importing source first, then by
matching path suffix and unique portable basename; ambiguous includes fail.
Content-equivalent libraries retain all admitted source names. Identical
programs at different paths do not make their relative includes interchangeable.
A compiled artifact is never passed to the source command.

For SIMPLIS, an explicit catalog include executes its selected source program
in the current preprocessing scope and in source order. Only active conditional
and loop occurrences execute; a nested include resolves relative to its own
origin. Local variables remain local, while promoted globals are visible to
subsequently specialized subcircuits. Active model cards and aliases are emitted
once at the declaration scope; occurrence-specific declarations are specialized
with their caller's bindings. Merely adding a catalog member does not execute
its root control program. The parsed-library v8 format preserves the selected
source program separately from its indexed declarations and retains physical
source positions. Regenerate older parsed artifacts before using them.

Recursive relative `.init` resources are admitted in the same session and
only in the selected simulator projection. Missing, ambiguous and
recursive resources are errors. Dialect translation emits only the native
electrical circuit and known generated libraries; attach the desired
analysis separately. Residual external includes are rejected rather than
silently referring to the original working directory. Experimental ALB
compilation remains an explicit specialized workflow, not an automatic
fallback from the unified entry point. Build its CLI tools explicitly with
`cargo build --locked -p model-alb-experimental`; see the
[experimental runtime guide](runtime/xspice/README.md).

SIMetrix decks preserve runtime argument binding, including implicit formals,
source-order actuals, callee defaults and isolated local scopes. Direct and
nested electrical model dependencies are resolved against declared catalog
models, including optional substrate/bulk terminals and passive model/value
forms; ambiguous model positions are rejected. Bare passive values resolve
to visible parameters only when no visible model has that name. Charge
expressions preserve dynamic probe and temperature semantics independently
of instance parameter bindings; capacitor temperature coefficients lower
to the native two-coefficient form.

SIMetrix source absolute-base exponentiation (`**` and `PWR`) preserves
source associativity, unary operands, the zero-base Jacobian and true
negative-power singularities. Lowering is performed before cross-language
source linking, within the existing expression expansion budget. Native
signed power and HSPICE/PSpice expression contexts remain distinct.
`LIMIT` clamps between either ordering of its two bounds.

Nominal SIMetrix preparation lowers PI, E and the historical BOLTZ/ECHARGE
constants, stable LNCOSH and lazy nominal GAUSS(x) (always 1). Predefined
parameters cannot be redeclared and electrical node names are not rewritten
as parameter symbols. Static TEMP binds to the single declared analysis
temperature (default 27 °C); behavioral TEMP becomes native live temperature.
Model T_ABS on diode, BJT and resistor cards is moved to matching device
instances in model scope, preserving explicit instance TEMP precedence.
A different nominal temperature requires preparing the source again.
Statistical analyses and conflicting or multiple analysis temperatures
are rejected rather than approximated by the nominal translation.

SIMetrix discrete MOS level 17 cards use explicit absolute geometry and a
native MOS level-1 channel and junction core, with series-resistance
multiplicity and temperature binding. The nonlinear gate-drain capacitance
is lowered as conserved terminal charge (not capacitance times voltage).
Model references resolve across linked artifacts and lexical scopes; local
native model declarations shadow inherited discrete MOS models. Independent
DC, transient and complex AC measurements qualify both polarities,
resistance temperature coefficients, multiplicity, and junction and gate
charge. Native semiconductor constants differ slightly from the historical
source constants. Unsupported geometry and nonconstant instance fields
are rejected; source SOA annotations are retained as metadata, not enforced
by the native MOS core.

### Standalone HSPICE

Select `hspice-deck` explicitly; no ngspice compatibility flag is required.
Raw linked includes and named library sections inherit the including
resource's lexical dialect, unless a local source header overrides it.
The compiler preserves the admitted logical statement's physical source
span, including continuation lines. A compiled HSPICE library is neutral
source, not a prematurely lowered native include:

```sh
ngspice-rs-preprocess hspice-library devices.sp catalog --search ./models
ngspice-rs-preprocess hspice-deck circuit.sp native --parsed-library catalog/library.json
```

Raw catalog members and compiled members use the same occurrence binding.
The root deck owns `PARHIER`, `XMULT_IN_EXP`, temperature defaults and
model-selection policies. Source parameter and model aliases preserve
electrical terminal names, nested multiplier products, and source spans.
Mixed scalar/function `.PARAM` declarations and `.FUNC` macros use the same
occurrence binding: `PARHIER` selects visible declarations, free symbols
resolve in the calling occurrence and formals shadow parameters. Native
function aliases retain nested calls and electrical probes without expanding
expression trees in the producer. Cyclic and out-of-scope calls are diagnosed.
Native output retains qualified OP, source DC, AC and single-interval
transient commands. The supported model subset is nongeometric level-1
junction diodes (including AREA/PJ, breakdown and depletion charge) and
level-54 Berkeley BSIM4 versions 4.5.0, 4.6.4 and 4.7.0 with the qualified
ACM=12 junction mode. Geometry bins use source-order, lexical model-family
resolution and half-open length/width intervals, with XLREF/XWREF and
WNFLAG, rather than native fuzzy bin matching. Version-specific parameter
inventories reject unknown fields instead of allowing a native parser to
ignore them.

This is not blanket HSPICE equivalence. Other physical variants,
non-HSPICE model cards inside an overridden resource,
controlled transfer extensions, statistical or multi-interval analyses
and unqualified output directives are diagnosed rather than guessed.
See issues [#68](https://github.com/codeandsolder/ngspice-rs-preprocess/issues/68),
[#69](https://github.com/codeandsolder/ngspice-rs-preprocess/issues/69),
[#70](https://github.com/codeandsolder/ngspice-rs-preprocess/issues/70)
and the [typed analysis schedule work](https://github.com/codeandsolder/ngspice-rs-fast/issues/237).
The focused HSPICE tests compare authored circuits with documented
equations and independently selected native ngspice references; they
do not use a global compatibility mode or claim proprietary simulator
equivalence.

### Source model selection

SIMetrix model selection is request-local. The typed mapping key is the
case-insensitive model type and declared level (zero denotes omission).
The selection layers are built-in defaults, `HSPICEMODELS`, an optional
explicit device configuration (`--device-config PATH`), and deck `.MAP`
declarations, in increasing priority. Conflicting declarations within a
layer are rejected. Simulation-wide mapping directives in conditional or
subcircuit scopes are rejected until their global evaluation semantics
are qualified. The source-language header controls expression and
lexical semantics independently of model selection. The mapping table is
shared across the deck's admitted includes; indexed catalog libraries
cannot independently introduce global selection directives. Include such
resources explicitly instead. Parsed-library format v8 retains projected
source statements so that a catalog can be selected again under a new
request's mapping rather than baking in a previous request's defaults.

For example, `.MAP ModelName=D Level=1 Device=Diode3` selects the
level-3 diode implementation for cards declared `D LEVEL=1`. The
manifest's `model_selections` list records original projected card text,
source identity, lexical scope, dialect, declared and effective version,
and selection origin. Model selections retain the admitted physical line
span, including continuations. A new alias requires an explicit `LETTER`.
The analysis plan records consumed model options with the
`model_selection` disposition; native option emission skips them.
Selected SIMetrix level-1 diodes are emitted as
`.model <name> simetrix_diode_level1 (LEVEL=1 ...)`. This is a typed prepared-kernel family, not an alias for
ordinary ngspice `D`. The consumer must bind the
[measured level-1 kernel](crates/model-kernel-diode/) through the
prepared-model ABI. Model fields retain their lexical expressions and
temperature policies until instance-scoped evaluation. Literal model
parameters are checked against the kernel schema during preparation;
unknown physical fields and unsupported levels are rejected. Explicit
level-3 selection still emits the native level-3 model, and native-deck
preparation does not substitute the SIMetrix kernel for ordinary `D`.

Selection identifies the intended physical implementation; it does not
certify numerical equivalence of every parameter or geometry variant.
Unsupported target implementations, versions, and access letters are
reported instead of falling back to a different model. In particular,
HSPICE resistor model cards and unsupported diode geometry parameters
are not qualified for native lowering. `HSPICECOMPATIBILITY` is rejected
when enabled because its additional temperature and statistical options
must also be implemented; `HSPICEMODELS` selects models alone.

See the [SIMetrix device configuration reference](https://help.simetrix.co.uk/9.3/simetrix/simulator_reference/topics/simulatordevices_customisingdeviceconfiguration.htm).

`Options::power_target` (CLI `--power-target`) selects the consumer's
power-expression contract. `native-ngspice` is the default and keeps the
existing portable guarded power expressions. For `simetrix-deck`,
`prepared-solver-v1` preserves the explicit absolute-base, caret, real-part
and ordered absolute-product operations instead. A prepared consumer must
implement the four named operations and resolve function bodies in their
actual scalar or behavioural phase. This capability is recorded in the
bundle, not inferred from simulator names or a compatibility mode. Source
catalog libraries remain target-neutral until linked; other source formats
reject the prepared power target.

Native linking preserves physical source lines, comments, continuations,
scope boundaries and the root title. Static `.include`, `.inc` and named
`.lib path section` resources are resolved relative to their containing
source, followed by explicitly supplied search paths. Included standalone
`.end` directives become comments; a complete deck retains its own
terminator. Control-language commands are preserved, not executed or
interpreted as static source includes. Dynamic resources in control
commands still require their original runtime context.

The CLI publishes the complete artifact directory only after successful
preparation, using a temporary sibling directory and a rename. The parent
directory must already exist; the output directory must not exist. A
versioned `manifest.json` records format, primary filename, artifact byte
counts, dependencies and provenance. Manifest version 13 records the explicit `power_target` capability and
includes typed PWL resistor declarations as well as
`required_code_models` for each artifact and the deduplicated union for
the whole bundle. The Rust API exposes the same requirements through
`Artifact::required_code_models` and `PreparedBundle::required_code_models()`.
The `logic` requirement describes the generated event-timed logic model
schemas; a C ngspice 47 reference must load the matching
`ngspice_preprocess_logic.cm` before parsing the deck. A native solver may
implement these schemas directly. Requirements do not infer or load
arbitrary user-supplied native code models. See
[runtime/logic-xspice](runtime/logic-xspice/README.md) for building and
testing the reference module. On Linux publication uses a
no-replacement rename. Native line maps are exact at the admitted-source
boundary, including synthetic elements created during compatibility
normalization. Generated dialect artifacts retain source identities and
dependencies, but no invented one-to-one output line mapping. Native
library callers receive exact source path values; the JSON manifest uses
their display spellings. Directly linked artifacts also retain an admitted
source graph: each mapped output line identifies its source occurrence and
electrical lexical scope, including generated lines. The graph records the
parent include, selected library section, effective dialect and declaration
line. Specialized HSPICE output currently retains physical spans without a
linked graph; full cross-dialect occurrence-model binding is tracked in #70.

See [original analysis intent](#original-analysis-intent) for the binding contract.

The combined artifact/intermediate-link byte limit defaults to 128 MiB and
is configurable with `--max-output-bytes`. Per-source and command limits are
available with `--max-input-bytes`, `--max-command-output-bytes`,
`--max-diagnostic-bytes`, and `--timeout-ms`. These do not replace the cumulative request limits in `Options::work`
(`WorkLimits`), shared by the admission sessions, source linker, control-flow
expander, specialization, scalar evaluation, and translation. Configure the
following independently using the corresponding CLI switches:

| Work limit | Default | CLI option |
| --- | ---: | --- |
| Original source bytes | 256 MiB | `--max-work-input-bytes` |
| Admitted snapshot bytes | 256 MiB | `--max-work-admitted-bytes` |
| Source identities | 16,384 | `--max-work-sources` |
| Source visits (including cached includes) | 65,536 | `--max-work-source-visits` |
| Scanned bytes | 1 GiB | `--max-work-scanned-bytes` |
| Statement visits | 8,000,000 | `--max-work-statements` |
| Expression work | 32,000,000 | `--max-work-evaluation-steps` |
| Generated definitions, nodes and vector elements | 2,000,000 | `--max-work-generated-nodes` |
| Intermediate and final emitted bytes | 512 MiB | `--max-work-emitted-bytes` |
| Recursive preprocessing depth | 64 | `--max-work-depth` |

These are cumulative work allowances, not exact heap-memory or wall-clock
measurements: processing the same bytes again can consume additional work.
Canonical repeated source admissions reuse their snapshots, but every expansion
visit consumes work. Nested loops share the request allowance without changing
the source-language iteration limit. Depth has an implementation ceiling of 64
to bound native stack use. Recursive includes and unchanged-actuals recursive
specializations are diagnosed separately from work exhaustion. A budget failure
is sticky and rejects the entire request, including when an optional analysis
would otherwise skip an unsupported expression.


The SIMetrix `PWLS PUL` independent voltage/current waveform is lowered to
native repeating PWL with the original source name and an explicit DC value.
The period is specified by exactly one of `FREQ` or `PERIOD`; omitted rise is
period/1000, omitted fall equals rise, and omitted plateau width is
(period - rise - fall)/2. Omitted levels are 1 and -1. Delay is signed;
`OFF_UNTIL_DELAY` defaults to enabled, while disabling it makes the waveform
periodic before the specified delay. Zero plateau width and zero idle time
are supported without artificial edge delays. Parameter expressions stay
bound to their source instance; malformed timing is rejected. Finite
`CYCLES`, other PWLS waveform kinds and unrecognized fields (including
`DAMP_COEF`, which did not change the PUL waveform in the reference probes)
are rejected rather than silently approximated.

## Optional external source command

Configure one `AdmissionConfig` and share a `SourceSession` across the whole
source graph. A `CommandConfig` contains an absolute executable pathname,
literal arguments, and an optional absolute working directory. No shell,
argument interpolation or executable PATH search is performed by source
admission. An adapter receives:

- The configured argument vector, followed by the canonical original source
  pathname as one additional argument. A virtual in-memory source must also
  supply an explicit origin pathname, with an existing parent directory.
- The exact original source bytes on standard input, followed by EOF.
- The original source's containing directory as its working directory unless
  the caller explicitly overrides it. The environment is inherited; the
  protocol does not require any environment variables.

On exit zero, standard output is the full replacement byte stream (including
an intentionally empty result). Standard error is diagnostic only. A nonzero
exit, launch/I/O error, timeout, exceeded byte limit or cancellation rejects
the source; no partial output enters the parser. The adapter is trusted code
with the caller's normal permissions, not a sandbox. It must complete its
work before returning and must not leave persistent descendants.

Default admission ceilings are 64 MiB of original input per source, 128 MiB
of stdout and 64 KiB of stderr per invocation, and a 30-second process
deadline. The child receives an anonymous input snapshot (memory-backed on
Linux, temporary file elsewhere), and both output streams are drained
concurrently. Adapters may consume all, some or none of stdin. The deadline
covers staging the input and waiting for the process to exit after its pipes
close.
Cancellation uses a shared `Cancellation` token. On Unix, failed invocations
terminate the child process group; on Windows, the direct child is terminated.
Cleanup is separately bounded to five seconds. Ordinary filesystem reads
are synchronous and may block at the OS/filesystem level. Cumulative graph
budgets also bound admission and expansion work across all invocations.

`SourceLinker::link_with_session` applies the same session to the root,
relative includes and named library sections. SIMPLIS callers use
`parse_library_file`, `SubcircuitCatalog::add_library_file`, and
`preprocess_netlist_file` or `preprocess_netlist_linked_file`. Byte-oriented
parsers accept already-admitted data. Previously compiled library artifacts
are not raw sources and do not pass through the command.

Native, LTspice, PSpice, QSPICE and TINA preparation share one admitted
source linker. The chosen root format supplies the inherited language. A
recognized `*#NATIVE` (or `*#NGSPICE`), `*#LTSPICE`, `*#PSPICE`,
`*#QSPICE` or `*#TINA` declaration can override it at a resource's legal
header position (second physical line of a complete deck; first or second
line of an included resource after an initial comment or blank line).
Unrelated later comments do not change the language. The parent context
is restored after each include. Lowering uses the original statement's
language while sharing lexical declarations across the linked graph. For
example, an LTspice library does not make an unrelated native inductor
acquire LTspice's default series resistance. Mixed physical-model and
hierarchy policies for HSPICE/SIMetrix remain separate work ([#70](https://github.com/codeandsolder/ngspice-rs-preprocess/issues/70)); the native pipeline rejects these declarations rather than selecting a global mode.

Identity is the canonical source pathname, not a hash of its bytes: equal
contents in different directories can have different relative dependencies.
Repeated references, including symlink aliases and different sections of one
file, reuse the first snapshot. Failed admissions are cached as failures, too;
a fresh session is required to retry. The source origin path is preserved,
and line numbers refer to the admitted text (a generic adapter cannot
reconstruct a physical line mapping if it changes the number of lines).

The optional ngspice reference validator uses the same bounded process-capture
implementation, with 4 MiB limits for each output stream and the configured
timeout per attempt. It no longer waits on a child before draining its pipes.

See per-crate documentation and upstream notices for model-specific
provenance, supported formats and reproducible reference validation.

See [third-party notices](THIRD_PARTY_NOTICES.md) for upstream licensing and
reference-fixture provenance.
## Validation

Use the repository's dated nightly from `rust-toolchain.toml`, the managed
sccache compiler wrapper and an isolated Cargo target directory. CI checks the
locked workspace through the shared strict gate and builds/tests an archive
of tracked source files, without Git metadata or other checkout-only inputs.

The full source check is `bash .github/scripts/verify-source.sh` after
configuring the wrapper and target directory. It also installs the pinned
ngspice 47 reference in temporary storage, checks XSPICE execution and runs
the opt-in VDMOS differential tests. On Linux hosts unable to use the pinned
Arch binary, it builds the checksum-verified upstream source release instead.
This check does not publish packages or release artifacts.

### Private differentiator conditioning

SIMetrix deck preparation can rescale a private current coordinate after the
complete electrical artifact graph has been linked and translated. An eligible
loop contains a linear capacitor and ideal voltage-defined branches, including
a driven voltage. Its current can be measured directly or through ideal
zero-volt sensing branches. All but at most one loop node must be
electrically private. An exposed port, global net, extra current path or
unresolved current observation disqualifies the transformation.

For a positive constant factor s, replacing C with s*C multiplies the
circulating capacitor and voltage-source currents by s, without changing any
loop voltage or net external injection. Every reference to a scaled current is
replaced with its inverse-scaled value. Thus external terminal current, charge
law and derivatives are unchanged; the internal residual and Jacobian undergo
a constant diagonal coordinate transformation. The scale is chosen from the
nominal current-feedback response using the existing scoped scalar evaluator;
its value is not a model fit. An unresolved or unsuitable scale leaves the
source unchanged.

The prepared artifact and JSON manifest report the affected lexical scope,
capacitor, voltage branches and factor in `current_scalings`. A consumer that
explicitly requests a scaled internal branch current from the prepared graph
must divide that value by the recorded factor to recover the original physical
current. The optimization does not mutate the admitted source.

The coordinate proof assumes the emitted electrical topology is the topology
that is simulated. When a downstream analysis adds global shunt conductances
or capacitances, or requires unchanged internal current observations, select
`Options::conditioning = ConditioningPolicy::Disabled` (CLI:
`--conditioning disabled`). This leaves the original coordinates intact.

### Original analysis intent

A translated dialect deck is an electrical artifact, not an implicitly
executable replica of the original netlist. Manifest v14 and
`PreparedBundle::analysis_plan` carry an ordered analysis plan alongside
the circuit, with explicit source dialect, title, lexical scope and
declaration locations. The canonical artifact types and bounded scalar/axis
binding live in the independent `model-analysis-plan` crate; the solver does
not need to link the source translators. Complete SIMetrix decks reserve their first
physical line as the title; include files and raw SIMPLIS streams do not.

HSPICE preparation extracts OP, AC, DC and transient requests after
lexical occurrence binding. Multi-segment transient timing is a symbolic
`ScheduledTransient` declaration: each output interval has an absolute end,
START gates output, UIC selects startup, and explicit DELMAX is a separate
integration ceiling with its source location. The four-positional-operand
legacy form is distinguished from two segments at candidate binding, not
guessed while parsing. Conflicting positional and option DELMAX values are
rejected: the published command and option references disagree on precedence.
Omitted DELMAX retains the automatic HSPICE policy rather than an invented
native limit. RUNLVL=1..6 (default 3) retains its soft duration/20 hint and
ignores RMIN/RMAX. RUNLVL=0 binds hard segment-wise RMIN/RMAX limits
(default multipliers 1e-9 and 5). INTERP=0 (default) saves internal accepted
points; INTERP=1 requests linear interpolation at the output intervals, not
forced solver landings. The typed policies do not claim a native RUNLVL
implementation. Source references: HSPICE User Guide H-2013.03 p. 546,
Reference Manual J-2014.09 p. 524 (INTERP), and E-2010.12 .TRAN/.OPTION DELMAX. Independent SWEEP axes on AC and transient requests stay
separate from DC continuation coordinates. Analyses are removed from the
electrical artifact, so a consumer must execute the typed plan.

Native-equivalent SIMetrix analyses are represented as requests with
canonical arguments and a separate UIC flag. A static SIMetrix transient
request materializes omitted or zero output and maximum steps using
the SIMetrix default `(stop-start)/50`, rather than ngspice defaults.
Symbolic transient timing requires explicit binding. Supported physical
options are emitted in source order; initialization options are owned
by the typed passive initialization policy. Unsupported options and
operations, including periodic SIMPLIS analyses, remain visible in the
plan. SIMPLIS transient and snapshot/output sampling semantics are
not substituted with ordinary SPICE transient arguments.

Directly linked LTspice and PSpice analysis declarations are captured in the typed
analysis plan before electrical normalization. Each declaration retains
its admitted source occurrence and lexical scope, including schematic
directive origins. Independent parameter, source, temperature and model
parameter steps are distinct from DC continuation axes. Transient timing
is evaluated for the candidate, with output start and maximum integration
step kept separate; source startup and steady-state policies are not UIC.
Measurement operations preserve source waveform expressions, event
qualifiers and previous-result dependencies in declaration order. The
consumer must bind this analysis plan; an electrical artifact alone is
not a complete analysis request. Native-only decks retain native analysis
cards. Unqualified conditional and mixed-dialect analysis is diagnosed
rather than executed with guessed semantics.

PSpice STEP uses positive linear increment magnitudes even for descending
endpoints, and explicit sweep lists preserve monotonic source order. A
nested DC request retains its continuation axes independently of STEP
and multi-temperature repetition. Conflicting sweep declarations are
rejected rather than multiplied indiscriminately. PSpice transient
timing operands (including scheduled ceilings) retain their original
dialect for consumer binding. SKIPBP is represented by the typed bias-skip
flag, with the original statement retained for provenance. Statistical Monte Carlo, distribution and
worst-case directives require a separate realization policy; they are
not treated as ordinary numerical sweeps.

SIMetrix compatibility STEP is a single independent axis around one
eligible analysis. Explicit general SWEEP clauses belong to their
individual request, and distinguish an arithmetic increment (STP) from
a total point count (LIN), logarithmic density and explicit lists.
NUMCORES is scheduling metadata rather than a numerical setting.
The consumer selects an analysis index and calls
`AnalysisPlan::bind_independent_sweeps` with its lexical scalar resolver
and shared work budget. The returned axes preserve source targets and
declaration identities; candidate enumeration uses bounded mixed-radix
indices, not materialized circuit copies. DC continuation axes are
separate. Circuit/model invalidation and occurrence binding belong to
the consuming solver, not the source grid generator.

Named sweep stimuli, noise and transfer-function probes, output selectors
and measurements retain source coordinates for binding against device
identities and prepared current scalings. Control scripts
are opaque unsupported declarations, never executed by preparation.
Root statements have admitted physical source spans, including
continuations and selected include sections; generated declarations use
pre-lowering artifact locations, not fabricated original source lines.
Include-only plans cannot be executed as complete decks.

Consumers should call `validate_unbound_execution()` before running a
plan without dialect-specific bindings. Consumers implementing additional
bindings must inspect every declaration and option disposition and resolve
source coordinates, initialization and sampling semantics before execution.
`native_requests()` is a filtered view, not a validation or execution API.
Native-only ngspice input retains its own executable directives.

### Analysis-scoped passive initialization

The SIMetrix deck policy is carried separately from ordinary electrical
artifacts in `PreparedBundle::initial_conditions` and manifest v7.
It retains source-specific `INITCONDMODE`, `ICRES`, per-device `IC`,
`ICMODE`, and optional `BRANCH`, plus ordered node `.IC` hints. The
source-only device and node IC fields are removed from the emitted
electrical cards: consumers must not apply native ngspice IC rules as
well as this policy.

Resolve the deck defaults and each declaration's expressions for the
actual elaborated occurrence and enclosing analysis, including
the predefined `ANALYSIS` value. A declaration may be instantiated
repeatedly with different local parameter bindings. `BiasConstraint`
describes exact hard or finite soft forcing only for the bias solve;
the solver owns the row allocation, convergence retries, and ordinary
storage initialization. During UIC only storage IC and node hints
participate. In AC, ordinary small-signal storage stamps replace the
bias constraints. Conflicting ideal constraints must remain singular.

This policy selects the measured SIMetrix Elements GUI defaults
(mode 2, ICRES=2 mOhm) when no explicit option overrides them.
Do not simulate the electrical text alone if this policy contains
initialization declarations. Native decks have no source-specific
initialization policy and retain their normal IC semantics.

### SIMPLIS piecewise-linear resistor startup

The `PreparedBundle::pwl_resistors` list and manifest v9 contain each
translated resistor's signed voltage/current knots, independent axis,
one-based optional `initial_segment`, emitted artifact and lexical scope,
and original linked device and terminal names. A linked definition may be
instantiated repeatedly; bind its declaration independently for each
elaborated occurrence. Hierarchical snapshots specialize definitions
before translation and the effective segment must be valid.

The `binding` describes the exact native behavioral source and, for a
current-defined law, its series zero-volt sensor and intermediate node.
`series_sensor` describes additional instrumentation inserted to observe
the current in another device's control law. Preserve these observation
branches when replacing the behavioral stamp. The emitted electrical
text represents the same physical law regardless of the startup hint.

During an initial nonlinear bias attempt, an explicit hint selects
the first linearized segment only. Recompute the segment from the
actual solved independent coordinate, including end extrapolation;
interior negative slopes are valid. Do not enforce terminal voltage or
current as an initial constraint, and do not reset the hint during an
ordinary Newton iteration or continuation step. A consumer that ignores
this metadata cannot guarantee the source's multiroot startup choice.

Generated branch and internal node names are allocated against the entire
lexical scope, including subcircuit ports and current-control targets.
Distinct linked devices therefore cannot alias through punctuation
normalization. Artifact validation checks scoped electrical card identity,
orientation and sensor/law type under the shared preparation work budget.

References: [SIMPLIS device statements](https://simplis.com/documentation/simplis/simplis_reference/topics/devicestatements_simplisdevicetypes.htm),
[resistor model laws](https://www.simplis.com/documentation/simplis/simplis_reference/topics/modelstatements_devicemodelsusedinsimplis.htm),
and [initial-condition overrides](https://simplis.com/documentation/simetrix/simplis_reference/topics/controlstatements_controlstatementsforsettinginitialconditions.htm).


### SIMPLIS reactive startup and snapshots

For linked SIMPLIS decks, `.INIT` assignments are resolved per instance
before translation. An ancestor snapshot wins over a local snapshot or
device IC; repeated instances can therefore have different initial states.
`V(C...)` / `V(!C...)` are voltages and `I(L...)` / `I(!L...)` are
signed currents, not charge or flux. The source target kind is checked
before canonicalizing the instance path.

A PWLC IC is converted to the normalized charge coordinate on its Q(V)
curve; a PWLL IC is converted to the normalized flux-linkage coordinate
on its flux/current curve. The first and last segments extrapolate
outside the tabulated range. SIMPLIS reactive curves must have strictly
increasing independent and storage coordinates. The normalized state
preserves charge/flux conservation and is invariant to an arbitrary
constant storage offset. The unified bundle stores these initial states
in the existing typed initialization manifest, bound to the actual
emitted storage device and definition. Linked SIMPLIS defaults to hard
bias initialization (or storage initialization when UIC is requested).
The low-level native text translator emits ordinary IC slots for a
consumer explicitly running the native transient with UIC.

Reference: SIMPLIS simulator manual, sections "Device Models Used in
Simplis", "SIMPLIS Device Types", and "Control Statements for Setting
Initial Conditions".


### Prepared independent-source waveforms

Manifest v6 carries `PreparedBundle::waveforms`, keyed by emitted artifact,
lexical subcircuit scope and independent source identity. Bind each declaration
in its actual occurrence's parameter environment using `PreparedWaveform::resolve`.
The result includes the time-zero DC value, the time-domain forcing, and the
periodic-operating-point idle flag. Native positional sources are unchanged.

Named PUL (rectangular or trapezoidal), PWLS PUL, SAW, SIN and PWL share this
contract. Periodic linear forcing stores one sorted cycle of unique knots with
separate left and right values, an origin, a positive period and an optional
pre-delay hold. Ungated cycles extend both ways; the origin is the nearest
signed cycle boundary to zero, avoiding loss of a small positive delay to
period subtraction. The first knot is at zero, the endpoint is implicit and
interpolation approaches its left value. Coincident edges collapse to their
exterior limits, not artificial finite-duration ramps. DC startup uses the
right-sided value except while the source is held. PWL may have signed times
and duplicate times; SIN keeps signed delay and damping. Symbolic timing is
validated when bound.

The typed payload is authoritative, including when a native-looking source
card has only its DC placeholder. Consumers must bind the payload before
analysis; do not silently simulate a placeholder as a constant source.
The standalone expanded translator can emit native reference cards for
constant parameters, including ngspice's duplicate-knot PWL syntax; this
is a reference representation, not the prepared solver contract. The
native solver binding and exact event scheduling are tracked in
ngspice-rs-fast issue 223.


### Legacy Berkeley BSIM preparation

[model-compact-intrinsic](crates/model-compact-intrinsic/) provides exact
legacy BSIM1/2/3 source catalogs and independent BSIM1/2 preparation and
constitutive equations. [model-kernel-bsim](crates/model-kernel-bsim/)
assembles the BSIM1/2 generalized physical residual, charge and noise
artifacts. Solver registration and additional BSIM3 revision physics are
tracked separately; the existing native BSIM3 implementation remains
the owner of its validated modes. See [the ownership audit](docs/legacy-bsim.md).
