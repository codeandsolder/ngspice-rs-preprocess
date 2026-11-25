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
| `simplis-deck` | Select SIMPLIS projection, specialize hierarchy and lower electrical primitives | `deck.cir` plus referenced companions |
| `simetrix-deck` | Select SIMetrix projection, specialize hierarchy and lower electrical primitives | `deck.cir` plus referenced companions |
| `expanded-deck` | Lower already expanded dialect source without hierarchy specialization | `deck.cir` |
| `expanded-library` | Lower already expanded dialect source as an include | `model.lib` |
| `simplis-library` | Compile a SIMPLIS library projection | `library.json` |
| `simetrix-library` | Compile a SIMetrix library projection | `library.json` |

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
library path suffixes take priority over a unique portable basename;
ambiguous includes fail. Content-equivalent library aliases retain all
source names. A compiled artifact is never passed to the source command.
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

### Source model selection

SIMetrix model selection is request-local. The typed mapping key is the
case-insensitive model type and declared level (zero denotes omission).
The selection layers are built-in defaults, `HSPICEMODELS`, an optional
explicit device configuration (`--device-config PATH`), and deck `.MAP`
declarations, in increasing priority. Conflicting declarations within a
layer are rejected. The source-language header controls expression and
lexical semantics independently of model selection. The mapping table is
shared across the deck's admitted includes; indexed catalog libraries
cannot independently introduce global selection directives. Include such
resources explicitly instead. Parsed-library format v6 retains projected
source statements so that a catalog can be selected again under a new
request's mapping rather than baking in a previous request's defaults.

For example, `.MAP ModelName=D Level=1 Device=Diode3` selects the
level-3 diode implementation for cards declared `D LEVEL=1`. The
manifest's `model_selections` list records original projected card text,
source identity, lexical scope, dialect, declared and effective version,
and selection origin. Logical projected cards do not claim a physical
source line number. A new alias requires an explicit `LETTER`.
Selection identifies the intended physical implementation; it does not
certify numerical equivalence of every parameter or geometry variant.
Unsupported target implementations, versions, and access letters are
reported instead of falling back to a different model. In particular,
HSPICE resistor model cards and unsupported diode geometry parameters
are not qualified for native lowering. `HSPICECOMPATIBILITY` is rejected
when enabled because its additional temperature and statistical options
must also be implemented; `HSPICEMODELS` selects models alone.

See the [SIMetrix device configuration reference](https://help.simetrix.co.uk/9.3/simetrix/simulator_reference/topics/simulatordevices_customisingdeviceconfiguration.htm).

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
counts, dependencies and provenance. Manifest version 6 includes
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
their display spellings.

The combined artifact/intermediate-link byte limit defaults to 128 MiB and
is configurable with `--max-output-bytes`. Per-source and command limits are
available with `--max-input-bytes`, `--max-command-output-bytes`,
`--max-diagnostic-bytes`, and `--timeout-ms`. These do not replace the
separate cumulative evaluation and hierarchy work limits.


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
budgets are a separate preprocessing policy.

`SourceLinker::link_with_session` applies the same session to the root,
relative includes and named library sections. SIMPLIS callers use
`parse_library_file`, `SubcircuitCatalog::add_library_file`, and
`preprocess_netlist_file` or `preprocess_netlist_linked_file`. Byte-oriented
parsers accept already-admitted data. Previously compiled library artifacts
are not raw sources and do not pass through the command.

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

### Analysis-scoped passive initialization

The SIMetrix deck policy is carried separately from ordinary electrical
artifacts in `PreparedBundle::initial_conditions` and manifest v6.
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

Manifest v5 carries `PreparedBundle::waveforms`, keyed by emitted artifact,
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
