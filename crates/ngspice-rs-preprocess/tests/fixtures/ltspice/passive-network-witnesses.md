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

## Signed occurrence coefficients and independent startup laws

For a 2 V DC / 1 V AC source feeding a 10 ohm series resistor and one
network to ground, LTspice accepts signed series and parallel storage
and loss coefficients. At 10 kHz:

| Device | Terminal voltage |
|---|---|
| C=2u Rser=-2 | 0.3716902792754108-j0.6249912365375416 |
| C=2u Rpar=-20 | 0.2733529969952181-j0.6870110137575443 |
| C=2u Cpar=-1u | 0.7169568003248977-j0.4504772433683886 |
| C=-2u | 0.3877266367391514+j0.4872316614323186 |
| L=3m Rpar=-20 | 1.977733752088942+j0.2098439503836435 |
| L=-3m Rser=0 | 0.9971934217275060-j0.0529027540955512 |
| C=2u m=-2 Rser=0 Rpar=0 | 0.1366764984976091+j0.3435055068787721 |
| L=3m m=-2 Rser=0 Rpar=0 | 0.9888674202791085-j0.1049220919999672 |

Omitted leakage depends on the sign of *effective* storage. C=1e9 with
m=-2 gives 2 V DC, whereas C=-1e9 with m=-2 gives 1.96078431372549 V.
L=1e-10 with Rser=5 and m=-2 gives -2/3 V DC; L=-1e-10 with the same
resistance and multiplicity gives -5/7 V. Zero copies give zero leakage.
An explicitly zero L with nonzero copies and omitted Rpar has an undefined
leakage law; explicit Rpar=0 permits the ordinary finite series law.
With Lser nonzero, omitted Cpar instead follows positive *unscaled* main
capacitance times 1e-6; explicit zero, small signed and negative values
are not replaced.

For C=2u IC=.75, Rser=-2 and Rser=0 both give OP terminal voltage
0.7501249875012499 V. The physical negative Rser is unchanged for the
dynamic analysis: only the startup bias conductance selects 1000 S
when Rser is nonpositive. Positive Rser uses its actual unscaled
reciprocal, including Rser below 1m. L=3m IC=.1 Rpar=-20 with omitted
Rser gives 2 V OP and zero external current, not zero winding current.

Thermal noise is a separate law: Rpar=-20 uses absolute conductance,
whereas Rser=-2 or RLshunt=-20 produces undefined (NaN) reference
noise density. Negative voltage-source Rser has a finite absolute
thermal magnitude as described above. These signed native requirements
are tracked separately from source normalization in solver #278.

## Current-reference identity

For C=2u Rser=5 Rpar=20 on a 2 V source through 3 ohms, the operating
point terminal current and a behavioral monitor I(C1) both equal
0.08695652173913043 A. The monitor has zero small-signal current
perturbation, despite a nonzero physical port current. Current branch
identity is separate from the sampling law: the capacitor uses its
physical terminal branch, while an inductor uses the intrinsic winding.
At 10 kHz, L=3m Rser=5 Rpar=20 Cpar=1u m=2 driven by AC 1 through
3 ohms has winding current -0.001818572457001496-j0.007668199776010198 A
and reported current 0.1657616466885765+j0.04446561363256242 A.
Do not infer either branch from the output reporting law.

## Contextual storage temperature

At 27 °C nominal, a capacitor with tc=.01 doubles its main storage at
127 °C. Higher-order tc list entries are accepted; the isolated sixth
coefficient 1e-12 at delta-T=100 produces a factor of two. Comma- and
whitespace-separated symbolic entries are accepted. Global .options tnom
changes the reference temperature, and local temp overrides ambient.
For a 1 V AC source through 10 ohms, C=2u Rser=5 at 10 kHz has terminal
voltage 0.4797550849386705-j0.2759984995014461 at 27 °C and
0.3771577201505801-j0.1652140460077680 at 127 °C.

An inductor instead accepts tc1/tc2 and local tnom (default 27 °C,
independent of global tnom). Temperature changes the self inductance,
not the nominal mutual-coupling base. With L1=3m tc1=.01 at 127 °C,
L2=12m temp=27, K=.5, the winding mutual inductance is exactly 3m,
although L1 self inductance is 6m.

Only primary storage changes with the temperature polynomial. Explicit
parasitics are unchanged; default capacitor Cpar is calculated from
unscaled nominal C. Omitted capacitor leakage follows positive effective
temperature-adjusted capacitance, while omitted inductor leakage follows
the pre-temperature L and copy sign. Signed and zero effective values
must not be clamped. Full numeric witnesses and native requirements:
solver issue #284.

Noise from C/L parasitics follows local temp, or ambient if omitted.
The voltage-source temp setter is diagnosed as unknown in the reference;
voltage-source parasitic noise follows ambient. Do not translate this
invalid source setter to a valid native local noise-temperature field.

## Behavioral current selection across families

At 10 kHz a resistor divider's physical current of 0.125 A produces
zero behavioral I(R) AC perturbation. The same zero-tangent law applies
to diode, capacitor, current-source, F/G and current-output B reads.
Voltage-source, E/H, voltage-output B and inductor winding references
retain instantaneous AC behavior. For voltage-output B with
V=2*V(s), V=V(s)*V(s), or V=sin(V(s)), driving 3 ohms from a control
bias of 2 V and unit AC excitation, the behavioral current monitors are
respectively -2/3, -4/3 and +0.1387156121823808 A. Thus classification
does not depend on linearity of the expression. Linear F/H source
controls accept a true winding or voltage-output branch, not R/C/I.
Behavioral read policy must be selected by the actual bound occurrence,
after macro expansion, rather than a source-text prefix heuristic.

The TC vector accepts optional enclosing parentheses, comma/whitespace
separators (including repeated/trailing commas), braced scalar expressions
and an empty reset. Repeated TC setters replace the entire previous
polynomial, not just its prefix. Independent tests at delta-T=100 °C
confirmed isolated eighth-order coefficient 1e-16 produces a factor of
two. The explicit physical projection is `pn_storage_tc={a1} {a2} ...`,
with implicit constant 1 and occurrence-bound entries. For L, optional
`pn_storage_tnom` selects nominal temperature (27 °C by default in the
source) and `pn_coupling_nominal=1` selects the pre-temperature coupling
base. For temperature-dependent C with omitted Rpar, the explicit
`pn_gterminal_positive_storage_dc` and `_tran` coefficients multiply
positive effective aggregate primary capacitance; ordinary per-copy
constant damping must not be added as well. These new native setters are
the contract proposed in solver #284, not yet an end-to-end acceptance
claim.

## Noiseless storage networks

Bare `noiseless` on C or L disables every physical parasitic thermal
generator, without altering any electrical stamp. In the 127 °C noise
witnesses above, adding this flag produces exactly zero input and output
noise with the external load also noiseless. With C=2u Rser=5 Rpar=20
Lser=3m RLshunt=20 Cpar=1u, gain remains 0.4963351202196508 at 10 kHz;
L=3m Rser=5 Rpar=20 Cpar=1u gives 0.6219468798426032. Source
`noiseless=0` or `=1` and `noisy` are diagnosed by LTspice. Voltage
source `noiseless` is likewise diagnosed. The explicit native
`pn_noiseless=1` selector belongs to the loss/noise contract in solver #278.
