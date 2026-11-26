# Native prepared compact-model contract

This dependency-free crate owns the shared Rust model/engine interface.
An artifact holds immutable model-specific data and its validated descriptor;
the circuit engine binds coordinates and owns Newton iteration, numerical
integration, limiting, accepted/rejected trial state and matrix assembly.

## ABI version 3

The producer supplies a stable content fingerprint and an ordered descriptor.
Every coordinate declares its physical domain and terminal/hidden binding.
Electrical coordinates use volts, static amperes and stored coulombs;
thermal coordinates use absolute kelvin, watts and joules; internal
coordinates have family-defined physical meanings.

Each kernel implements the generalized residual

```text
F(x) + dQ(x)/dt = 0
G = dF/dx
C = dQ/dx
```

The engine adds the affine Newton right-hand side and integrates stored
quantities with its own accepted history. DC uses F and G; linearized AC
uses G + j*omega*C. Kernel evaluation is stateless. A failed trial must not
be consumed. A new candidate is prepared independently; a structural slot
cannot change without constructing a new artifact and rebinding topology.
Source-level parameter parsing is not part of this interface.

- `Artifact<K>` validates and owns the descriptor and kernel. Individual
  `Prepared<K>` objects belong to that exact artifact, even when two
  fingerprints happen to agree.
- `PointBuffer` and `batch::BatchOutput` retain caller-owned output storage.
  `batch::PreparedTile` owns contiguous, homogeneous prepared lanes.
  The default traversal can be replaced by a generated or SIMD kernel.
  No allocation is required on a successful numerical path.
- At candidate boundaries, `PreparedTile::stage_replacements` takes
  an exact artifact reference and changed lane recipes. It checks all
  ordinals, prepares only the changed lanes, and returns an exclusive
  transaction. Commit installs the prepared values in the existing
  contiguous storage. Dropping a transaction aborts without changing the
  tile. Stage disjoint tiles before committing any of them when the engine
  changes several tiles together. A failed preparation must not publish a
  partial candidate; structural edits still need a new artifact.
- `PreparedTile::from_prepared` transfers already prepared standalone
  instances into the tile without invoking physical preparation again.
  Scalar and noise evaluation of grouped instances use `evaluate_lane`
  and `evaluate_lane_noise` on that same tile, with the same validation as
  standalone evaluation. No lane handle can outlive its tile or be
  retained across a replacement; ordinals are local to the owning tile.
- Static residual and Jacobian are mandatory. Charge/flux and noise can be
  computed, explicitly zero, or unavailable; unavailable is never
  interpreted as zero. Outputs are reset to missing sentinels and validated.
  KCL and charge conservation apply only to electrical rows. Electrical
  reference invariance applies to all equations. Conservation checks scale
  before summing to prevent overflowing near the floating-point range limit.
- `noise::Source` is a nonnegative spectral density with a borrowed complex
  injection vector expressed in model-local row ordinals. All terms of one
  source are coherent; distinct sources are independent. The consumer
  owns adjoint transfer, global row binding and frequency integration.
  Discard partial sink results if noise evaluation fails.

Use the existing pinned nightly and managed build cache:

```sh
cargo test --locked -p model-kernel-api -p model-kernel-vdmos
cargo test --locked -p model-kernel-vdmos --test ngspice47 -- --ignored
```

The VDMOS and SIMetrix level-1 diode artifacts implement this contract.
Their circuit-coupled engine integration is a separate task. This is a native Rust source-level
contract, not a stable dynamic-library ABI.
