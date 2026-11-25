# IHP SG13G2 VBIC device-level numerical oracle

The fixture is the nominal, resolved Nx=4 npn13G2 core parameter set from
[IHP Open PDK](https://github.com/IHP-GmbH/IHP-Open-PDK/tree/af6b1ebc82c36daf30516a46d3a0a6a5777df0c1/ihp-sg13g2/libs.tech/ngspice/models).
Copyright 2023 IHP PDK Authors; source licensed Apache-2.0. The numeric
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
