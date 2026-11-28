"""Regenerate the independent BSIM1/2 DC witness data with ngspice 47."""

import argparse
import re
import subprocess
import tempfile
from itertools import product
from pathlib import Path

FIXTURES = (
    Path(__file__).resolve().parents[2] / "crates/model-kernel-bsim/tests/fixtures"
)


def current(stdout: str, node: str) -> float:
    """Read an explicitly printed voltage-source current."""
    match = re.search(rf"i\(v{node}\)\s*=\s*([-+0-9.eE]+)", stdout)
    if match is None:
        msg = f"missing current i(v{node}): {stdout}"
        raise ValueError(msg)
    return float(match[1])


def generate(family: str, executable: str) -> str:
    """Run each independent polarity, geometry and bias combination."""
    base = (
        FIXTURES / ("dc-probe.cir" if family == "bsim1" else "dc-probe-bsim2.cir")
    ).read_text(encoding="utf-8")
    rows = ["# ngspice 47 OP; pmos Rsh Vd Vg Vs Vb Ivd Ivg Ivs Ivb (supply signs)"]
    for pmos, rsh, forward, vbs in product([0, 1], [0, 52], [0, 1], [-0.4, 0.04]):
        sign = -1 if pmos else 1
        vd, vs = (1.2, 0.1) if forward else (0.1, 1.2)
        volts = [sign * x for x in (vd, 1.7, vs, 0.1 + vbs)]
        deck = (
            base.replace("nmos(", ("pmos" if pmos else "nmos") + "(")
            .replace("rsh=52", f"rsh={rsh}")
            .replace("display\nprint all", "print i(vd) i(vg) i(vs) i(vb)")
        )
        for name, value in zip("dgsb", volts, strict=True):
            deck = re.sub(
                rf"^v{name} {name} 0 .*$",
                f"v{name} {name} 0 {value:.17g}",
                deck,
                flags=re.MULTILINE,
            )
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "dc-case.cir"
            path.write_text(deck, encoding="utf-8")
            result = subprocess.run(
                [executable, "-b", str(path)],
                capture_output=True,
                text=True,
                check=True,
                cwd=tmp,
            )
        if "ngspice-47 done" not in result.stdout:
            raise ValueError(result.stdout)
        values = [current(result.stdout, node) for node in "dgsb"]
        rows.append(" ".join(str(x) for x in [pmos, rsh, *volts, *values]))
    return "\n".join(rows) + "\n"


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("family", choices=("bsim1", "bsim2"))
    parser.add_argument("--ngspice", default="ngspice")
    args = parser.parse_args()
    print(generate(args.family, args.ngspice), end="")


if __name__ == "__main__":
    main()
