# ngspice-rs-preprocess

Standalone SPICE normalization, dialect lowering, source linking,
parameter compilation and reusable prepared model kernels.

The workspace contains the ngspice normalizer, SIMPLIS source preprocessor and
translator, and VDMOS prepared numerical kernel. The ALB/XSPICE runtime is
experimental; unsupported source semantics remain explicit errors.

The simulator owns numerical circuit orchestration. Source admission is
handled by `model-preprocess-command` before normalization, linking or model
preparation. The default does not launch any external process.

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
deadline. The input stream and both output streams are handled concurrently;
the deadline includes waiting for the process to exit after its pipes close.
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
