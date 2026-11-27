# Standalone package release qualification

The root workspace is the source distribution; its default members are the
production registry package set. Experimental ALB/XSPICE packages are available
only in the complete source checkout, with their separate runtime fixtures.
The CLI does not link either experimental crate.

## Version and dependency policy

All workspace packages currently inherit the release version 0.1.0. Internal
dependencies are centralized in `workspace.dependencies`: the local path is
used in the source checkout, and the exact version requirement is used when
Cargo generates a registry manifest. Update both the package version and every
internal version requirement as a single release change. The metadata check
requires every production dependency to be either a member of the production
set or a public crates.io dependency; it includes development and
target-specific dependencies. It rejects an unversioned path or Git dependency.
The shared external-dependency release-age quarantine remains in force.

Every production package carries its actual license text. Packages with an
SPDX license retain that metadata and include a local LICENSE. MIT texts must
match the root LICENSE; the MIT/BSD packages carry the root third-party notices
(including the MIT text and relevant upstream attributions). The bipolar package also
retains the Apache-2.0 IHP Open PDK fixture license. The Touchstone
codec has its own BSD license. The intrinsic compact-model package has a
non-SPDX license-file preserving the HiSIM/HiSIM-HV attribution and noncharging
conditions, legacy BSIM and BSIM SOI Berkeley BSD notices, and STAG DERA licence. The latter requires
specific review before registry distribution; no blanket MIT claim applies.
Review additions against the source licenses, not just registry metadata.

## Verification

Ordinary source-archive CI checks the release graph and license files:

```sh
bash .github/scripts/verify-packages.sh --metadata-only
```

The same shared managed-sccache CI runs the full qualification on release tags
and manual dispatch. Locally use the pinned nightly and a dedicated configured
target directory, with the existing managed compiler wrapper:

```sh
bash .github/scripts/verify-packages.sh
```

The command takes a tracked source archive of HEAD, without Git metadata or
local untracked files. It validates the graph before setting registry
publication allow-lists **only in the temporary archive**. Cargo intentionally
excludes `publish=false` dependencies from its temporary registry overlay.
The actual checkout remains unpublishable. Cargo's multi-package
`publish --dry-run --locked` assembles and verifies every production tarball
in dependency order, using its temporary registry overlay for unpublished
workspace dependencies. No upload occurs.

The verifier compares all tracked crate file bytes against the staged source,
including Cargo.toml.orig before normalization, and rejects unexpected files.
It also compares packaged license/readme bytes and checks that normalized
manifests contain only registry dependencies. It then builds a temporary checksum-verified directory source
from the exact public dependencies in Cargo.lock and the actual local package
tarballs. Every packaged lockfile resolves offline against that source. It
performs a real offline registry installation of the CLI (not a path install),
and runs it against an independently authored native deck outside the source
tree. This verifies the package-format/install boundary without requiring a
published workspace package or registry credentials. Temporary source,
registry and install trees are removed; compilation uses the isolated
managed target and sccache.

## Publication gate

A successful qualification is not a release or proof of registry ownership.
The repository retains `publish=false`. Before an explicitly authorized
release, review source and history provenance, verify each package name and
version in the public registry, establish ownership or trusted publishing,
and review the component-specific license conditions. Only then enable the
approved registry publication allow-lists in a reviewed release change.
Publish in Cargo's dependency order (the qualification logs the order);
each exact version must be visible in the public registry before its
dependents are released. Repeat the locked dry run and install against the
actual public registry after publication, and qualify the downstream solver
with the exact released kernel API and other kernel dependencies. Do not add
credentials or private-source fallbacks to make a public build pass.
