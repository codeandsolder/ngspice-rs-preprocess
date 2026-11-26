# Native BSIM3SOI temperature preparation witnesses

The reference is the original ngspice 47 FD/DD 2.1 and PD 2.2.3 compiled
model parameter, instance parameter, setup, model-check and temperature
routines. The harness supplies only allocations, circuit node numbering,
matrix storage and diagnostics; it does not reimplement any model
equations. Every fixture coefficient is printed directly from the
original model, instance or size-dependent parameter structure.

The 18 cases cover both polarities, 245–390 K, explicit and default
nominal temperatures, doping and body coefficients, length/width/cross
binning, explicit threshold and substrate properties, PD layout and
thermal geometry, signed substrate doping, and PARAMCHK corrections.
All values must be finite. The Rust test consumes the fixed full-precision
reference without requiring a C compiler or native ngspice installation.

Regenerate using a configured and compiled ngspice 47 tree, with source
and object directories named src and obj, and a caller-owned existing
build directory:

    bash regenerate.sh /path/to/pinned-c /path/to/build

The compiler uses the managed sccache client. The temporary C build and
diagnostics are removed after the fixture is generated.
