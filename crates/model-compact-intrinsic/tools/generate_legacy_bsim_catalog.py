"""Emit Rust input-catalog facts from a checked-out ngspice source tree.

Run with uv run --no-project python generate_legacy_bsim_catalog.py SOURCE_ROOT.
No model equations, defaults or documentation strings are copied.
"""
import argparse
from pathlib import Path
import re

VARIANTS = [
    ("bsim1", "b1", "BSIM1"),
    ("bsim2", "b2", "BSIM2"),
    ("bsim3v0", "b3v0", "BSIM3_V0"),
    ("bsim3v1", "b3v1", "BSIM3_V1"),
    ("bsim3v32", "b3v32", "BSIM3_V32"),
    ("bsim3", "b3", "BSIM3_V33"),
]
TYPES = {
    "IF_REAL": "Real", "IF_FLAG": "Flag", "IF_INTEGER": "Integer",
    "IF_STRING": "Text", "IF_REALVEC": "RealVector", "IF_VECTOR": "RealVector",
}


def generate(root: Path) -> str:
    lines = [
        "//! Source interface facts for the six legacy Berkeley bulk-MOS implementations.",
        "//! Derived from the upstream IFparm tables, preserving alias identity.",
        "//! Output-only rows are excluded from the input schema; no equations/defaults are copied.",
        "",
    ]
    for directory, stem, variant in VARIANTS:
        source = (root / directory / (stem + ".c")).read_text()
        source = re.sub(r"/\*.*?\*/|//[^\n]*", "", source, flags=re.S)
        tables = re.findall(r"IFparm\s+(\w+)\[\]\s*=\s*\{(.*?)\};", source, re.S)
        if len(tables) != 2:
            raise ValueError(f"{directory}: expected exactly two IFparm tables")
        scopes = set()
        for name, body in tables:
            rows = re.findall(r'\b(\w+)\(\s*"([^"]+)"\s*,\s*(\w+)\s*,\s*([^,]+),', body)
            # Reject any unrecognized row rather than silently truncating the catalog.
            if not rows or len(rows) != len(re.findall(r"\b\w+\s*\(", re.sub(r'"(?:[^"\\]|\\.)*"', '""', body))):
                raise ValueError(f"{directory}/{name}: unparsed table row")
            scope = "MODEL" if name.endswith("mPTable") else "INSTANCE"
            if scope in scopes:
                raise ValueError(f"{directory}: duplicate scope {scope}")
            scopes.add(scope)
            slots, entries, kinds = {}, {}, {}
            for access, keyword, ident, ty in rows:
                if access not in ("IOP", "IOPA", "IOPR", "IP", "OP"):
                    raise ValueError(f"{name}: unknown access {access}")
                if access == "OP":
                    continue
                keyword, ty = keyword.lower(), ty.strip()
                if ty == "IF_VECTOR" and keyword != "ic":
                    raise ValueError(f"{name}: unknown vector semantics {keyword}")
                kind = TYPES[ty]
                slot = slots.setdefault(ident, len(slots))
                if kinds.setdefault(ident, kind) != kind or keyword in entries:
                    raise ValueError(f"{name}: inconsistent alias or duplicate {keyword}")
                entries[keyword] = (slot, kind)
            lines.append(f"// {directory}/{stem}.c: {name}.")
            lines.append(f"pub const {variant}_{scope}: &[crate::ParameterSpec] = &[")
            lines.extend(f'    spec!("{key}", {slot}, {kind}),' for key, (slot, kind) in sorted(entries.items()))
            lines.extend(["];", ""])
    return "\n".join(lines)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source_root", type=Path)
    args = parser.parse_args()
    print(generate(args.source_root / "src/spicelib/devices"), end="")
