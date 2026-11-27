"""Generate clamped BSIMSOI full-terminal DC and AC reference using ngspice 47."""
from itertools import product
from pathlib import Path
import subprocess
import math
import tempfile

CARD = ("tsi=3e-8 tbox=1e-7 nch=1.7e17 nsub=6e16 ngate=1e20 tox=3e-9 "
        "k1=.53 k2=-.02 vfb=-.8 rth0=.005 cth0=1e-6 rbody=1e-5 rhalo=1000 "
        "rbsh=120 rbdb=300 rbsb=500 isdif=10 iddif=20 rsh=0 "
        "rgatemod=0 rbodymod=0 rdsmod=0 igcmod=0 igbmod=0")
NODES = ("d", "g", "s", "e", "p", "b", "t")
FREQUENCY = 1e6


def native(pmos, soi, cap, heat, reverse, excite, perturb=None):
    sign = -1 if pmos else 1
    x = [sign * val for val in (-.45 if reverse else .65, 1.25, 0, .12, -.65, -.67)]
    x.append(25 if heat else 0)
    if perturb is not None:
        axis, step = perturb
        x[axis] += step
    vsrc = "\n".join(f"V{node} {node} 0 {value:.17g} ac {int(index == excite)}"
                     for index, (node, value) in enumerate(zip(NODES, x)))
    currents = " ".join(f"i(v{node})" for node in NODES)
    polarity = "pmos" if pmos else "nmos"
    deck = f"""BSIMSOI complete clamped terminal reference
{vsrc}
M1 d g s e p b t soi tnodeout l=.5u w=4u nf=2 nseg=2 m=2.3 pd=4u ps=4u nrb=1.2 nrd=0 nrs=0
.model soi {polarity} level=10 soimod={soi} fdmod=1 shmod={heat} capmod={cap} vth0={sign*.55}
+ {CARD}
.temp 27
.options gmin=1e-18 reltol=1e-10 abstol=1e-18 vntol=1e-12
.control
set numdgt=17
set wr_singlescale
op
wrdata op.dat {currents}
ac lin 1 {FREQUENCY} {FREQUENCY}
wrdata ac.dat {currents}
quit
.endc
.end
"""
    if perturb is not None:
        deck = deck.replace(f"wrdata op.dat {currents}\nac lin 1 {FREQUENCY} {FREQUENCY}\nwrdata ac.dat {currents}",
                            "wrdata charge.dat @m1[qb] @m1[qd] @m1[qs]")
    with tempfile.TemporaryDirectory(prefix="bsimsoi-oracle-") as tmp:
        directory = Path(tmp)
        (directory / "test.cir").write_text(deck)
        result = subprocess.run(["ngspice", "-b", "-n", "test.cir"], cwd=directory,
                                text=True, capture_output=True, check=True)
        try:
            if perturb is not None:
                b, d, s = [float(v) for v in (directory / "charge.dat").read_text().split()][1:]
                if reverse:
                    d, s = s, d
                return tuple(sign * 2.3 * q for q in (d, s, b))
            op = [float(v) for v in (directory / "op.dat").read_text().split()][1:]
            ac = [float(v) for v in (directory / "ac.dat").read_text().split()][1:]
        except (OSError, ValueError) as exc:
            raise RuntimeError(result.stdout + result.stderr) from exc
        if len(op) != 7 or len(ac) != 14:
            raise ValueError((pmos, soi, cap, heat, reverse, excite, op, ac))
        return x, op, ac


def intrinsic_gradient(case, axis, raw):
    """Independent central difference of native stored intrinsic charges.

    QB/QD/QS are pre-overlap, pre-junction, pre-M and normalized to mode.
    QG has no working ask accessor; recover from conservation using QE.
    The QE/B native AC derivative isolates the buried oxide capacitance;
    QE/T is likewise free of extrinsic contributions. Ideal FD has no QE.
    """
    step = 1e-3 if axis == 6 else 1e-5
    plus = native(*case, 0, (axis, step))
    minus = native(*case, 0, (axis, -step))
    d, s, b = ((hi - lo) / (2 * step) for hi, lo in zip(plus, minus))
    e = 0.0 if case[1] == 2 else -raw[6 if axis == 6 else 5][7] / (2 * math.pi * FREQUENCY)
    if axis == 3:
        e = -e
    return d, -(d + s + b + e), s, e, b


def main():
    out = Path(__file__).resolve().parent.parent / "tests/fixtures/bsimsoi-clamped-ngspice47.txt"
    gradient_out = out.with_name("bsimsoi-intrinsic-charge-gradients-ngspice47.txt")
    with out.open("w") as f, gradient_out.open("w") as g:
        g.write("# ngspice 47: pmos soi cap heat reverse axis, intrinsic dQD,dQG,dQS,dQE,dQB; central difference 10uV or 1mK. M included.\n")
        f.write("# ngspice 47: pmos soi cap heat reverse excite, 7 native coordinates (T rise), 7 supply DC currents, 7 complex supply AC currents (re im). Frequency 1 MHz.\n")
        f.write("# model-card " + CARD + "\n")
        for case in product(range(2), range(3), (2, 3), range(2), range(2)):
            raw = []
            for excite in range(7):
                x, op, ac = native(*case, excite)
                f.write(" ".join(map(str, (*case, excite, *x, *op, *ac))) + "\n")
                raw.append(ac)
            for axis in (3, 5, 6):
                if axis != 6 or case[3]:
                    gradient = intrinsic_gradient(case, axis, raw)
                    g.write(" ".join(map(str, (*case, axis, *gradient))) + "\n")
    print(out)


if __name__ == "__main__":
    main()
