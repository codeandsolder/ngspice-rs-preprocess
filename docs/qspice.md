# QSPICE plaintext source adapter

Select `qspice-deck` or `qspice-library` explicitly. The input is a
plaintext SPICE deck or library, not a QSPICE schematic, symbol, raw result,
or executable device. A successful projection is not a claim of complete
QSPICE simulator compatibility.

## Source and binding contract

| Source area | Current projection |
| --- | --- |
| Source text | Windows-1252; explicit UTF-8 BOM; reject binary/project/compiled resources and undefined encoding bytes. |
| Includes | Static whole-file or named-section references, original-deck-relative paths, root `.libpath`, caller search directories, physical origins. Included end markers are made include-safe. |
| Scalar values | Engineering suffix spelling, embedded decimal notation, and micro-unit spelling; preserve opaque node and model names. |
| Declarations | Case-insensitive lexical parameter and model identities; forward declarations and shadowing; static model/formal/actual expressions; validate explicit BJT polarity against the visible model. |
| Native independent sources | Literal, increasing, cumulative PWL coordinates and explicit positive-duration PULSE. Preserve source identity and physical line mapping. |
| Symbolic independent sources | Mandatory occurrence-bound PWL/PULSE declaration in the prepared bundle. Resolve relative coordinates in order, without expanding expressions; validate candidate-specific timing. |
| Transient analyses | One to four time fields, explicit UIC, positive maxstep, symbolic time fields and explicit absolute-time output. Default maximum step is projected as stop/1024 (contemporary documented behavior); conflicting option/card values and relative nonzero output starts require a separate analysis binding. |
| Ordinary native SPICE cards | Forward native-compatible source syntax to the shared normalizer; unqualified source-specific defaults are not proven equivalent. |

If the bundle contains waveform declarations, the circuit text alone is not
the complete simulation artifact. A consumer must bind each declaration to
its named source in the recorded artifact and lexical definition scope, for
every elaborated occurrence and parameter candidate. The native source card
may contain a placeholder DC bias. The resolved declaration separates
operating-point DC from transient startup; the AC card remains independent.
Do not run the placeholder deck as a stand-alone reference.

## Explicit qualification boundaries

- Conditional netlist directives require graph selection **before** resource
  admission. Selected conditional blocks are diagnosed; unselected named
  library sections are not expanded. This includes conditional search paths.
- Known unsupported specialized/digital/compiled device classes are
  diagnosed. External code is never implicitly executed by this adapter.
- Nonzero or symbolic passive series/shunt parasitics, initial conditions,
  implicit inductor shunt, and nonlinear passive laws require the corresponding
  native physical contracts. Explicit root `.option LFT=0` disables the
  omitted inductor shunt (equivalent to explicit `Rpar=0` only when no
  shunt was written). Constant zero expressions are equivalent. Positive LFT
  is admissible only when every inductor has an explicit supported shunt
  setting; otherwise frequency-derived damping requires qualification.
  Symbolic LFT, conflicting values, Q/Qser/Frequency-derived losses, and
  deck-wide LFT in standalone libraries are diagnosed. Do not equate LFT=0 with a
  zero-ohm shunt. In particular, `R=`, `C=`, `Q=` and `Flux=` are not
  interchangeable with static positional values.
- Idealized diode and smooth switch laws require their own model binding.
  QSPICE's nonzero default soft-recovery parameter VP changes the diffusion
  charge law when TT is nonzero. Explicit literal `VP=0` opts into the
  ordinary snap-recovery law; the redundant parameter is removed in that
  projection. Other VP/TT cases requiring the soft-recovery state are
  diagnosed, not silently converted to native default VP=0. Explicit zero TT
  has no diffusion charge; a valid literal VP is then redundant.
- Repeated/file-backed/triggered PWL, default or signed/extended PULSE timing,
  and source parasitics are not projected as ordinary native source cards.
- Boolean XOR (`^^`) is distinct from single-caret exponentiation and is
  diagnosed until source-specific truth/precedence binding is available.
  Other source-specific operators, built-ins, model families, defaults,
  analysis stepping and output/measurement semantics require continued
  qualification. No global simulator compatibility setting is used.

## Reference evidence and ongoing integration

The independently authored regression fixtures exercise OP, DC, AC,
transient circuit laws, scoped parameter overrides, library selection and
original-line diagnostics. C ngspice provides an independent native-reference
engine for projected circuit laws. Prepared waveform tests also check
serialization, exact knot ordinates and occurrence binding. These are not
direct QSPICE binary reference traces.

Public source behavior references:

- [Source encoding](https://forum.qorvo.com/t/can-text-comment-support-chinese-utf8/16722/3),
  [library search paths](https://forum.qorvo.com/t/tutorial-libpath-directive/16303),
  [numeric spelling](https://forum.qorvo.com/t/bug-unit-prefixes/14163)
- [Parameter and subcircuit syntax](https://forum.qorvo.com/t/params-keyword-on-x-call-and-in-subckt-definition-make-this-optional/14301),
  [model scope](https://forum.qorvo.com/t/cascode-model-works-in-ltspice-but-not-in-qspice/14421),
  [BJT polarity](https://forum.qorvo.com/t/manually-and-shematic-generated-netlist/15861),
  [boolean XOR versus exponentiation](https://forum.qorvo.com/t/ti-tps63805-unencrypted-model-doesnt-run-in-qspice/23185)
- [Cumulative PWL timing](https://forum.qorvo.com/t/interesting-feature-increment-operator/25677),
  [transient output timing](https://forum.qorvo.com/t/post-processing-issue-in-qspice-simulations/22858),
  [maximum-step default](https://forum.qorvo.com/t/oscillator-problems/25473/10)
- [Conditional source topology](https://forum.qorvo.com/t/are-there-examples-of-using-the-new-sim-directives-display-if-else-and-endif/23763),
  [behavioral impedances](https://forum.qorvo.com/t/behavioral-impedances-r-l-and-c/15098),
  [soft reverse recovery](https://forum.qorvo.com/t/there-is-a-strong-difference-in-the-results-in-the-qspice-and-ltspice-programs-when-calculating-the-simplest-circuit/16472/2)

Shared integration work:
[conditional source graphs](https://github.com/codeandsolder/ngspice-rs-preprocess/issues/66),
[waveform execution](https://github.com/codeandsolder/ngspice-rs-fast/issues/223),
[expression binding](https://github.com/codeandsolder/ngspice-rs-fast/issues/227),
[passive networks](https://github.com/codeandsolder/ngspice-rs-fast/issues/228),
[idealized diode laws](https://github.com/codeandsolder/ngspice-rs-fast/issues/226),
[switch laws](https://github.com/codeandsolder/ngspice-rs-fast/issues/229),
[soft-recovery charge state](https://github.com/codeandsolder/ngspice-rs-fast/issues/238).
