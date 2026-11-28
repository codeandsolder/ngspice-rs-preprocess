# Source expression witnesses

These independently simulated source decks and numeric references cover
absolute-base powers (including the complex small-signal Jacobian at zero),
explicit charge expressions written in all three source grouping forms,
and capacitor temperature coefficients. All numeric reference files are
preserved without resampling. The tests replay the prepared deck through
ngspice 47 at the original operating points and frequencies. Transient
comparison uses a 100 ns output grid and 0.5 ns maximum internal step.

`prepared-signed.net` combines the signed-power and zero-factor witnesses
from the solver's qualified SIMetrix power-law tests into one source deck.
The prepared-only cross-repository gate exercises the original `^`, `POW`,
`**`, ordered singular product, and a user-defined runtime function, rather
than hand-writing their lowered solver representation. Run it with the
qualified `spice-rs` executable on PATH and `cargo test -p
ngspice-rs-preprocess --test runtime_expressions prepared_ -- --ignored`.
