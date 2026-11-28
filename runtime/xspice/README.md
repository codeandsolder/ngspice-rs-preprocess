# Experimental ALB / XSPICE runtime

This is an **explicit, optional experiment** in compiling SIMetrix ALB
(`D_LOGIC_BLOCK`) source into ngspice 47 XSPICE subcircuits. Production
electrical translation does not depend on this compiler, its FFI, ngspice,
or the code-model library. It rejects ALB models and never selects this
runtime automatically.

## Components

- `model-alb-experimental`: ALB syntax and semantics, truth-table and timed
  combinational plans, stateful event scheduling, physical load and drive
  modeling, lexical model binding and hierarchy-aware boundary discovery.
- `model-alb-xspice-ffi`: optional Rust static library for the stateful native
  code model. Every native instance has its own mutable event state.
- `runtime/xspice`: source-only ngspice code models, independently authored
  regression decks and numerical reference observations. The runtime
  is never built as a mandatory dependency of the production translator.

The compiler preserves the model and instance parameter environments
separately (USER vs DEVICE), shares equivalent immutable compiled kernels,
and expands physical digital pins into three consecutive nodes:
`state sink_meta source_meta`. Boundary metadata carries capacitance and
independent sink/source current demands. The native ADC/DAC bridges
retain analog interfaces, including signed supply/drive/load parameters
where explicitly supported. Generic standalone LUTs offer Boolean
consensus for unknown inputs; source-compiled ALBs use the measured
initial-zero, subsequently last-known input history. The stateful engine
supports bounded register/event semantics, including EDGE/LEVEL and
qualified clocks, with speculative events committed only on simulator
acceptance. Unsupported forms return diagnostics rather than being
silently approximated.

External native X interfaces must be explicitly declared and verified
against their code-model interface specifications, physical pin roles,
vector widths, parameter constraints and original pin ordering.
See the `fixtures/family` examples for external stock code models and
hierarchical analog/digital boundary mapping.

## Build

Use the workspace toolchain and the existing managed compiler cache.
Choose an explicit build and scratch directory; do not use a shared
source checkout as scratch space.

```sh
cargo build --locked -p model-alb-experimental --bins --target-dir BUILD_DIR
runtime/xspice/build-stateful.sh NGSPICE_47_BUILD_TREE OUTPUT_DIRECTORY
```

For a runtime without the Rust stateful model:

```sh
runtime/xspice/build.sh NGSPICE_47_BUILD_TREE OUTPUT_DIRECTORY
```

For a prebuilt Rust static library, pass its path as the third argument
to `build.sh`. Both builds produce
`OUTPUT_DIRECTORY/ngspice_preprocess_alb.cm`.

A configured ngspice 47 source/build tree with a built `cmpp` and
matching ABI is required. Source and build directories may differ.
The script stages the source and generated objects in temporary module
directories and removes them on exit. The external simulator binary
must match the code-model ABI; a different build configuration can
crash the simulator. No simulator binary or generated code-model
library is committed.

## Explicit compilation

The command-line tools belong to `model-alb-experimental`, not the
production translation package. They require explicit electrical
parameters rather than guessing a digital family.

```sh
BUILD_DIR/debug/alb-xspice-compile BODY.alb SETTINGS.json OUTPUT.inc SUBCKT_NAME
BUILD_DIR/debug/alb-boundary-discover ORIGINAL.cir ELECTRICAL.json BOUNDARIES.json
BUILD_DIR/debug/alb-family-compile ORIGINAL.cir BOUNDARIES.json NATIVE_MODELS.json OUTPUT.cir COMPILED.inc
```

For separate stages, use `alb-logical-expand`, `alb-family-autowire`,
`alb-family-resolve`, and `alb-family-materialize`. Running a tool
without arguments prints its exact invocation.

## Numerical validation

All smoke runners take explicit paths to their compiled library, the
appropriate Rust binary/include directories when needed, and a
pre-existing scratch root. They create disposable per-run directories.
They do not invoke a vendor simulator to regenerate the comparison
waveforms; the recorded observations and manifest hashes are inputs
to the checks, not outputs from the implementation under test.

```sh
cargo test --locked -p model-alb-experimental -p model-alb-xspice-ffi --all-targets
runtime/xspice/smoke.sh MODEL.cm SCRATCH_ROOT
runtime/xspice/smoke-stateful.sh MODEL.cm SCRATCH_ROOT
runtime/xspice/smoke-stateful-compiled.sh MODEL.cm BUILD_DIR/debug SCRATCH_ROOT EXTERNAL_MODEL_JSON
runtime/xspice/smoke-logical-alb-auto.sh MODEL.cm BUILD_DIR/debug SCRATCH_ROOT
```

Run the complete opt-in native regression suite against a stateful
code-model library with:

```sh
runtime/xspice/test.sh MODEL.cm BUILD_DIR/debug SCRATCH_ROOT
```

The runner reports every independent suite and returns a nonzero exit
status on any failure. Append one or more suite names (for example,
`smoke-stateful-compiled` or `smoke-generated`) to reproduce just those
cases. The default list avoids replaying the original-model tests that
are already included by the automatic physical ALB suite.

Other smoke scripts cover source-derived timing/qualifier boundaries,
UNKNOWN and release history, physical ADC/DAC domain and clamp
behavior, dynamic loading, directional drive, signed parameters,
nested and heterogeneous receiver groups, global-node and passive-path
discovery, original stock digital/real event models, and differential
comparisons against explicitly supplied external analog cells. Each
script has its own checks and thresholds. The independently recorded
reference data are retained with their original hashes and time grids;
the presence of a reference fixture is not a claim that every circuit
or every simulator version has been validated.

To run the generated LUT controls, compile the four named fixture
includes below, using `fixtures/bundle-buffer.alb` for both buffer
variants. Generate into a dedicated directory.

| Include name | ALB body | Settings | Subcircuit |
|---|---|---|---|
| timed-enable.inc | timed-enable.alb | timed-enable.json | ALB_TIMED |
| constant.inc | constant.alb | constant.json | ALB_CONSTANT |
| bundle-buffer.inc | bundle-buffer.alb | bundle-buffer.json | ALB_BUFFER |
| bundle-buffer-low.inc | bundle-buffer.alb | bundle-buffer-low.json | ALB_BUFFER |

Then run `smoke-generated.sh MODEL.cm GENERATED_INCLUDE_DIR SCRATCH_ROOT`.
For `smoke-family-materialized.sh`, compile the buffer with
`fixtures/family/buffer-40r.json` and
`fixtures/family/buffer-zero-current.json` and pass both includes.

The source-model probes are project-authored. Upstream interface
specifications retain their notices (see the root third-party notices).
Vendor simulator binaries and unlicensed model-library distributions
are not included.

## Scope

The measured semantics apply to the explicit experimental path and its
documented numerical controls. They do not establish comprehensive
SIMetrix parity or enable production translation of ALB models.

Vendor-physical smoke runners take the external UNIV model JSON as their final argument. The synthetic UNIV wiring fixture is not vendor numerical evidence.
