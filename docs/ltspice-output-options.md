# LTspice raw-file presentation options

The LTspice dialect projects `plotwinsize`, `numdgt`, `plotreltol`,
`plotvntol` and `plotabstol` to native `ltspice_output_*` option
names. These five control compression or serialization of saved output,
not the electrical problem. The original assignment order, scalar
expressions, source line mapping and unrelated options are retained.
The native consumer evaluates the scalar in its ordinary lexical scope,
requires a finite number and records the setting as an output-only no-op.
It does not reproduce LTspice's proprietary raw-file representation.
Undefined or nonnumeric values remain errors; arbitrary unknown options
are not dropped. Native ngspice input is not projected.

LTspice 24.0.12 accepts explicit assignments, whitespace-separated
numeric values, duplicate settings, finite fractional and negative
values, and parameter expressions for these settings. Bare names without
values and undefined symbols fail. Independent transient decks with
compression enabled and disabled exercise these settings; their raw
serialization differs, not the underlying circuit definition.

Reference: [LTspice .OPTIONS](https://ltspicehelpmanual.azurewebsites.net/options--setsimulatoroptions.htm).
