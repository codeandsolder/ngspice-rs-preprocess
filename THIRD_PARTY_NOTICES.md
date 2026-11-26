# Third-party notices

Project-authored files use the root MIT license except where a component
specifies a different license. The following notices apply to the relevant
components and must be retained in source and binary distributions.

## VDMOS physical model

The VDMOS implementation in `crates/model-kernel-vdmos` is based on the
ngspice 47 numerical model, revision
[`a80f6e3e95d51534905b1f23410a951802666656`](https://sourceforge.net/p/ngspice/ngspice/ci/a80f6e3e95d51534905b1f23410a951802666656/tree/).
The relevant upstream files include
`src/spicelib/devices/vdmos/vdmosload.c`, `vdmostemp.c`,
`vdmosset.c`, `vdmosnoi.c`, and the VDMOS capacitance equations in
`src/spicelib/devices/devsup.c`. The Rust implementation retains the
upstream physical equations while independently representing preparation,
derivatives, state and numerical evaluation.

Upstream file notices:

- Copyright 1990 Regents of the University of California. All rights reserved.
- Thomas L. Quarles (1985); modifications by AlansFixes (2000);
  VDMOS contributions by Holger Vogt (2018) and Dietmar Warning (2020).
- Noise-model author Gary W. Ng (1987).

The applicable upstream modified BSD notice, from ngspice's COPYING file:

Copyright 1985 - 2018, Regents of the University of California and others

Redistribution and use in source and binary forms, with or without modification,
are permitted provided that the following conditions are met:

1. Redistributions of source code must retain the above copyright notice,
this list of conditions and the following disclaimer.

2. Redistributions in binary form must reproduce the above copyright notice,
this list of conditions and the following disclaimer in the documentation
and/or other materials provided with the distribution.

3. Neither the name of the copyright holder nor the names of its
contributors may be used to endorse or promote products derived from this
software without specific prior written permission.

THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS"
AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE
IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE
ARE DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE
LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR
CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF
SUBSTITUTE GOODS OR SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS
INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN
CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE)
ARISING IN ANY WAY OUT OF THE USE OF THIS SOFTWARE, EVEN IF ADVISED OF THE
POSSIBILITY OF SUCH DAMAGE.



## Touchstone codec

The codec in `crates/model-touchstone` is distributed under its own
BSD-3-Clause license. See [the component license](crates/model-touchstone/LICENSE).

## External tools and research inputs

The optional ngspice reference simulator and XSPICE build tools are external
dependencies, not bundled simulator source or executables. The ngspice
distribution has component-specific licenses; consult its COPYING and file
notices when distributing those tools or their compiled output. The
experimental code-model sources here are project-authored implementations.

The test suite uses independently specified circuits and numerical fixtures.
Measured values obtained by running those circuits in external simulators
are experimental observations, not redistributed simulator code. Vendor model
libraries and model configurations without established redistribution rights
must be supplied externally. Their availability for download is not a license
to redistribute them. The catalog tools accept explicit input paths.

Review the tracked release tree and its history before publication. Removing
a fixture from the current tree does not remove its historical versions.

## Experimental XSPICE interface specifications

The reference interfaces in `runtime/xspice/fixtures/family/georgiatech-*-original.ifs`
are public-domain XSPICE files by Georgia Tech Research Corporation; the
file headers retain their individual authors and dates. The
`tim-edwards-d_genlut-original.ifs` interface is public-domain work by
Tim Edwards (2016). The four `original-xtraevt-*.ifs` files are unmodified
ngspice 47 interface specifications from `src/xspice/icm/xtraevt`
(`d_to_real`, `real_delay`, `real_gain`, `real_to_v`); the modified BSD
notice above applies to these files. Only these interface specifications
are redistributed, not the simulator's C implementations. The native
test wrappers are independently authored circuits.

The ALB regression decks are independently constructed test stimuli;
the accompanying numerical CSV/DAT records and hash manifests record
observations from reference simulators. The original simulator and vendor
model-library distributions are not part of this repository.

## Bipolar compact model kernels

The VBIC 1.1.5 equations in `crates/model-kernel-bipolar` follow the public
VBIC pseudocode and the ngspice 47 VBIC implementation (copyright 1990 Regents
of the University of California; Thomas L. Quarles, 1985; model author Colin
McAndrew, Motorola, 1995; Spice3 implementation Dietmar Warning, 2003).
HICUM/L2 2.4.0 follows the public Verilog-A reference and the ngspice HICUM2
implementation (copyright 1993–2024 Michael Schroter; Spice3 implementation
2019–2020 Dietmar Warning, Markus Müller, Mario Krattenmacher). The upstream
ngspice implementations carry the modified BSD terms reproduced above.
The crate's README records the specific equation references and independently
generated ngspice numerical witnesses.

## HiSIM-HV1/HV2 intrinsic compact-model preparations

`crates/model-compact-intrinsic` derives HiSIM-HV1/HiSIM-HV2 source-order
setup rules, binning, geometry, and physical subkernel equations from the
Hiroshima University/STARC implementations in ngspice 47. Copyright 2012
Hiroshima University & STARC (HV1) and 2014 Hiroshima University & STARC
(HV2). The upstream HiSIM_HV Distribution Statement and Copyright Notice,
including attribution, redistribution and charging conditions, and the
original-code MIT license are reproduced in
[`crates/model-compact-intrinsic/LICENSE`](crates/model-compact-intrinsic/LICENSE).
The top-level MIT license does not supersede these terms.

## Legacy bulk BSIM1/2 preparation

The legacy BSIM1 and BSIM2 model and instance setup in
`crates/model-compact-intrinsic/src/legacy_bsim` and `legacy_bsim.rs`
implements the physical preparation rules of ngspice 47
(`bsim1/b1set.c`, `bsim1/b1temp.c`, `bsim2/b2set.c`, `bsim2/b2temp.c`)
at revision `fa6c2a236507fd440c671de237cad4f49a4e6b68`.
Copyright 1990 Regents of the University of California. All rights reserved.
Authors: Hong J. Park, Thomas L. Quarles and Min-Chie Jeng.
The modified BSD notice reproduced above also applies to these preparations;
the package LICENSE reproduces the terms for standalone distribution.

The BSIM1 constitutive, junction, overlap and noise implementation in
`model-compact-intrinsic/src/legacy_bsim/bsim1.rs` and `model-kernel-bsim`
is based on ngspice 47 `bsim1/b1eval.c`, `b1ld.c`, and `b1noi.c`,
Copyright 1990 Regents of the University of California; Hong J. Park and
Thomas L. Quarles. See the respective crate licenses.


The BSIM2 constitutive implementation is based on ngspice 47
`bsim2/b2eval.c`, and the shared legacy BSIM kernel assembly also follows
`bsim2/b2ld.c` and `b2noi.c`. Copyright 1990 Regents of the University of
California; Min-Chie Jeng, Hong J. Park and Thomas L. Quarles.

## BSIMSOI 4.4 intrinsic compact-model preparations

Source-derived model defaults, ordered instance setup, effective geometry,
size binning, material/thermal and body-junction laws in
`crates/model-compact-intrinsic` follow ngspice revision
`972dbf45db1e47d18bc4fda5d8bb829aff2d0e05`, files `bsimsoi/b4soiset.c`,
`b4soipar.c`, `b4soimpar.c`, `b4soi.c`, `b4soitemp.c`, and `b4soild.c`. Copyright 2010
Regents of the University of California. The upstream author notice and
modified BSD terms appear in the package LICENSE.
