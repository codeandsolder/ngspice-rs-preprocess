# OSDI artifact preparation

The binary envelope inspector reads an explicit ELF shared-object byte snapshot,
checks its exported OSDI ABI version, descriptor count, descriptor stride and
array extent, and records SHA-256 and target architecture. It never loads a
library or executes constructors. OSDI 0.3 uses its fixed pointer-width-specific
layout; OSDI 0.4 declares the stride. Other revisions and non-ELF formats are
rejected rather than guessed.

This envelope is only the first stage: descriptor-level schema, relocations,
capabilities, typed instance binding and compiler/source provenance still need
validation before producing a runtime-ready artifact. The numerical solver owns
explicit runtime loading and stepping, not compiler invocation.

References: [OSDI specification](https://openvaf.semimod.de/osdi/osdi_v0p3.pdf),
[OpenVAF-Reloaded](https://github.com/OpenVAF/OpenVAF-Reloaded).

`cargo run --locked -p model-prepare-osdi --example inspect -- /absolute/model.osdi`

Tests compile independently constructed C export fixtures on Linux and verify
that even an aborting library constructor is never invoked.
