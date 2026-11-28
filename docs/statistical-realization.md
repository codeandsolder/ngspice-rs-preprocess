# Statistical stream definition

The statistical source adapter emits exact 32-bit words for the seed, sample
index and static source-call ordinal. These values are not floating-point
hashes or traversal counters. The lexical consumer binds declarations,
physical occurrence paths and model owners before numerical evaluation.

## Identity and random stream

Sampling identities are 128-bit XXH3 values, with separate fixed domains
for roots, byte components, ordered parent/child composition and ordered
pairs of already-bound identities. Composition hashes fixed-width
little-endian records, so component boundaries, embedded zero bytes,
ordering and hierarchy are unambiguous. These keys identify random
streams only. Electrical object equality and binding use the actual
declaration/occurrence identities, never a hash match alone.

The prepared stream key combines the canonical distribution name, expression
identity (including the static invocation ordinal) and the physical owner.
Independent draws use the physical occurrence; lot draws use the actual
bound model owner. Numeric tolerances, realization seed and sample number
are not part of this key.

The generator is Philox4x64-10, with the low and high halves of the stream
key as the two Philox key words and counter coordinates
[block index, sample index, seed, 0]. The full 64-bit seed, sample and block
coordinates are retained. The four words of a block give independently
addressable scalar draws; a normal proposal uses two words of the same
block. A uniform value is the midpoint of one of 2^52 equal intervals in
(0, 1). The endpoints are excluded. Truncated Gaussian sampling uses the
existing bounded rejection laws with distinct indexed uniform pairs.
Distribution transforms, finite-domain validation and lot semantics are
unchanged.

The reference permutation is checked against the three Philox4x64-10
[Random123 known-answer vectors](https://github.com/DEShawResearch/random123/blob/main/tests/kat_vectors).
XXH3 is pinned to an independently available release in the workspace
lockfile. No cryptographic hash is required for these internal keys and
draws. Artifact and source integrity digests remain separate.

The new stream deliberately changes historical seed-to-exact-sample
mapping. Reproducibility requires the same stream algorithm and lexical
identities; no previous-generator compatibility mode is maintained.
Exact integer/uniform streams are platform-independent. Floating-point
transcendental results may differ by ordinary library rounding across
platforms; they are not an independent source of random state.

## Preparation boundary

Validate and bind the distribution before repeated realization. Do not
reconstruct stable declaration paths or density tables for each Monte
Carlo draw. If the law's evaluated tolerance operands change, validate
the changed law while preserving the original stream identity.
