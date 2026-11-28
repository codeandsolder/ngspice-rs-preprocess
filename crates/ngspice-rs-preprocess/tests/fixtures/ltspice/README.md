# Independent LTspice scalar observations

Synthetic circuit from the first 20 behavioral sources in
`vendor_operator_precedence_and_table_limiter_dc_sweep`. Run in LTspice XVII
and LTspice 24.0.12, using a DC sweep of Vx from -3 V to +3 V in 0.25 V steps,
with reltol=1e-10, abstol=1e-14 and vntol=1e-12. The ASCII raw files were
reduced to the independent sweep coordinate, V(x), then V(a) through V(w)
in source order, with 12 significant digits. Both versions produced exactly
the same 25 rows (SHA-256
`520d54243c7eb1fe5aa7a6f90dda9fc555fcd66d6d535f381ec8300322ed6f8e`).
The native comparison retains the strict static 0.5 Boolean threshold
and accounts for floating-point Newton trial ambiguity exactly at a runtime
discontinuity.

The extra edge cases in the test were verified separately in 24.0.12,
including duplicate table coordinates, zero-width and negative-width
limiters, and zero-base exponentiation. The AC probe independently measured
gain -2 for the interpolated table, exp(-0.5) and -exp(-0.5) for the smooth
limits, zero outside a table and for a single-point table, and +2 for the
linear multi-point table.
