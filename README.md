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
