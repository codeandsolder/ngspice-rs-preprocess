# Legacy bulk BSIM preparation and kernel ownership

## Scope and evidence

This audit is pinned to ngspice source revision
`fa6c2a236507fd440c671de237cad4f49a4e6b68` and the accepted Rust solver revision
`89570506ea17d9b2cde2252749730201bd86dd7d`.
The upstream dispatch is in `src/spicelib/parser/inpdomod.c` and
`inpfindv.c`; device tables, setup, temperature, load, AC, pole-zero
and noise routines are in the individual directories under
`src/spicelib/devices`. The solver's existing
[BSIM port document](https://github.com/codeandsolder/ngspice-rs-fast/blob/89570506ea17d9b2cde2252749730201bd86dd7d/docs/port/BSIM.md)
is the starting point for preserving numerical behavior, not a claim
that every listed mode is implemented.

## Distinct device identities

| Level | Version | Source directory | Model inputs | Instance inputs (ask-only excluded) | Charge model selection |
| --- | --- | --- | ---: | ---: | --- |
| 4 | independent BSIM1 family | bsim1 | 81 | 14 | Own equations, no capMod selector |
| 5 | independent BSIM2 family | bsim2 | 137 | 14 | Own equations, no capMod selector |
| 8/49 | 3.0.x | bsim3v0 | 354 | 12 | capMod=1 branch versus alternate branch; default 1 |
| 8/49 | 3.1.x | bsim3v1 | 367 | 12 | capMod 0/1/2; default 2 |
| 8/49 | 3.2.x | bsim3v32 | 428 | 15 | capMod 0/1/2/3; default 3 |
| 8/49 | 3.3.x/default | bsim3 | 429 | 19 | capMod 0/1/2/3; default 3 |

The exact input catalogs are generated from the six IFparm tables. Every
alias uses the same canonical slot as its upstream setter ID; OP-only rows
are not inputs. BSIM1/2 IC uses a real vector as confirmed by their
instance setters. The BSIM3 revisions differ even in the type and presence
of the version setter: absent in v0, real in v1, and string in v32/v33.
Source dispatch precedes applying the ordered setter stream. The selector
recognizes the upstream 3.x version prefixes and an absent or literal
`default`, rather than accepting arbitrary strings containing `default`.
No unknown revision is silently mapped to the newest kernel.

All four BSIM3 revisions declare NQS selectors; v33 adds separate
`acnqsmod` model and instance control. The corresponding charge node,
state equations and AC behavior cannot be reduced to a capMod change.
The source branches identify work to audit, not a license to accept every
integer selector: effective defaults, admissible values, geometry and
temperature prerequisites must be established before kernel preparation.

## Existing numerical ownership

At the audited solver revision, native BSIM3 v3.3 implements DC, OP,
capMod=3 quasistatic terminal charge, transient integration, AC, PZ, TF,
and noise. Distortion is linearized consistently with the C device.
The native device explicitly rejects the other charge modes, NQS,
nonzero ACM and older BSIM revisions; sensitivity is not implemented.
Existing v3.3 numerical evidence includes `c_bsim3_dc_reference`,
`c_bsim3_charge_reference`, `c_bsim3_small_signal_reference`,
`c_bsim3_device_reference`, `c_bsim3_setup_reference` and the native
BSIM3 unit and integration suites. Native BSIM4 remains a separate,
extensively tested 4.8.x device and is not an alias for BSIM3.

The catalogs in this crate are an input schema only. No legacy-BSIM
producer artifact or numerical kernel is advertised. In particular the
new v33 input metadata does not transfer ownership of the existing
working solver implementation. The producer should eventually own
immutable physical preparation, while the consuming solver continues
to own topology, Newton limiting, sparse stamping, integration history,
and analysis orchestration.

## Migration sequence and acceptance

1. Coordinate with the active BSIM3 charge, NQS, sensitivity and arithmetic
   campaigns before extracting physical routines. Reuse the validated v33
   preparation and quasistatic constitutive implementation; do not create
   a parallel independent implementation. Retire the old owner atomically
   when the replacement is accepted.
2. Resolve version, aliases, given flags, ordered setters and binning once.
   Separate model preparation from per-instance geometry and temperature.
   The schema does not supply defaults; missing assignments must remain
   distinct from zero until actual family setup rules have run.
3. Bind prepared quantities and the physical terminal/hidden-equation
   layout to the shared model-kernel API. Current residuals, charge and
   their Jacobians must share coordinates and preserve orientation,
   conservation and derivative consistency. Keep solver history and
   limiting out of the producer.
4. Validate v33 capMod=3 against the existing solver/C reference evidence
   before switching the native owner. Then add other capMod modes and NQS
   with their own source equations and charge-state references. Coordinate
   sensitivity with the common derivative contract, rather than inventing
   an alternate device API. Check prepared-artifact identity and batch
   equality for model, revision, mode, temperature and geometry changes.
5. Implement each older BSIM3 revision and BSIM1/2 with its own setup and
   physical semantics. Reuse algebra and storage only when equivalent.
   Require independent C OP/DC, current and charge derivatives, AC,
   transient and noise comparisons wherever the corresponding reference
   supports them, including both polarities, reverse orientation,
   geometry and temperature corners, and mode-dependent topology.
   Keep unsupported analyses explicit until validated.

Until these steps are complete, recognizing a source card is not evidence
of physical simulation support.

## Reproducing the catalog

From this repository's root, using a checkout of the pinned reference:

```sh
uv run --no-project python crates/model-compact-intrinsic/tools/generate_legacy_bsim_catalog.py /path/to/ngspice | diff - crates/model-compact-intrinsic/src/catalog/legacy_bsim.rs
```

The generator copies only interface facts (name, type, access and alias
identity), not formulas or source descriptions. The schema tests check
revision selection, counts, sorted unique names, dense canonical slots,
ordered aliases, rejected ask-only inputs and version-specific types.
