# IHP SG13G2 VBIC device-level numerical oracle

The fixture is the nominal, resolved Nx=4 npn13G2 core parameter set from
[IHP Open PDK](https://github.com/IHP-GmbH/IHP-Open-PDK/tree/af6b1ebc82c36daf30516a46d3a0a6a5777df0c1/ihp-sg13g2/libs.tech/ngspice/models).
Copyright 2023 IHP PDK Authors; source licensed Apache-2.0. See the
[original license](LICENSE-IHP), copied from the pinned source revision. The numeric
realization and measured simulation outputs are stored, not the source model
library. Resolve with native ngspice 47 listing e and showmod all; the
nominal temperature-dependent TF expression is evaluated at 27 °C. The
original external substrate RC and thermal wrapper are intentionally removed
to compare the native four-terminal VBIC core with the Rust prepared kernel.

generate_vbic_device_oracle.sh PDK_MODELS_DIRECTORY OUTPUT_DIRECTORY
reproduces both files. No network is needed by the tests. The 20 rows cover
four ambient temperatures, forward active, high injection, saturation and
reverse bias. AC uses a 1 V small-signal base drive, 1 GHz, and the other
terminals held by ideal voltage sources. Currents are signed into the device.

The thermal fixture adds twelve independent forced-rise operating points and
complex thermal-drive responses, using the published Nx4 thermal R/C,
three ambient temperatures, two junction-temperature rises and two biases.
Thermal drive is a unit source at the native temperature-rise terminal.
The shared model ABI instead exposes absolute junction temperature (kelvin);
the difference from effective instance ambient is the native rise.

The raw AC thermal-current response from ngspice 47 is retained in the
fixture. Its heat-generation derivative has the opposite sign to the
converged DC residual: vbicload.c computes Ith = -sum(V*I), whereas
vbicacld.c stamps -dIth/dT. At -40 °C, zero rise, VBE=0.78 V and
VCE=1.2 V, the native finite-difference heat slope is
0.0003058228383 W/K, but the native AC real response is
0.0003076740937 W/K. Their sum is 2/Rth. The physical Jacobian is
checked against independent native DC sweeps (±0.001 K) and AC
thermal admittance reflected about 1/Rth+jωCth. Electrical AC
responses are compared without modification.

The excess-phase fixture additionally enables TD=2 ps and measures native
DC and AC at 1 GHz and 100 GHz at two ambient temperatures. The free
thermal equilibrium fixture removes the imposed rise source and measures
the converged junction rise, internal collector voltage, and terminal
currents at four operating points.
