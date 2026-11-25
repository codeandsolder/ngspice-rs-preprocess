# SIMetrix Elements 9.40d original-source EDGE/LEVEL vendor oracle

Nine self-contained original-source ALB netlists were **actually simulated**
in official SIMetrix/SIMPLIS Elements 9.40d on November 14, 2026. All nine
produced valid numeric .PRINT TRAN waveforms and no vendor model errors.
The source definitions use .FILE/.ENDF, with no proprietary libraries.

These are **vendor measurement fixtures and a digital-language research
baseline**, not native ngspice circuits. The source-only Rust parser accepts
the embedded definitions but XSPICE production lowering of clocked ALBs
remains explicitly disabled. The vendor analog output is shaped by the 5 V
UNIV DAC; its 2.5 V crossing is not a raw digital event timestamp.

## Original probes

- edge-basic.net: rising-edge capture of 1/0 data, 750 ps register delay,
  250 ps output PORT delay; vendor Q midpoint at 11.833592, 21.587206,
  31.832393, 41.607206 ns.
- level-basic.net: transparent HIGH-enable latch; Q midpoint at
  11.833592, 15.719019, 31.832393, 35.709659 ns.
- edge-hold-minclock.net: an approximately 400 ps clock pulse under
  MINCLOCK=600 ps and a D violation within HOLD=900 ps; Q midpoint
  at 26.141820 and 35.902655 ns, no earlier update.
- level-setup.net: register DELAY=2 ns and SETUP=1 ns; a D transition
  500 ps before closing the latch is rejected. Q midpoint at
  13.073592 and 37.959019 ns.
- edge-async-reset.net: asynchronous active-low reset while clock LOW,
  with ASYNCDELAY=200 ps vs clocked DELAY=1 ns; Q midpoint at
  12.073592, 21.059019, 32.091834 ns.

The original SETUP probe had DELAY=750 ps < SETUP=1 ns and triggered a
vendor warning. The corrected 2 ns delay was run independently first;
the final main probe reproduced the independent valid CSV byte-for-byte
with **zero warnings**, then the redundant control was deleted.

## Controlled experiments

The controls subdirectory contains separate, original-source causal
variants. These are also real vendor-run inputs, each with its own .out
and numeric .csv retained as external research evidence.

- edge-minclock-only.net removes HOLD but retains MINCLOCK=600 ps;
  Q transitions at 16.136674 and 35.896376 ns.
- edge-hold-only.net removes MINCLOCK but retains HOLD=900 ps;
  Q transitions at 7.148154 and 35.896376 ns.
- edge-no-qualifiers.net removes both MINCLOCK and HOLD;
  Q transitions at 7.148154 and 35.896376 ns.
- level-setup-no-setup.net removes SETUP while retaining valid DELAY=2 ns;
  Q transitions at 13.073592, 22.359019, 33.092393, 37.969659 ns.

The measured, machine-readable source of the full **25 Q crossings**
is vendor-q-2p5v-events.csv. Its event rows are exactly the real vendor
derived values; only Windows CRLF line endings were normalized to LF
for source control. Original measured oracle CSV SHA256:
F41EF8C4C5E07A3FBE93A79841FA2BA73FBAFA4D8ACBD65C62514A36AB4494FC.
Normalized repository CSV SHA256:
CF5CE515AE3041FAA2DF4F14D5FE4D873F04D7DD33720DE4B33A93B0CD321027.

## Reproduction / evidence

Open Elements Main Window, run each .net through the GUI runner, check
each .out listing for actual model ERROR/WARNING lines, then export
numeric .PRINT tables. The vendor reference CSV uses linear interpolation
between 20 ps printed samples at the 2.5 V output threshold. All netlists
explicitly constrain **internal solver maximum step** with
.tran 20p ... 0 20p, after earlier default-step differences were found.
Never infer digital event timing directly from the analog midpoint without
accounting for register, output PORT and physical DAC rise/fall delays.


Official ALB and embedded-file syntax documentation:
https://help.simetrix.co.uk/9.4/simetrix/simulator_reference/topics/digitalsimulation_alb_udm.htm
https://help.simetrix.co.uk/8.4/simetrix/simulator_reference/topics/commandreference_dot_fileanddot_endf.htm

## Correctness boundary

These cases establish basic rising-edge capture, latch transparency,
MINCLOCK/HOLD/SETUP rejection and asynchronous reset. They do **not**
exhaustively establish simultaneous event priority, bitwise mixed
violations, register arrays, timing parameters from USER/DEVICE, or
larger feedback/state machines. A general native stateful scheduler
should be implemented against the existing StatefulProgram IR, tested
against broader vendor traces, and remain separate from the currently
COMB-only native compiler until complete enough for actual source
semantics. Production D_LOGIC_BLOCK lowering still fails closed.
