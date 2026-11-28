# Prepared physical Taylor evaluation (ABI 6)

The immutable descriptor declares `TaylorCapability::Computed { max_order }`
or `Unavailable`. There is no ordinary-point compatibility fallback. The
SIMetrix level-1 diode and native VDMOS artifacts support degrees 0–64;
other families must opt in with their own exact constitutive implementation.

## Coefficient contract

The circuit engine supplies a right-hand local state germ around an exact
physical point. With dimensionless local time `u = (t-t0)/scale`, coefficient
`k` is the kth physical time derivative multiplied by `scale^k/k!`.
Coefficients use the descriptor's coordinate order and physical units,
including absolute kelvin for thermal rows. Parameters and ambient temperature
are immutable over evaluation. Hidden physical coordinates are supplied in the
same order as public terminals. No limiting or timestep integration occurs.

For width `n` and degree `p`, the input is `(p+1)*n` scalars. The output is
four caller-owned arrays: `current` (F) and `charge` (Q) each `(p+1)*n`,
`conductance` (dF/dx) and `capacitance` (dQ/dx) each `(p+1)*n*n`.
All are degree-major; matrix entries within each degree are row-major.
In particular, `G[k*n*n+i*n+j]` is the kth normalized coefficient of the
instantaneous physical partial derivative of F_i with respect to x_j,
not the derivative of an integrated companion residual.
The chain rule in coefficient form is

```text
(k+1) F[k+1] = sum(j=0..k) G[j] * (k+1-j) x[k+1-j]
(k+1) Q[k+1] = sum(j=0..k) C[j] * (k+1-j) x[k+1-j]
```

At a constitutive boundary compare the constant first; if equal, the first
nonzero higher coefficient determines the one-sided branch. If the germ is
identically on the boundary use the scalar branch convention. The returned
constant and partial derivatives belong to that right-hand branch, including
at discontinuous joins. No bias epsilon is introduced. Nonregular or
nonrepresentable series are rejected. The consumer must handle actual time
discontinuities outside the constitutive law.

`Artifact::evaluate_taylor` and `PreparedTile::evaluate_lane_taylor` use
the same immutable physical data and validation. Selective tile replacements
change the prepared recipe, not the physical history. `TaylorBuffer` is
reusable; its dimensions are checked before evaluation. Input plus output
are bounded to 1,048,576 scalars, order to 64; family arithmetic uses fixed
capacity local series. The caller-owned guard trace lazily reserves bounded
space and reuses it on subsequent evaluations. Failed output must be discarded. All four arrays must be complete and
finite; electrical conservation and common-mode invariance are checked at
every degree. Known-zero charge must remain zero.

## Executed physical constitutive surfaces

Each successful Taylor evaluation returns the executed, physically active
branch sites in `TaylorPoint::guards.records()`. Every record contains a
stable family-local `site`, the strict `Less` or `Greater` predicate, the
actual selected boolean, and the normalized signed operand-difference
polynomial. Constants and lower-degree operands broadcast zero. The same
comparison chooses the constitutive branch and emits its surface, so a
solver never reconstructs thresholds or relies on scalar evaluations at an
ambiguous exact join. Equality with no nonzero higher coefficients selects
the non-strict complementary branch. Inactive and unexecuted sites are
absent, not assigned synthetic regions.

The trace is reset on each checked standalone or lane evaluation. Duplicate
sites, nonfinite or inconsistent surfaces, degrees beyond the input order,
and more than 256 sites are rejected. Guard polynomials are prediction
hints, not proof of crossings: the consumer must localize and verify actual
one-sided solutions and handle rollback, coincidence and discontinuities.
Stable site identity is scoped by artifact family and bound device instance;
do not assign global meaning to site numbers or rely on trace order across
different families.

The SIMetrix diode records positive-current injection and depletion
continuation when active. VDMOS records channel direction (including the
quasi-saturation branch), mobility threshold, hard channel cutoff, triode/
saturation boundary, gate-drain charge join, reverse diode continuation,
avalanche, its temperature-dependent matching regime, and thermal depletion potential/continuation. Numerical
rearrangements of analytic functions (scaled exponential, softplus, etc.)
are not physical branch sites. Invalid constitutive domains are errors,
not synthetic guards.

## Constitutive implementation

The diode instantiates the same generic current, depletion, diffusion,
recombination, high-injection and avalanche laws and analytic slopes over
scalar or Taylor arithmetic. VDMOS lifts its analytic partial derivatives,
channel, body diode, gate charge, series resistance, electrothermal and
hidden recovery equations to the same arithmetic, sharing nodal assembly.
The avalanche temperature root uses the converged scalar root and formal
series Newton iteration, not differentiation of a finite scalar iteration.
The matching excess is retained separately from one and the positive root
is bracketed above the tangent, avoiding cancellation near the matching boundary.
The existing recovery row transformation is retained; no fictitious storage
term is introduced for a temperature-dependent recovery rate gain.

The depletion integral uses the entire exponential quotient `expm1(z)/z`,
including its regular derivative at `z=0`; near that point its shifted
Taylor coefficients follow the integral of `exp(z*t)*t^k` on `[0,1]`.
This preserves higher temperature derivatives through grading exponent one
instead of differentiating a fixed cubic approximation. Linear coordinate
maps use compact scalar incidence coefficients; only nonlinear physical
partials are series-valued.
