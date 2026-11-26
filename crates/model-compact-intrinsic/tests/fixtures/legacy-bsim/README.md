# BSIM1/2 preparation reference

These fixtures contain independently authored scalar input assignments and
numeric output from ngspice source revision
`fa6c2a236507fd440c671de237cad4f49a4e6b68`, using the original parameter
setters and temperature routines. The harness under
`tools/legacy_bsim_prepare_oracle.c` does not include or redistribute simulator
implementation source.

Compile from a configured ngspice source tree, passing the configuration-header
directory (the directory containing ngspice/config.h) as the first include
path, the source's src/include, and both legacy BSIM device directories:

```sh
cc -std=gnu11 -O2 -ffp-contract=off \
  -I /path/to/configured/obj/src/include -I /path/to/ngspice/src/include \
  -I /path/to/ngspice/src/spicelib/devices/bsim1 \
  -I /path/to/ngspice/src/spicelib/devices/bsim2 \
  /path/to/legacy_bsim_prepare_oracle.c \
  /path/to/ngspice/src/spicelib/devices/bsim1/{b1.c,b1mpar.c,b1par.c,b1temp.c} \
  /path/to/ngspice/src/spicelib/devices/bsim2/{b2.c,b2mpar.c,b2par.c,b2temp.c} \
  -lm -o legacy_bsim_prepare_oracle
```

Pass family 1 or 2 as the argument and the fixture input lines (excluding
the comment and the separator/output) on standard input. The ordinary,
clamps and wide cases cover different channel dimensions and resistance
topologies; all model real fields are explicitly set to exercise setter
and size-coefficient mapping. They test physical preparation rather
than current, charge or convergence. The separate native-binary oracle
checks defaults. Flags are scalar 0/1 in these reference input records.


The two defaults files are the numeric model rows of ngspice 47 `showmod m1`
for the following independently authored deck (use level 4 and 5):

```spice
* legacy BSIM setup
Vd d 0 1
Vg g 0 1
M1 d g 0 0 xmod l=10u w=20u ad=20p as=40p pd=10u ps=20u nrd=2 nrs=3
.model xmod nmos level=4 tox=.03 vdd=5
.control
set numdgt=15
op
showmod m1
show m1
quit
.endc
.end
```

The two input-only polarity flags are not displayed by showmod; every
displayed model scalar is compared (79 BSIM1 and 135 BSIM2).
