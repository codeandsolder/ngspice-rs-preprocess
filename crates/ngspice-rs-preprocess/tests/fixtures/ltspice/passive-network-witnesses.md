# LTspice 24.0.12 independent passive network observations

All circuits below are synthetic. Unless specified, the reference used 27 °C,
the normal solver, modified trapezoidal integration, and uncompressed ASCII
raw output. The native physical network contract is documented in the solver's
`docs/port/PASSIVE_MODELS.md`. These measurements distinguish source semantics
from native coefficients; they do not establish parity for arbitrary decks.

## Omitted parallel resistance

With a 2 V source through 1 megohm, the observed operating-point terminal
voltages for C=1meg are:

| Setters | V |
|---|---:|
| omitted Rpar | 1 |
| Rser=1meg, omitted Rpar | 1 |
| Rpar=1meg | 1 |
| Rpar=0 | 2 |
| Rser=1meg Rpar=1meg | 1 |
| m=3, omitted Rpar | 0.5 |

The default is terminal conductance C*1e-12 per copy, not conductance
across the inner storage element. Under a constant forced 1 V, at t=1e8 s
with C=1meg the measured terminal current is 1.000000001027956e-6 A
(omitted), 1e-6 A with Rser=1meg, zero with Rpar=0, and 1e-6 A with
Rpar=1meg. AC does not include the omitted default.

For the following circuit, operating-point and steady-transient results
are the same:

```spice
Inductor default damping
.options plotwinsize=0
V1 s 0 2 AC 1
R1 s a 10
L1 a 0 1p Rser=5
R2 s b 10
L2 b 0 1p Rser=5 Rpar=0
R3 s c 10
L3 c 0 1p Rser=5 Rpar=20
R4 s d 10
L4 d 0 1p Rser=5 m=3
.op
.end
```

| Node | V |
|---|---:|
| a | 0.1538461538461539 |
| b | 0.6666666666666666 |
| c | 0.5714285714285714 |
| d | 0.05405405405405405 |

Adding K1 L1 L3 .5 leaves these DC results unchanged. Replacing .op with
.ac lin 2 1e11 1e12, V(a) and V(b) agree at 1e12 Hz:
0.4328459714834264+j0.2375689239255364. Thus omitted Rpar adds
1e-12/L per-copy terminal conductance in DC and transient, not AC.
L cards with Lser or RLshunt report unknown-parameter errors; those
capacitor setters must not be interpreted as inductor topology.

## Signed voltage-source series resistance

```spice
Signed source series resistance
.options plotwinsize=0
V1 a 0 1 AC 1 Rser=-2
R1 a 0 3 noiseless
V2 b 0 1 AC 1 Rser=-2 Cpar=1u
R2 b 0 3 noiseless
.op
.end
```

OP: V(a)=V(b)=3 V, I(V1)=I(V2)=-1 A.
AC at 100 Hz: V(a)=3+j0, V(b)=2.999957363914941+j0.01130957281866933.
At 10 kHz: V(b)=2.626688920383676+j0.9902383938651672.
With .noise V(a) V1 lin 2 100 10000, the output amplitude density at both
frequencies is 5.462321622098061e-10 V/sqrt(Hz); input density is
1.820773874032687e-10 V/sqrt(Hz). Preserve signed impedance and positive
physical noise magnitude separately.

## Elaborated current controls

```spice
Macro-selected source parasitic placement
V1 a 0 1 AC 1 Rser=2 Cpar=1u
V2 b 0 1 AC 1 Rser=2 Cpar=1u
V3 c 0 1 AC 1 Rser=2 Cpar=1u
R1 a 0 3
R2 b 0 3
R3 c 0 3
.func probe(x) {I(x)}
.func direct() {I(V2)}
B1 a1 0 V=probe(V1)
B2 b1 0 V=direct()
.ac lin 1 10k 10k
.end
```

At 10 kHz, V(a)=V(b)=0.602261102728891+j0.02998880634513171,
V(c)=0.5966083459066635-j0.04498320951769758. The source current reference,
including macro binding, changes where the capacitance is physically
placed. Save directives alone do not select the topology. The complete
consumer must resolve this at elaboration per occurrence.

## Startup and observations

Independent startup and zero-multiplicity witnesses are specified in
solver issues #271 and #272. Capacitor behavioral current sampling across
analysis phases is specified in #274, and occurrence-bound voltage source
capacitance placement in #275. These are separate native contracts;
a successful normalization is not sufficient to claim their implementation.
