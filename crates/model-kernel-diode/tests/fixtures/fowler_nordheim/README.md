# Fowler–Nordheim independent LTspice witnesses

All vendor measurements use LTspice 24.0.12. The included families.cir
sweeps -10 V to +8 V in 0.1 V steps, 181 samples. Of the 14 model cards,
12 LEVEL=2 variants gave identical conduction currents and two
ideal-family variants (LEVEL=0/3) gave identical ideal currents.
The compact DC table retains two independent columns of each family.

ac.tsv collects 63 independent terminal AC cases at 1 kHz, including
signed geometry, individual model and instance coefficients, zero or
nonrepresentable thickness/series cases, and negative thickness around
its zero differential conductance. Columns: case, terminal voltage,
model thickness/length/width, optional instance length/width, parallel
M, series N, measured terminal OP current if present, real and imaginary
terminal admittance. The AC admittance is taken from the voltage source
current with reversed sign, NOT the device's internal current display
(which reports a different capacitive scaling for N != 1).

noise.tsv records 59 measured noise points at the measured bias:
model/instance coefficients, frequency, impedance magnitude, and
input-referred current amplitude. The constant-current bias is
independent of the code under test. Negative N gives nonfinite noise;
negative M or area alone still produces nonnegative shot PSD.
KF/AF and temperature variations in the original vendor cases were
inert. No thermal or flicker term is added.

transient.cir and transient-witness.tsv are the independent Gear linear
voltage-ramp witness with L=2 W=3 M=2 N=2 Tox=50. The 72 samples exclude
the piecewise-linear slope change boundaries. Compare measured current
to I(V) + C*dV/dt and charge derivative to C.

extreme.tsv is separate analytic evidence, not vendor output: Python
decimal evaluated the law with 100 decimal digits from the exact
binary64 input values and rounded only the final outputs to binary64.
It includes representable currents despite exponential under/overflow,
unrepresentable local voltage or field ratio, overflow in geometry and
a representable terminal charge despite underflowed capacitance.
