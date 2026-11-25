# OSDI artifact preparation

This crate validates explicit ELF shared-object snapshots. It records their
SHA-256 identity and verifies OSDI 0.3/0.4 descriptor layouts, exported tables,
relocated metadata pointers, executable callback addresses, node topology,
residual and Jacobian offsets, parameter scopes/types/aliases, collapse pairs,
and noise sources. No constructors or device callbacks execute in this step.
Unsupported ABI revisions or relocation types are rejected explicitly. OSDI
0.4 adds descriptor-stride, extended callback and noise metadata; the reader
checks the known target-width-specific layout rather than guessing the ABI.

`ModuleSchema::bind` resolves model or instance assignments to ABI ordinals,
rejects ambiguous/duplicate aliases, nonfinite reals, incorrect types and
array shapes, and ties the result to the exact module SHA-256. Omitted slots
are intentionally left for model/instance setup: their defaults are not
encoded in this static descriptor.

`compiler::Compiler` is an explicit, bounded two-stage OpenVAF invocation.
It resolves source includes/macros with `--print-expansion`, freezes those
exact expanded bytes, and compiles them in a request-local temporary directory.
The result records root and expanded source hashes, compiler binary hash and
version, target, CPU and optimization; the immutable output also includes
bounded compiler diagnostics. The caller explicitly specifies source,
compiler, scratch and include directories. No implicit compiler or runtime
loader is used. The chosen compiler must support the requested target.

Examples:

```sh
cargo run --locked -p model-prepare-osdi --example inspect -- /absolute/model.osdi
cargo run --locked -p model-prepare-osdi --example prepare -- /absolute/openvaf-r /absolute/scratch /absolute/model.va /absolute/model.osdi
```

The example prepare command selects x86-64 Linux, generic CPU, optimization 3.
The library supports an explicit target configuration. C-compiled test
fixtures cover both layouts, malformed metadata and typed binding, including
an aborting constructor that must never run. The numerical solver owns
explicit runtime admission, setup, stepping, limiting, state and noise
integration. Static schema validation is not itself execution conformance.

References: [OSDI specification](https://openvaf.semimod.de/osdi/osdi_v0p3.pdf),
[OpenVAF-Reloaded](https://github.com/OpenVAF/OpenVAF-Reloaded).
