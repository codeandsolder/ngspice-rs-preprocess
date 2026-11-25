# Prepared SIMetrix level-1 diode kernel

This crate implements independently measured SIMetrix Elements 9.40d level-1
diode physics. It is not a replacement for the ngspice diode dialect.

`compile_simetrix` accepts normalized numeric source parameters, validates
their domain, and returns an immutable model. `bind` resolves instance
temperature, area, multiplicity and gmin once. `evaluate` returns signed
intrinsic current, charge and their exact bias derivatives. The circuit
solver owns series resistance topology, Newton limiting, history and
integration. `noise_density` returns independent local current PSDs for
shot, flicker and positive-series-resistance thermal noise. No per-bias
allocation or source parameter lookup is required.

## Measured physical laws

* Historical constants: k = 1.3806226e-23 J/K, q = 1.6021918e-19 C.
* Temperature-adjusted saturation, transport high-injection knee,
  depletion-dependent recombination, additive reverse avalanche terms.
* Depletion capacitance below FC: C0(1-v/VJ)^(-M), with a continuous
  linear capacitance extension above FC. Charge is its analytic integral,
  including the exact M=1 logarithmic limit.
* Diffusion charge = TT times total physical intrinsic current (including
  avalanche and recombination, excluding gmin).
* Shot PSD = 2q|I|, including gmin. Flicker PSD =
  M KF max(|I|/M, 1e-38)^AF / f, where M is instance multiplicity.
  Series resistor PSD = 4kT/RS_effective; the circuit applies the
  appropriate complex transfer functions to each independent source.
* Effective series resistance includes temperature and area/multiplicity.
  Signed resistance is retained for electrical analysis; noise of a
  negative resistance is explicitly unsupported pending characterization.

Other diode levels, unrecognized source parameters and nonrepresentable
results are rejected rather than silently approximated. This is a
physical kernel; solver integration is tracked separately (
ngspice-rs-fast #189).

## Validation

    cargo test -p model-kernel-diode --all-targets
    cargo clippy -p model-kernel-diode --all-targets -- -D warnings

Checked-in TSVs contain measured values, not generated expectations:

* 504 DC currents, 80 AC magnitudes and 80 AC phases, authored witnesses.
* 12,420 transient current samples (36 configurations at -30, 27, 95 C).
  Only exact ramp slope discontinuities are excluded. Five decks with
  trapezoidal integration ringing were remeasured using Gear, not looser
  acceptance tolerances.
* 20 temperature precedence and alias witnesses.
* 525 branch-resolved noise spectral amplitudes (36 configurations, seven
  frequencies), gate 0.5 ppm relative, including gmin-only and zero bias.
* Independent derivative, continuity, extreme-bias, batch and domain tests.

The optional manufacturer holdout contains 72 load-line currents and
voltages (144 observations), including all 18 previously failing cases:

    cargo run -p model-kernel-diode --example replay -- /path/to/diode-load-holdout.tsv

Maximum manufacturer errors in the external holdout: 48.05 pV and 38.89 pA.
The holdout is optional and not distributed; the checked-in tests use
independently authored numerical witnesses. Fixture hashes are recorded
in tests/fixtures/SHA256SUMS.
