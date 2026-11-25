# ngspice-rs-preprocess

Standalone SPICE normalization, dialect lowering, source linking,
parameter compilation and reusable prepared model kernels.

The workspace contains the ngspice normalizer, SIMPLIS source preprocessor and
translator, and VDMOS prepared numerical kernel. The ALB/XSPICE runtime is
experimental; unsupported source semantics remain explicit errors.

The simulator owns numerical circuit orchestration. An optional external
command will allow callers to supply a source transformation before
normalization. Ordinary plaintext inputs need no external command.

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
