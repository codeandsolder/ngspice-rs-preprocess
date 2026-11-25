# Experimental ALB / XSPICE runtime

Source-only ngspice 47 code models for the SIMetrix ALB translation effort.
The explicit experimental Rust compiler emits native XSPICE subcircuits,
but production translation still rejects `D_LOGIC_BLOCK` where exact
SIMetrix semantics have not been established.

## Models

- alb_probe: one-bit timing feasibility probe, t=0 wake, inertial pulse rejection,
  runtime capacitance × output-resistance delay.
- lut_bit: per-bit LUT code model with 1–16 digital inputs, output enable encoded
  by a Z table entry, runtime load-sensitive inertial delay and explicit IC.
  Table string is one symbol per binary input row, with input[0] as bit 0.
  Valid symbols are 0, 1 and Z/z (ngspice may lowercase parameter strings).
- bundle_driver: load-sensitive digital source on a three-net bundle with
  independent sink/source overcurrent limits.
- bundle_load: publishes sink-current, source-current, and capacitance metadata
  via digital LOAD on three separately connected nodes.
- family_dac: nonlinear bidirectional digital-to-analog current stage with
  separate strong/resistive/HI-Z curves and interrupted-slew event handling.
- family_adc: physical bidirectional analog input that reflects aggregate
  digital sink/source current and capacitance, applies smooth low/high
  clamps, and emits a timed digital output triple with optional Schmitt state.

The three-node bundle is (logic state, sink-current metadata, source-current
metadata). Driver metadata outputs use HI_IMPEDANCE drive strength to permit
multiple consumers. The experimental compiler now supports three-node physical ALB pin bundles
through hierarchy; the production translator still rejects the backend.

Known-input LUT rows are evaluated with exact binary indexing. The bare
native LUT supports **two distinct and explicitly chosen semantics**:

- Generic/standalone native code models (default `unknown_as_zero=false`)
  retain the **Boolean-consensus envelope**: an UNKNOWN input permits both
  binary rows; agreement preserves known drive, disagreement becomes
  UNKNOWN/UNDETERMINED. Z remains a distinct drive result: unanimous Z
  means HI_IMPEDANCE and mixed driven/Z rows are undetermined. This option
  is intentionally not presented as SIMetrix expression semantics.
- Source-compiled SIMetrix ALBs explicitly emit `unknown_as_zero=true`.
  The real Elements **9.40d** oracle establishes that ALB expressions
  convert a LOW or UNKNOWN *level* to binary 0 and HIGH to binary 1,
  **independent of STRONG/RESISTIVE/HI_IMPEDANCE/UNDETERMINED strength**.
  All 12 source-state combinations were tested on `out[0] = in[0]`;
  additional `1 ^ UNKNOWN`, `!UNKNOWN`, ternary and output-enable probes
  corroborate this rule. The C model performs direct table lookup with
  only the ONE-level bits set. A disabled output remains a genuine
  high-impedance drive, not a forced LOW; separately measured high-Z
  outputs float toward an external pull-up. The generic mode remains
  available for standalone XSPICE experiments.

Both evaluation modes work for the full 1–16 source-bit range without
constructing a separate ternary truth table. Invalid table dimensions or
symbols still produce a diagnostic. The new
`smokes/lut-vendor-unknown.cir` tests the vendor behavior against the
independent generic consensus mode in the same ngspice run. This is
**measured expression-input coercion**, not comprehensive ALB parity:
sequential EDGE/LEVEL, asynchronous assignments, timing and other
operators still require separate proprietary evidence. Production remains
fail-closed until their applicable semantics have been established.

## Reproduce

Requires a configured ngspice 47 source/build tree, with a built cmpp, matching
the installed ngspice's XSPICE+CIDER+OpenMP, non-ngshared configuration. An
ABI mismatch can cause a simulator crash. No ngspice, vendor library source,
generated C, or compiled code-model binary is committed.

The build uses the existing /usr/local/libexec/sccache-client wrapper by default
(or explicitly supplied SCCACHE_WRAPPER), with no new sccache daemon and no
cache relocation. It creates a temporary ICM module below the configured
ngspice build tree, compiles using its normal cmpp/GNUmakefile, copies only the
result to an explicit output directory, then removes the temporary module.

    runtime/xspice/build.sh \
      /path/to/build/alb-xspice/ngspice-47 \
      /path/to/build/alb-xspice/runtime-output

    runtime/xspice/smoke.sh \
      /path/to/build/alb-xspice/runtime-output/ngspice_preprocess_alb.cm \
      /path/to/build/alb-xspice/runtime-output

The smoke runner loads the freshly built model through a temporary .spiceinit,
runs transient circuits, numerically checks their output envelopes and removes
all temporary simulation files. It relies on the existing ngspice binary and
does not compile or invoke Rust. Do not run this with a different configured
ngspice build and assume ABI compatibility.

## Evidence and remaining gates

15 passing ngspice smoke circuits currently establish t=0 startup for the
single-bit probe, short-pulse swallowing, propagation, capacitive-load timing,
under-/over-current signaling, known-input AND and XOR LUT truth tables,
two-stage propagation, non-symmetric input row indexing, and tri-state return
to a driven state.

The new `lut-consensus.cir` transient also tests eight native digital
outputs together: `0 AND X = 0`, `1 OR X = 1`, XOR of a known bit
with X remaining X, unanimous 3-input zero/one tables staying
known under all-UNKNOWN inputs, a 3-input LUT with two uncertain
bits bracketing one known bit, unanimous Z losing to a separate
STRONG driver, and mixed driven/Z rows resolving undetermined.
A delayed 0→X and X→1 transition confirms that consensus output
changes still use the event and inertial scheduling path.

The `lut-inertial-contention.cir` regression checks the full 5 ps-sampled
trace for four simultaneous outputs: unloaded inertial contention, temporary
Z release under a second strong driver, the same 8 ns pulse suppressed by
4 pF fanout plus 1 kΩ output resistance (5 ns + 4 ns inertial delay), and
a transport-delay counterpart that preserves short pulses. An overlapping
2 ns return before a pending transition tests cancellation without stale
glitches. The checker verifies the complete sequence and every transition
time, not isolated favorable samples. Loads are fixed for each model during
the simulation; time-varying fanout and proprietary scheduler equivalence
are not yet established.

This is runtime feasibility, not full SIMetrix compatibility. Next work:
finish exactness and production integration of the experimental Rust backend;
verify time-varying load changes and differential SIMetrix scheduler behavior; implement actual
SIMetrix-family analog bridge models; differentially establish UNKNOWN-expression
evaluation. EDGE/LEVEL models
remain unsupported. Preserve production fail-closed behavior until those
requirements have passed.

## Experimental Rust-compiled timed subcircuits

A separate explicit compiler entry point converts an eligible timed acyclic,
BITWISE COMB ALB body into a runnable XSPICE subcircuit. It is not a
production ngspice translation mode. The compiler keeps
stage topological order, per-stage/output-bit LUT cells, physical pin indices,
initial COMB state, and distinct propagation delays. Inputs precede outputs
on the generated subcircuit; every physical pin expands to three consecutive
state/sink-current-metadata/source-current-metadata nodes in ALB pin order. Constant-fanin
stages are given an internal stock d_pulldown and a duplicated binary LUT
row rather than being dropped or connected to an unknown external signal.

The CLI takes all resolved timing/electrical parameters explicitly via a
JSON settings file and emits only a native subcircuit; it does not load code
models into the simulator or claim family-bridge or UNKNOWN-state fidelity.
No approximate fallback occurs if the graph or native per-bit fanin is not
supported.

    CARGO_BUILD_EPHEMERAL_INTERMEDIATES=false \
      RUSTC_WRAPPER=/usr/local/libexec/sccache-client \
      CARGO_TARGET_DIR=/path/to/build/preprocess/target \
      cargo build --locked -p model-translate-simplis --bin alb-xspice-compile

    /path/to/build/preprocess/target/debug/alb-xspice-compile \
      runtime/xspice/fixtures/timed-enable.alb \
      runtime/xspice/fixtures/timed-enable.json \
      /path/to/build/alb-xspice/timed-enable.inc ALB_TIMED

The other fixtures exercise constants, hierarchy, fanout and multi-driver
conditions. Generate four includes (`timed-enable`, `constant`,
`bundle-buffer`, `bundle-buffer-low`), using the ALB bodies and JSON files
under `runtime/xspice/fixtures`. The latter two use `bundle-buffer.alb`,
and both set the emitted subcircuit name to `ALB_BUFFER`. Build the matching
native code model before running:

    runtime/xspice/smoke-generated.sh \
      /path/to/ngspice_preprocess_alb.cm \
      /path/to/generated-include-directory \
      /path/to/scratch-root

This runner verifies nine compiler-generated transient circuits, including
stage delays, constants, hierarchy, fanout overload, multiple drivers,
a timed ALB output driving the nonlinear HC-family DAC, and a complete
native family ADC → timed ALB → nonlinear DAC path. Production translation
remains fail-closed.

## Experimental three-node load bundles across hierarchy

The native model ABI and emitted subcircuit pin list now expand **each physical
ALB input and output** to three consecutive digital nodes:
`state sink_meta source_meta`. The event-driven LUT model drives the state;
the two metadata outputs remain HI-Z. `TOTAL_LOAD(state)` measures physical
capacitance for the model's `delay + out_res * capacitance` propagation.
`TOTAL_LOAD(sink_meta)` and `TOTAL_LOAD(source_meta)` independently aggregate
the connected input-current demands, forcing UNKNOWN for values outside the
configured sink/source bounds.

Every external ALB input has exactly **one** passive `sr_bundle_load` instance,
whether or not any expression reads that pin. Internal Boolean stage inputs
are assigned zero physical load. Reusing one input in several LUTs therefore
does not multiply `INPUT_LOAD` or current; connecting several ALB instances
to a real net does sum their physical loads. All three nodes pass through
nested subcircuits, so callers must use the expanded interface consistently.
This intentionally breaks the old, state-only experimental ABI.

Native ngspice 47 tests demonstrate one stage/output signal rising near
**4.003 ns** unloaded and **8.003 ns** with two downstream 2 pF ALB inputs
through another subcircuit (1 kΩ output resistance yields 4 ns extra).
Three 10 mA input loads breach a 21 mA source-current limit and yield
UNKNOWN; a separate test covers sink-current overload. Two strong native
ALB output drivers on the same three-node net resolve UNKNOWN when their
states conflict, then recover HIGH when they agree. The native
runtime-only smokes and all seven generated-netlist smokes also cover
startup, inertial timing, binary LUT rows, output enable/HI-Z and constants.

**Exactness scope:** these results validate the experimental native backend,
not equivalence to the SIMetrix digital expression engine or analog family
law. The current electrical configuration is uniform across a model's
input pins, shares one pair of current bounds, and lacks the recovered
family-specific analog bridges and asymmetric output resistance.
Outstanding work includes accurate per-model/per-pin parameters,
family interfaces, rollback under competing event histories and full
differential UNKNOWN semantics. The normal D_LOGIC_BLOCK lowering remains
disabled until those gates are established.


## Experimental nonlinear family digital-to-analog stage

The `sr_family_dac` XSPICE code model is not an ideal output-voltage source.
It has an analog inout port `%gd(node 0)` that injects its current and analytic
conductance (`PARTIAL(out,out)`). The three digital input nodes correspond to
`state sink_meta source_meta`. Physical input capacitance, sink current and
source current each contribute precisely one `LOAD` on their corresponding
digital node. The model builds with the existing configured ngspice 47 code
model library and is **not** part of the normal `D_LOGIC_BLOCK` translation.

The static strong-low curve uses `g_pulldown`, `out_low`, `knee_low`, and
`v_smooth`. Define `d=v(out)-out_low`, `k=knee_low-out_low`,
`s=v_smooth` and `g=g_pulldown`:

- Below `d=k-s`, current into the device is `g*d`.
- Above `d=k+s`, current is the plateau `g*k`.
- Within the smoothing band for `s>0`, current is
  `g*(d-(d-(k-s))²/(4*s))`, giving continuous current and derivative.

The strong-high curve mirrors this with `d=out_high-v(out)`,
`k=out_high-knee_high`, and opposite current polarity. The RESISTIVE
strength is a plain `g_resistive` path to the selected rail; HI-Z is a
`g_hiz` path to the midpoint between low and high rails. UNKNOWN is an
equal-weight blend of the two strong drive curves. AC Jacobians are
provided as well as DC and transient current equations.

For time-dependent switching, the model maintains weights over five
physical current/voltage basis curves (strong low/high, resistive low/high,
and HI-Z). The event scheduler snapshots the **actual current weighted
curve** when a new event interrupts an incomplete transition, then
interpolates toward the new strength/state target. Nominal rail swing
determines the scaling of the configured `t_rise`/`t_fall` duration.
This goes beyond stock ngspice `dac_bridge`, but does not claim full
SIMetrix time-domain equivalence.

Published electrical semantics: [SIMetrix Mixed Signal Reference](https://simplis.com/documentation/simetrix/simulator_reference/topics/digitalmixedsignaldevicereference_mixedsignal.htm).
The measured HC reference uses 34 mS/24 mS low/high conductance,
2.0/2.75 V output knees and 0.5 V quadratic smoothing. Expected
low-current at 2 V is 63.75 mA and saturates at 68 mA; high-current
plateau is 54 mA. LS reference tests use 50 mS/5.5 mS conductance
and unsmoothed 0.65 V/0 V knees.

Validation (ngspice 47, existing sccache wrapper):

    runtime/xspice/smoke-family-dac.sh       /path/to/ngspice_preprocess_alb.cm /path/to/scratch-root

Thirteen native netlists test HC/LS low and high I-V sweeps,
AC small-signal derivatives (linear, quadratic knee, saturated),
resistive and HI-Z strengths, loaded analog slew, interrupted analog
transitions, and upstream digital current overload. The eighth
compiler-generated smoke, `timed-enable-family`, binds a compiled ALB
output bundle to the nonlinear HC stage and verifies both STRONG-HIGH
and HI-Z electrically at an analog output with a weak midpoint pull.

Remaining exactness gates: implicit bridge insertion/selection,
`in_family`/`interface_model` and power-supply scaling, SIMetrix ADC
input clamps and reflected analog loads, complete UNKNOWN expression
semantics and cross-simulator differential transient comparisons.
This is an independently useful experimental model, **not a claim of
SIMetrix family equivalence or a production translator mode**.


## Experimental nonlinear analog-to-digital family bridge

`sr_family_adc` has one `%gd(analog_node 0)` port and outputs three
digital nodes: `state sink_meta source_meta`. It is not an ideal comparator.
Physical downstream ALB input devices contribute `LOAD` values on the
three output nodes; `TOTAL_LOAD(state)` represents their total capacitance,
and `TOTAL_LOAD(sink_meta/source_meta)` their signed low/high input currents.

To reflect the static analog input load, the model constructs one
conductance and one bias current whose load points are
`(out_low, sink_current)` and `(out_high, source_current)`. Specifically,
`G=(source_current-sink_current)/(out_high-out_low)` and
`I(v)=sink_current+(v-out_low)*G`. This is compatible with
the documented parallel current-source/resistor topology, but the
specific SIMetrix interpolation coefficients are not yet established.
Capacitance is reflected as a backward-Euler current companion
`I_C=C*(v(t)-v(previous))/dt`, with derivative `C/dt`. AC contributes
`j*omega*C`. This is a stable first-order numerical implementation,
**not** a claim that SIMetrix uses the same integration method.

The configurable low and high clamp terminals use smooth
diode-to-linear-resistor current functions, and pass their analytic
derivative to ngspice. The low clamp starts below `clamp_low`; the upper
starts above `clamp_high`. The exponential branch joins the
`1/clamp_res` slope continuously at `clamp_bias` volts of excess,
and has zero current and derivative at its onset. Omitted clamp
thresholds default to the configured `out_low/out_high`, not fixed
0 V/5 V, so the LS model's `clamp_high=15` can explicitly suppress
the upper clamp until its high-voltage breakdown point. The precise
SIMetrix diode interpolation is **not** documented; this is a physical
approximation with explicit tests, not an asserted exact replica.

A non-Schmitt ADC emits ZERO below `in_low`, ONE above `in_high`,
and UNKNOWN in between, with independent `rise_delay/fall_delay`.
The `schmitt=true` variant retains its previous bit inside the band
and uses `ic` when starting there. No implicit family negotiation,
digital family compatibility rules, or full input-threshold
`time_tol`/adaptive timestep reconstruction are implemented.

The new repeatable native suite:

    runtime/xspice/smoke-family-adc.sh \
      /path/to/ngspice_preprocess_alb.cm /path/to/scratch-root

runs HC DC input-load and diode-resistance sweeps, AC complex
admittance, RC transient caused by physical digital fanout, slow
threshold/UNKNOWN/delay trajectories, LS upper-clamp behavior and
Schmitt hysteresis. For the explicit example, a combined 2 nF load at
1 kHz produces `Y=6mS+j12.566uS`; 2 pF through 1 kΩ yields ~2 ns
time constant. The compiler-generated `timed-enable-both-family`
fixture additionally drives an ALB subcircuit with three independent
family ADCs, then terminates its digital output through the nonlinear
family DAC. Production netlist conversion stays fail-closed until the
family wiring, precise SIMetrix clamps, threshold time-step control
and differential UNKNOWN semantics have stronger evidence.


## Threshold crossing time tolerance in experimental family ADC

SIMetrix `adc_bridge` and `adc_schmitt` use `time_tol` (default 100 ps)
to bound **threshold crossing time error**, not as a propagation delay.
With large analog timesteps, a bridge can otherwise sample an input
long after it passed a digital threshold.

The experimental `sr_family_adc` uses ngspice 47's
`cm_analog_set_temp_bkpt()` to request rollback to an interpolated
threshold crossing. The previous accepted voltage **and timestamp**
are tracked together in ngspice's analog-state vectors, so speculative
nonlinear iterations and multiple digital EVENT callbacks at the same
simulation time do not corrupt the reference sample. After rollback,
a small time-tolerance-aligned projection of the previous accepted
slope prevents floating-point near-equality from undoing an accepted
UNKNOWN→LOW transition in a subsequent EVENT callback.

Normal ADC mode checks both thresholds in chronological order,
so a narrow UNKNOWN interval does not get skipped. Schmitt mode
only tests the active hysteresis boundary and retains the known
state in the threshold band. If `time_tol` is much larger than
the current timestep, the comparator does not extrapolate early
into the next state; the coarse timestep simply remains unrefined.

The regression circuits deliberately allow 200 ns analog timesteps
with a 0→5 V rise over 1 µs and a symmetric fall later. The 2.1/2.2 V
thresholds are crossed at 420/440 ns rising and 2580/2560 ns falling.
`time_tol=10us` can directly jump from LOW to HIGH much later than
the physical crossing; `time_tol=10ps` aligns native output edges with
both analog crossing boundaries, including the Schmitt paths. The
test fixtures and numeric checks are part of
`runtime/xspice/smoke-family-adc.sh`.

This is an ngspice-specific implementation of the documented intent,
not proof of SIMetrix's exact nonlinear/noisy-signal crossing algorithm.
Native minimum-breakpoint spacing and source interpolation quality can
still limit accuracy. Automatic bridge placement, cross-family
compatibility, VSUPPLY scaling and UNKNOWN expression semantics are
separate production exactness gates.

## Explicit mixed-signal family planning (no production auto-selection)

The experimental Rust CLI validates family resolution *before* attempting
native netlist emission. It does **not** infer a device family from a
transistor topology or substitute stock ngspice ADC/DAC models.

The source-only matrix resolver targets SIMetrix **Elements 9.2**:
the caller supplies the documented COMPAT.TXT structure (header,
N×N input/output compatibility, N×N input/input priorities and
N×N output/output priorities). No proprietary or guessed built-in
compatibility table is bundled. Valid digital compatibility values are
OK, WARN and ERR; priority values are ROW, COL, OK (equivalent) and ERR.
Contradictory opposite-facing priorities, unknown families, cycles
and incompatible connections fail closed. A WARN is reported explicitly.
The reserved UNIV family is handled by documented universal rules,
not redefined by external compatibility files.

The full family/subfamily name is retained for **physical** bridge
selection, independently of the resolved digital compatibility base.
The bridge planner groups input pins by the tuple
(analog net, full IN_FAMILY, exact VSUPPLY) and gives each output pin
its **own** DAC, even when the family and supply match. The caller must
provide actual bridge models by exact family and direction, plus their
nominal supply voltage. A subfamily such as HC_SCHMITT cannot silently
fall back to HC_ADC. Explicit supply and nominal supply are retained;
the CLI **does not yet scale analog thresholds or drive curves**,
because the family-specific SIMetrix scaling equation is not verified.

From the repository root, using the synthetic, deliberately non-vendor
fixtures:

    cargo run --locked -p model-translate-simplis \
      --bin alb-family-resolve -- \
      runtime/xspice/fixtures/family/compat-synthetic.txt \
      runtime/xspice/fixtures/family/plan-synthetic.json \
      /path/to/plan.json

The output contains digital net compatibility decisions/warnings and
resolved physical ADC/DAC groups. Inputs with different supply voltages
or Schmitt suffixes remain separate groups. The CLI rejects duplicate
pin IDs, missing exact bridge models and invalid supplies before
writing the output. Fixture matrices are **synthetic test data** and
are not authoritative SIMetrix HC/LS/ECL compatibility rules.

**Version boundary:** newer SIMetrix documentation (9.3+) describes a
different physical-threshold compatibility mechanism rather than the
Elements 9.2 three-table algorithm. This experimental planner implements
only the explicit 9.2 table algorithm, with bridge grouping defined as
independent physical topology. Supply-specific electrical materialization
is handled by the separate experimental step below, while whole-deck
automatic rewriting remains disabled. The production `D_LOGIC_BLOCK`
translator remains fail-closed.

## Source-only native family bridge materialization

The standalone `alb-family-resolve` compiler (legacy 9.2 compatibility
rules) yields deterministic digital compatibility decisions and
*physical bridge groups*. The separate `alb-family-materialize` pass
can turn those groups into native ngspice 47 bridge instances. This
still requires explicitly supplied pin-to-digital-bundle wiring,
source-model parameters, and nominal supplies. It is not production
`D_LOGIC_BLOCK` translation or automatic whole-deck topology discovery.

Inputs to the materializer:

1. `RESOLVED.json` from `alb-family-resolve`, identifying each physical
   ADC group or independent DAC driver, its full family name, and supply.
2. `NATIVE.json` with a strict `models` catalog (complete
   `sr_family_adc`/`sr_family_dac` parameters) and `pins` dictionary.
   Each pin has an exact digital `state`/`sink_meta`/`source_meta`
   triple. A shared ADC group must use the same triple for all its
   connected physical input pins. Each DAC driver has its own triple.

The materializer validates every group against the exact model family,
bridge kind and nominal supply, rejects missing/unused/overlapping pin
wiring, and writes one compiled native `.model` + `A` device per
resolved group. It does **not** add `sr_bundle_load`: the Rust ALB
subcircuit compiler already adds one physical load per *actual* input,
and shared ADC groups must see their combined `TOTAL_LOAD` metadata.

Only the documented electrical quantities are supply-scaled:

- ADC input thresholds `in_low/in_high` are multiplied by
  `VSUPPLY / VSUPPLY_NOMINAL`.
- DAC analog output levels `out_low/out_high` use the same factor.
- If a DAC `g_pullup` or `g_pulldown` is explicitly set, that value
  wins. Otherwise, the matched physical output device's `OUT_RES`
  provides the corresponding conductance `1 / OUT_RES`.
- Unspecified scaling of clamp rails, knee voltages, smoothing width,
  capacitance and current parameters is **not invented**.

Example using solely synthetic, non-vendor fixtures:

    cargo run --locked -p model-translate-simplis \
      --bin alb-family-resolve -- \
      runtime/xspice/fixtures/family/compat-synthetic.txt \
      runtime/xspice/fixtures/family/buffer-plan-3v3.json \
      /path/to/resolved.json

    cargo run --locked -p model-translate-simplis \
      --bin alb-family-materialize -- \
      /path/to/resolved.json \
      runtime/xspice/fixtures/family/buffer-native.json \
      /path/to/bridge.inc

Include the resulting `bridge.inc` alongside a Rust-generated ALB
`.subckt`. The `runtime/xspice/smoke-family-materialized.sh` native
suite does exactly this with the existing `bundle-buffer.alb` physical
input/output port triple. A 1.5 V analog signal is HIGH at the
3.3 V-scaled HC thresholds (1.386/1.452 V), but LOW at the nominal
5 V thresholds (2.1/2.2 V); its analog DAC output should accordingly
be near 3.3 V or 0 V. Input load is not duplicated and the DAC
pull-up/down conductances are derived from an explicit 40 Ω output
resistance (25 mS). A third native circuit connects two separately
compiled ALB input pins to **one shared ADC** and verifies that their
2 pF input capacitances add to a 4 pF analog load (1 kΩ × 4 pF
≈ 4 ns), without an extra bridge capacitor.

Native clamp-law differential comparison, automatic net discovery and
ALB graph wiring, original 9.2 table recovery, 9.3+ electrical
compatibility resolution, precise supply treatment of undocumented
nonlinear parameters, and exact UNKNOWN/stateful behavior remain
separate production gates.

## Automatic hierarchical pin discovery and bridge insertion (experimental)

The third standalone pass, `alb-family-autowire`, removes the need for
a handwritten physical pin-to-(state/sink/source)-node JSON mapping.
It reads **actual SPICE X instances and nested .subckt port mappings**
and follows each ALB instance occurrence down the hierarchy, preserving
the physical-port identity of every compiled ALB input/output.

The source-only compiler's declarations have a canonical sequence of
`sr_inN/sr_inN_sink/sr_inN_source`, then
`sr_outN/sr_outN_sink/sr_outN_source`. The walker uses this
*structural interface* to detect compiled ALB instances, rather than
guessing from subcircuit or device names. It reads each compiled output
bit model's actual `out_res` value to supply the physical DAC's
documented `OUT_RES` fallback. Zero output resistance is representable;
a missing DAC conductance at zero `OUT_RES` cannot be modeled as a
finite native conductance and is rejected, while explicit conductance
parameters may override that fallback.

The required annotations describe only each **intentional**
analog/digital boundary as an analog root net, digital *state* net,
pin direction, full family and supply voltage. The triple metadata
nodes, physical pin IDs, hierarchy paths, family grouping and bridge
instance source are discovered/generated automatically. A shared
digital state across two physical inputs maps both ALB input loads to
one ADC, provided their source triples are actually identical.
Independent outputs are not merged.

Run from the source repository (use the standard Rust 1.99 and
configured sccache wrapper for actual builds):

    cargo run --locked -p model-translate-simplis \
      --bin alb-xspice-compile -- \
      runtime/xspice/fixtures/bundle-buffer.alb \
      runtime/xspice/fixtures/family/buffer-40r.json \
      /path/to/generated.inc ALB_BUFFER

    cargo run --locked -p model-translate-simplis \
      --bin alb-family-autowire -- \
      runtime/xspice/fixtures/family/auto-nested.cir \
      /path/to/generated.inc \
      runtime/xspice/fixtures/family/auto-bindings-3v3.json \
      runtime/xspice/fixtures/family/auto-models.json \
      /path/to/autowired.cir

Keep `generated.inc` alongside `autowired.cir` for their relative
`.include` relationship. The output includes the original source
deck and automatically inserted native family ADC/DAC models and
instances before the final `.end`. Source electrical parameters and
the explicit family catalog remain in the version-controlled inputs;
the materializer does not invent model curves.

The executable acceptance test `runtime/xspice/smoke-family-autowire.sh`
uses ngspice 47 and Rust-compiled ALB code models to verify four
circuits: direct 3.3 V HIGH, direct 5 V LOW, nested wrapper 3.3 V
HIGH, and a single ADC driving two separately compiled physical
2 pF ALB inputs, whose 4 pF total capacitance produces the expected
4 ns RC loading through a 1 kΩ resistor. Existing mixed-signal and
native-runtime regression tests must remain green.

**Scope and next gates:** The input circuit must have its relevant
`.subckt` declarations available in the deck and supplied compiled
model file(s). Unresolved opaque X instances, illegal recursion and
ambiguous references fail; the discovery pass does not guess what
external includes contain. Mixed boundaries on genuinely private
subcircuit-local nodes cannot be wired at the root without
scope-local bridge specialization, so they are rejected rather than
treated as global nodes. The source ALB X instances already have
their compiler-defined *physical triple* port layout; rewriting an
arbitrary original logical-only SIMetrix circuit into this layout
is still a separate pass. Version-specific family compatibility,
implicit boundary inference from proprietary models, physical
clamp/knee equivalence, and UNKNOWN/stateful ALB interpreter exactness
remain unresolved; production D_LOGIC_BLOCK conversion is unchanged.

## Original logical SIMetrix ALB port expansion (experimental)

The explicit `alb-logical-expand` compiler accepts original source
containing `A1 [logical-inputs] [logical-outputs] DL` and
`.model DL d_logic_block` with inline `DEF` or embedded `.file`
language bodies. It reuses the existing translator's ALB parser and
model-parameter evaluator rather than reimplementing that language.
Combinational known-state truth tables compile to native output-major
timed LUTs; supported acyclic timed COMB models use the existing native
bit-LUT compiler. Unrepresentable asymmetric output resistance,
dynamic feedback and unsupported interpreter semantics fail closed.

The transformation collects digital node roles from *all* A instances
and propagates them through nested/sibling `.subckt` interfaces until
stable, regardless of definition order. Every connected digital node
gets **one** shared state/sink/source triple *per circuit scope*,
including formal ports and parent X-actual connections. Two logical
receivers sharing an input retain their combined per-physical-pin
load metadata; unrelated pre-existing nodes are never reused as
metadata. The original A devices become physical `XSR_<instance>`
instances of source-compiled `SRALB<N>` subcircuits. The original
D_LOGIC_BLOCK cards and consumed ALB language blocks are absent from
the generated native SPICE deck; production translation is unchanged.

Source digital/analog mixed boundary **annotations** remain explicit:
each entry maps the original analog net to its distinct intended
digital state net and provides family/supply information to the
already-merged autowiring pass. Only verified digital A/X sites are
renamed; analog voltage sources, resistors and other analog devices
retain their original analog node identities. Internal mixed nodes
that cannot be separated at root and unknown devices connected to
the digital net fail closed rather than becoming fictitious bridges.

For a full original-source pipeline using the synthetic fixtures:

    cargo run --locked -p model-translate-simplis \
      --bin alb-logical-expand -- \
      runtime/xspice/fixtures/family/logical-original.cir \
      runtime/xspice/fixtures/family/auto-bindings-3v3.json \
      /path/to/lowered.cir /path/to/generated.inc

    cargo run --locked -p model-translate-simplis \
      --bin alb-family-autowire -- \
      /path/to/lowered.cir /path/to/generated.inc \
      runtime/xspice/fixtures/family/auto-bindings-3v3.json \
      runtime/xspice/fixtures/family/auto-models.json \
      /path/to/circuit.cir

Then load the configured ngspice 47 source-only native code-model
library and simulate `circuit.cir` with `generated.inc` beside it.
`runtime/xspice/smoke-logical-alb.sh` runs the complete chain for
four native transient circuits: original embedded-file HC 3.3V HIGH
and 5V LOW, nested inline DEF HIGH, and two original ALB receivers
sharing one ADC with measured 2pF + 2pF = 4pF analog RC loading.

This is a deliberate **experimental source lowering**. The real
SIMetrix family compatibility table, exact UNKNOWN/stateful ALB
interpreter, model-specific analog nonlinear bridge curves, arbitrary
opaque external digital primitives, and genuinely private mixed-net
scope-local bridge placement are separate gates. Normal production
D_LOGIC_BLOCK translation still refuses these models.

## Definition-local mixed-signal bridges (experimental)

A digital ALB pin may connect to analog circuitry **inside** a subcircuit
definition, with neither of its mixed nets exported as a formal port.
Placing the bridge in the root circuit would refer to unrelated node
names or create a duplicate boundary. The explicit source-only pipeline
now accepts a lexical subcircuit `scope` array in each boundary entry:

    {
      "connections": [
        {
          "scope": ["TOP"],
          "analog_net": "local_in",
          "digital_net": "d_in",
          "direction": "input",
          "in_family": "HC",
          "supply_v": 3.3
        },
        {
          "scope": ["TOP"],
          "analog_net": "local_out",
          "digital_net": "d_out",
          "direction": "output",
          "out_family": "HC",
          "supply_v": 3.3
        }
      ],
      "bridge_models": [
        {"name": "HC_ADC", "kind": "adc", "family": "HC", "nominal_supply_v": 5},
        {"name": "HC_DAC", "kind": "dac", "family": "HC", "nominal_supply_v": 5}
      ]
    }

The `scope` path is a lexical **definition** path, such as
`["OUTER", "INNER"]` for a nested .subckt declaration. It is not
a path to one runtime occurrence. An omitted/empty scope retains the
original root behavior.

Run `alb-logical-expand` followed by `alb-family-autowire` with the
**same** boundary JSON. The first pass preserves original local analog
device nodes and splits only verified digital ALB/X connections into
the annotated digital state node, carrying sink/source metadata per
logical net within that scope. The second pass discovers the actual
compiled ALB input/output physical triples **relative to the owning
subcircuit** and emits the required nonlinear family ADC/DAC models
and instances immediately before the matching `.ends`. A definition
instantiated twice gets an independently evaluated electrical bridge
per occurrence, while a group of physical receivers on a shared local
digital net gets a single ADC with their combined physical input loads.

The source-only regression `runtime/xspice/smoke-private-family.sh`
proves four real ngspice 47 cases: two copies of an identical definition
with separate 3.3 V native bridges, 5 V nominal thresholds rejecting
the same 1.5 V input, a nested private definition, and two separate
2 pF ALB receivers whose shared private ADC exhibits a 4 pF input
load through a 1 kΩ source resistor. The emitted bridge adds no
`sr_bundle_load` duplicate.

Only genuine private nodes are eligible. Annotating analog or digital
formal ports is rejected, as are missing lexical definitions, duplicate
or incompatible boundary assignments and previously existing unverified
A devices on the same selected digital state. Different per-instance
family/supply parameters for occurrences of one definition require
proper **model specialization**, not mutation of a shared definition.
Mixing analog and digital through exported formals also needs a distinct
interface transformation. These are remaining explicit gates; normal
production D_LOGIC_BLOCK lowering is still fail-closed.

## Exported analog ports with internal digital ALB boundaries (experimental)

Original SPICE subcircuit ports may be **analog-facing** even when the
enclosed ALB uses the same node spelling as a digital pin. The explicit
boundary entry with lexical `scope` can now name that exported analog
formal while assigning a **different internal** `digital_net`. For
example:

    {
      "scope": ["TOP"],
      "analog_net": "analog_in",
      "digital_net": "d_in",
      "direction": "input",
      "in_family": "HC",
      "supply_v": 3.3
    }

The source graph marks the analog formal as a precise **interface cut**
*before* propagating digital roles through X-instance actual/formal
mappings. This preserves the original single-node analog formal in
`.subckt TOP analog_in analog_out` and in the root `X... TOP`
instance. Within TOP, the compiled ALB gets its native three-node
digital state and source/sink metadata; the native ADC/DAC is placed
inside TOP, immediately before its `.ends`. The original analog node
and its electrical source/load remain connected to the analog side.

Only annotated analog formals are cut. A separate truly digital
formal in that same definition still expands to a three-node
state/sink/source bundle through every X-instance call. This enables
a producer with an analog input and a digital output to feed multiple
consumers with digital inputs and analog outputs. The shared digital
bus retains one physical triple and preserves fanout/loads, while
the ADC lives in the producer definition and the DAC in the consumer
definition. Each instantiated consumer gets its own analog output.

The five actual ngspice 47 source-only regression circuits in
`runtime/xspice/smoke-exported-family.sh` validate: two independent
instances of an analog-port ALB under 3.3V and nominal 5V supplies,
nested wrappers whose analog formals remain intact, one ADC driving
two physical input receivers for a total 4pF loading, and a mixed
producer/bus/two-consumer topology crossing both analog and digital
subcircuit interfaces.

**Validation and remaining exactness:** An annotated exported *analog*
formal cannot reuse an exported digital formal as its internal state,
or use the same node name on both sides. Missing cuts still fail
when an analog source would be treated as a digital pin. A truly
bidirectional mixed electrical pin with independently specified
digital input/output state, per-instance supply/family variations
on one shared definition, unknown external digital devices, and
unproven SIMetrix compatibility/interpreter behavior remain separate
gates. Normal production D_LOGIC_BLOCK translation is unchanged.

## Hierarchical X occurrence-specific native family bridges (experimental)

One original SPICE subcircuit can be instantiated at multiple electrical
supplies. Add `occurrence`, an array of X instance names starting at the root, to
each family boundary in the same JSON used by the experimental
`alb-logical-expand` and `alb-family-autowire` commands.
For example, a boundary in scope `["TOP"]` can use occurrence
`["XONE"]` at HC 3.3V while occurrence `["XTWO"]` uses HC 5V.
Supply-specific output/DAC boundaries are declared in the same way.

The logical source compiler compiles the ALB body once and deduplicates
only the repeated structural cuts bearing occurrence annotations.
The native family stage resolves every reachable hierarchical X call, checks that
every caller of a specialized definition has complete annotations,
then creates variant subcircuits from sorted electrical signatures.
Signatures contain analog/digital pin identity, direction, full logic
family assignment and bit-exact VSUPPLY. Two identical signatures
reuse one deterministic variant; different supply voltages produce
distinct variants and native ADC/DAC models. Both share the same
compiled ALB truth-table logic, with independent physical state.

The synthetic source fixture `occurrence-3calls.cir` and associated
`occurrence-3calls.json` specify Xone at 3.3V, Xtwo at 5V, and Xthree
at 3.3V, all with identical 1.5V analog inputs. The native ngspice 47
acceptance script `smoke-family-occurrence.sh` verifies HIGH/LOW/HIGH
simultaneously. Structural Rust tests verify same-signature reuse and
source-order independence, plus rejected missing/duplicate X
assignments, unknown paths, invalid supplies and model collisions.

The current implementation builds a complete **acyclic reachable X
occurrence tree**, resolves each occurrence by lexical subcircuit scope,
and computes variants bottom-up. A parent definition is specialized when
its own electrical parameters change **or any descendant X instance
requires a different variant**. Variant signatures include child X
instance names, original lexical target identities and recursively
nested electrical signatures, so definitions are reused only when
all their relevant electrical behavior is equivalent. The generated
model declarations stay inside the **corresponding cloned lexical
parent**, preserving local `.param` and `.model` resolution rather
than flattening private definitions to root. A definition with two
inner calls at different supplies gets two inner variants even when
only one parent instance exists.

The additional native regression `smoke-family-recursive.sh` runs
three original-source fixtures: root-defined OUTER→INNER, lexically
nested OUTER/INNER with local `.param RMON`, and one OUTER whose two
inner ALBs operate at 3.3V and 5V simultaneously. All use the
same source-only native code-model library. Invalid occurrence paths,
missing/duplicate electrical assignments, conflicting port roles,
recursive subcircuit calls and generated-name collisions are rejected
instead of emitting a partial circuit. Unannotated external X devices
are still validated by the downstream physical topology resolver.

Normal production D_LOGIC_BLOCK translation remains fail-closed;
original simulator family compatibility/version, bidirectional pin
behavior and UNKNOWN/stateful interpreter semantics are separate gates.

## Experimental bidirectional analog-facing ALB pins

An original `D_LOGIC_BLOCK` may sense **and** drive one physical pin.
Its original `A` device can list that same node in both its input and
output port vectors. Treating this as one native digital triple would
short the input and output *digitally*, bypassing the ADC/DAC and
incorrectly suppressing real analog contention.

The experimental `alb-logical-expand` JSON boundary now requires
`direction: "input" | "output"`; this is the same field already used
by `alb-family-autowire`. To expose an analog-facing inout pin, specify
**two distinct physical roles**, with the same `analog_net`, but
different `digital_net` values, e.g.:

    {"scope":["TOP"],"analog_net":"pin","digital_net":"D_SENSE",
     "direction":"input","in_family":"HC","supply_v":3.3}
    {"scope":["TOP"],"analog_net":"pin","digital_net":"D_DRIVE",
     "direction":"output","out_family":"HC","supply_v":3.3}

The source-only logical lowering distinguishes **ALB input versus
output positions**, while preserving a single analog formal pin in
`.subckt TOP pin ...`. The compiled ALB gets independent state,
sink-current metadata and source-current metadata triples for the
ADC input and DAC output. Both bridges attach to the *same physical
analog net*, so analog drive strength, voltage/current, input loading,
and external loading/contending sources are handled by the actual
ngspice electrical solver. No synthetic digital feedback wire is
created, and the ALB's original input load is emitted once.

Missing either direction for an analog inout boundary, aliasing the
two digital states or the analog node, and ambiguous X interfaces
remain fail-closed. The source graph now propagates actual ALB input/output
roles upward through any number of X wrappers without inheriting direction
from unrelated callers. An input-only child formal routes to the ADC triple;
an output-only child formal routes to the DAC triple. A child formal
with both roles still cannot fit one triple into two independent physical
interfaces, and multiple ALB output drivers on one annotated analog net
need separate DACs rather than silently sharing a digital bus; both cases
are explicitly rejected. Internal metadata identifiers are allocated
collision-free even if user-selected states resemble generated names.

`runtime/xspice/smoke-family-bidirectional.sh` runs the original
source `fixtures/family/bidirectional-port.cir`: the analog PIN is
sensed through an ADC and driven through a tri-state native DAC,
while the output monitor follows the sensed digital level. With
the output disabled the external 1kΩ source controls the pin at
LOW and HIGH. With the external source LOW and output enabled,
a native 40Ω high driver raises the pin to approximately **3.17V**;
on release the pin and monitor return LOW. Real ADC thresholds,
2pF input capacitance and the native delay/current curves remain
active throughout. This proves the **explicit direct-bidirectional
ALB pin topology** in ngspice 47, not proprietary SIMetrix
bidirectional/UNKNOWN interpreter equivalence. Production
`D_LOGIC_BLOCK` remains fail-closed.

### Nested directional ALB boundary regression (Linux/ngspice)

`runtime/xspice/smoke-family-nested-bidirectional.sh` uses one original
`.subckt TOP PIN OE MONITOR` whose physical PIN passes through two nested
wrapper levels. One child is an input-only ALB sensor, the other an
output-only ALB driver. The parent retains its original single-node
analog formal, but its X calls are rewritten to distinct three-node
`D_SENSE` and `D_DRIVE` bundles. The two native family bridges
attach to the original analog PIN, with no direct digital feedback.
Two ALB input pins each contribute exactly one physical load.
ngspice 47 checks external 1 kΩ drive opposed by a strong native LOW
output, then a strong HIGH native output opposed by external LOW, and
the actual returned sensor voltage. This is a Linux-validated structural
and electrical gate, not a proprietary SIMetrix waveform oracle.


### Independent ALB output drivers on one physical analog net

The family planner and native emitter already require a separate DAC for each
physical ALB output; the source-level boundary planner previously rejected
multiple output annotations sharing the same analog node before native wiring
could run. The experimental source-only `alb-logical-expand` now accepts
**multiple independently identified ALB output states** on a common analog
net. The distinct original ALB outputs must already be separate digital
states in the source; neither the source pass nor ngspice may short those
states together. Source annotations supply each independent driver's physical
net, family and supply, e.g.:

    {"scope":["TOP"],"analog_net":"pin","digital_net":"D_LEFT",
     "direction":"output","out_family":"HC","supply_v":3.3}
    {"scope":["TOP"],"analog_net":"pin","digital_net":"D_RIGHT",
     "direction":"output","out_family":"HC","supply_v":3.3}

Each selected state must correspond to a real ALB output pin in the scope's
resolved X hierarchy; duplicate or directionally invalid declarations are
rejected before output. The existing family planner constructs **one native
DAC bridge per physical ALB output occurrence**, even with identical net,
family and supply. The input-side ADC grouping behavior is unchanged.

`runtime/xspice/smoke-family-multi-driver.sh` exercises
`fixtures/family/multi-physical-drivers.cir`: two ALB drivers inside
reused two-level wrapper definitions are attached to one physical analog PIN
while receiving independently timed analog control inputs. The native
simulation checks LOW/LOW, HIGH/LOW contention, HIGH/HIGH, LOW/HIGH contention,
and LOW/LOW again. The contention plateaus are about 0.5498 V under the supplied
nonlinear DAC curves (HIGH current-limited at 0.55 V from its rail,
LOW with 40 Ω conductance and a 100 kΩ load). They must be solved electrically
from the actual DAC current models rather than from a digital logic resolver.

Multiple original ALB producers with the same logical net spelling need
explicit source-occurrence selectors, described next. Production
`D_LOGIC_BLOCK` conversion remains disabled; Linux ngspice electrical
results do not establish proprietary SIMetrix interpreter equivalence.

### Same original logical net — occurrence-specific ALB output split

When multiple original ALB A devices use the *same* logical output node
name, one digital state triple would combine their outputs before the
physical family bridges. The experimental original-source lowering now
supports an **exact source producer selector** on each output boundary:

    {"scope":["TOP"], "analog_net":"pin", "digital_net":"D_LEFT",
     "direction":"output", "producer":{"path":["Xleft","Xinner","A1"],
                                        "output_pin":0},
     "out_family":"HC", "supply_v":3.3}
    {"scope":["TOP"], "analog_net":"pin", "digital_net":"D_RIGHT",
     "direction":"output", "producer":{"path":["Xright","Xinner","A1"],
                                        "output_pin":0},
     "out_family":"HC", "supply_v":3.3}

The `producer.path` is relative to the annotated lexical `scope`:
zero or more X instance names ending with the original ALB A instance;
`output_pin` is the zero-based bit position in that A device's output
vector. Matching is case-insensitive. *All* physical ALB output
occurrences originally driving this node must have exactly one
annotation. The compiler discovers the complete instance paths through
reused lexical definitions; it does not infer a first/last winning
driver, silently drop outputs, or resolve contention digitally.

For each selected output, the compiler creates a collision-checked
private digital state and sink/source metadata triple. For distinct
X calls, only that call's output interface is rewritten: the child
subcircuit definition remains shared, so the two actual native DACs
are electrically separate but use the same compiled ALB library.
Direct A instances are also individually selected, including their
specific output bit index. An original net that is also read by ALB
inputs still requires a separate ADC input boundary, so feedback
returns through the physical analog solver.

If multiple selected producers are behind the *same* X-instance output
formal, the compiler now promotes a separate triple for each one across
all necessary child interfaces, as described below. Missing, duplicate,
unmatched, wrong-direction or incomplete producer selectors still fail
before writing generated source. Existing separate-logical-state output
boundaries remain supported without a selector.

Reproduce the new Linux electrical check with
`runtime/xspice/smoke-family-multi-driver.sh CODEMODEL_CM BIN_DIR SCRATCH_ROOT producer-identity-drivers`.
It shares the complete waveform and nonlinear KCL checks with the
existing `multi-physical-drivers` test. The fixture contains two
reused two-level wrapper X instances with the *same original* output
spelling `PIN`, then proves two actual native family DACs and analog
contention at ~0.54978 V. No SIMetrix interpreter equivalence or
production `D_LOGIC_BLOCK` enablement is claimed.


### Recursive promotion of shared original X output formals

A single original X formal can export more than one original ALB output.
The source-only compiler now recursively extends the physical interface
of such a child with **one extra state/sink/source triple per actual
output producer**. These promoted triples are appended after the
original formals, before any original subcircuit parameter suffix, and
every X caller is rewritten to supply the same number and order of
physical triples. An original formal is retained for preexisting
sensing or ordinary digital connections; it is never treated as the
single DAC representing all original output producers.

The extensions are per **lexical definition**, not a flattened or cloned
copy of every instance. A selected caller connects its extra formals to
independent physical DAC bundles. An unselected caller connects all the
new output formals back to its original shared digital bundle, so a
reused subcircuit still behaves as its original digital circuit when
not explicitly attached to an analog output boundary. Stable ordering
uses the full relative X/A occurrence paths and ALB output-bit indices.
Generated internal state names are collision-checked against the
original source and other planned states.

The checked example is
`fixtures/family/promoted-x-formal-drivers.cir`, with boundaries in
`promoted-x-formal-drivers.json`. Its source hierarchy runs
`TOP/Xgateway/GATE/Xgroup/GROUP/{Xleft,Xright}/OUTER/INNER/A1`:
both physical ALB producers originally spell the *same* GROUP output
formal `PIN`. `GATE` and `GROUP` each acquire additional output
formals; the existing reusable `OUTER` and `INNER` definitions remain
unchanged. A second `TOP/Xidle` call reuses `GROUP` without analog
output annotations and retains the original shared digital state.

A Rust regression also checks three independent physical outputs behind
one X formal, and a bidirectional version with a separate native ADC
bundle for the sensing side. Negative tests reject incomplete or
mismatched source producer identities. Run the native electrical test:

```sh
runtime/xspice/smoke-family-multi-driver.sh CODEMODEL_CM RUST_BIN_DIR SCRATCH_ROOT promoted-x-formal-drivers
```

Like the previous two multi-driver fixtures, it must prove two independent
DACs driving one analog PIN, with an opposing-drive plateau near
`0.54978 V` determined by the nonlinear native family current curves.
This is source-only ngspice 47 functionality, not a SIMetrix behavioral
oracle; production proprietary `D_LOGIC_BLOCK` lowering stays disabled.

### Dynamically loaded physical bus with ALB readback (native 3-state ADC proof)

`fixtures/family/dynamic-sensed-contention.cir` places two independently
promoted ALB outputs and an **actual third ALB sensing the original PIN**
behind the same nested `TOP/GATE/GROUP` interface. Their exact identities
and separate analog-to-digital input, output and readback boundary bundles
are in `dynamic-sensed-contention.json`. The readback ALB's digital output
is routed to its own DAC on analog `OBSERVE`; it is not an idealized
sampling function. An unselected caller of the reused `GROUP` definition
continues to use the original digital bus. The analog circuit includes
an 80 pF capacitor, a 100 kΩ bias load and a voltage-controlled 52 Ω
shunt (0.05 Ω switch on-resistance), with two independent logic-control
transitions and the load being switched while the DACs are both HIGH
and again while they oppose each other.

The native regression varies **only the physical shunt resistance** across
35 Ω, 52 Ω and 80 Ω, keeping the exact same compiled ALB, native ADC and
DAC models. The nominal 5 V HC ADC thresholds, after scaling to 3.3 V,
are `1.386 V` (LOW) and `1.452 V` (HIGH). When both DACs drive HIGH,
each is current-limited to `(3.3-2.75) * 0.025 = 13.75 mA` below its
knee. Therefore the expected settled bus voltage is approximately
`(2 * 13.75 mA) / (1/(Rshunt+0.05 Ω) + 1/100 kΩ)`, respectively
**0.96354 V**, **1.43063 V**, and **2.19961 V**. The analog ADC must
therefore drive the *physical ADC* into **LOW**, **UNKNOWN**, and
**HIGH**, respectively. The actual source-compiled ALB (following the
12-state Elements 9.40d expression oracle) coerces UNKNOWN to 0, so
its separate analog monitor settles at **LOW**, **LOW**, and **HIGH**.
The earlier generic Boolean-consensus experiment instead produced an
UNDETERMINED digital monitor and the native DAC's nonlinear blend
at approximately 0.54956 V for the 52 ohm case; that was an honest
native-model result but is **not vendor ALB expression behavior**.
Physical ADC state and ALB expression value must not be conflated.

During LOW/HIGH electrical contention, the same switched load also
changes the analog equilibrium to
`13.75 mA / (0.025 S + 1/(Rshunt+0.05 Ω) + 1/100 kΩ)`;
those additional predictions are checked independently. The 80 pF
capacitor makes the physical pin respond continuously rather than
jump to the new DC equilibrium. Releasing the load while both outputs
remain HIGH must restore the bus and readback to HIGH; releasing it
during opposed drive restores the nonlinear contention equilibrium.

At 52 Ω the real ngspice 47 waveform crosses the scaled ADC's upper
threshold at approximately **63.490 ns** while falling and **72.114 ns**
while rising; the observed ALB→DAC voltage crosses the monitor midpoint
at **65.041 ns** and **73.585 ns**, respectively. The regression checks
the order and bounded ~1.5 ns readback delay, not the exact absolute
timestamps. It also checks all five independent logic/load phases,
actual analog transient settling, separate physical bridges and exact
retention of unselected reusable subcircuit wiring. The shell test
runs all three physical-load variants without recompiling code models:

```sh
runtime/xspice/smoke-family-dynamic-feedback.sh CODEMODEL_CM RUST_BIN_DIR SCRATCH_ROOT
```

These are verified *ngspice physical circuit semantics* with
vendor-measured 9.40d ALB UNKNOWN-to-zero expression coercion; the
separate HC electrical curves remain experimental, not an exact
proprietary HC physical-interface claim. Production `D_LOGIC_BLOCK`
conversion remains disabled pending broader language equivalence.


## Native vendor-UNIV differential oracle (SIMetrix Elements 9.40d)

The original source-only ALB physical sharing fixture now also has an
**independent vendor-UNIV test configuration**,
`fixtures/family/dynamic-vendor-univ.json` plus an explicitly supplied
external model configuration. SIMetrix Elements 9.40d
automatically selects `UNIV_adc` and `UNIV_dac` at analog boundaries even
when the original logical ALB declarations say `family="HC"`; the separate
physical bridge annotations therefore select `UNIV`, nominal and actual
`VSUPPLY=5`, without changing the logical declarations or the existing
3.3 V HC fixtures.

The tested vendor ADC thresholds are 2.1/2.2 V, with **retained prior LOW/HIGH
state within that band**. The vendor DAC uses 5 V rails, 24/34 mS high/low
conductances, 2.75/2.0 V knees, 0.5 V smoothing, 2 ns edges, and a **signed
-31 pF digital input load** for rise/fall compensation. Those are represented
explicitly in the external test configuration; parameters not directly reported by
the vendor remain experimental assumptions. No production ALB conversion is
enabled.

The negative compensating load can exceed the intrinsic ALB output delay.
The code models now share `common/load_delay.h` to saturate the effective
propagation delay at zero rather than scheduling events in the past or
discarding the signed compensation. The compiled LUT also evaluates its
combinational truth table at DC if `IC=UNKNOWN` and its input levels are
known, instead of producing a fictitious UNKNOWN analog equilibrium at t=0.
Explicit known IC values continue to override DC combinational evaluation.

The optional differential regression requires the independently obtained
external model JSON (not distributed with this repository):

```sh
runtime/xspice/smoke-family-vendor-univ.sh \
  /path/to/ngspice_preprocess_alb.cm /path/to/debug-bin /path/to/scratch \
  /path/to/external-models.json
```

It simulates original ALB hierarchy with two independent DACs and a monitor
ADC, and checks initial DC plus seven measured vendor UNIV load/history
equilibria: 18/20/22 ohm shunts with/without a preexisting HIGH, and the
original 52 ohm switched-shunt case. In particular, a loaded 20 ohm bus at
2.16497 V must keep LOW if loaded from startup but retain HIGH if the same
shunt is switched in after the bus was HIGH. The separate 3.3 V HC
physical fixture still tests its actual ADC UNKNOWN threshold band,
but (correctly) expects the source ALB to interpret that X as LOW; bare
generic LUT consensus tests remain unchanged.

The proprietary vendor `.PRINT TRAN` traces and original `.out` listings
are external research evidence and are not distributed. The first 20 ohm
loaded-from-startup comparison gave native
`V(pin)` **5.86 mV RMS difference across 7,251 samples** and exact sampled
`V(observe)` equality.

**Important comparison correction:** In SIMetrix `.TRAN 20p 145n` sets a
20 ps `.PRINT` interval but *not* the internal maximum time step; without
an explicit fourth argument, the solver caps its steps at `145ns/50=2.9ns`.
In ngspice the original transient source already limits internal steps to
roughly 20 ps. More importantly, SIMetrix's hysteretic `SW` defaults to
`TON=TOFF=1ns` with continuous resistance transitions, whereas ngspice's
`SW` changes abruptly. These simulator differences accounted for almost
all apparent switched-load transient mismatches, **without any changes to
ALB code models**. Separate proprietary measurement copies set
`.TRAN 20p 145n 0 20p` and add `Ton=1p Toff=1p` to the *external* switch;
the original ALB definitions, physical bridge models, and native fixture
are unchanged.

Under those matched conditions, full 7,251-sample vendor-vs-native errors
for the 20/52 ohm switched cases are **3.89/3.70 mV RMS for PIN** and
**17.75/16.40 mV RMS for OBSERVE**, respectively. The new
`fixtures/family/vendor-univ-edge-reference.csv` contains independently
measured vendor 2.5 V edge times, and `verify-vendor-univ-edges.awk` checks
all four rising/falling PIN and monitor crossings for 20/52 ohm loads
within 100 ps. This tests real dynamic readback timing in addition to
previous steady-state/hysteresis cases, without shipping vendor libraries
or using undocumented simulation constants. Complete
proprietary ALB language parity is not yet claimed.
