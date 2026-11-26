#!/usr/bin/env bash
set -euo pipefail
# Existing, configured and compiled ngspice 47 source and object tree.
base=$(realpath "${1:?path to pinned-c directory containing src and obj}")
here=$(cd "$(dirname "$0")" && pwd)
# Keep the C compiler products and native diagnostic files outside the repository.
work=$(mktemp -d "${2:?existing temporary build directory}/bsim3soi-oracle.XXXXXXXX")
trap 'rm -rf "$work"' EXIT
cd "$work"
uv run --no-project python "$here/make_oracle.py"
for f in fd dd pd; do
    obj="$base/obj/src/spicelib/devices/bsim3soi_$f"
    src="$base/src/src/spicelib/devices/bsim3soi_$f"
    /usr/local/libexec/sccache-client cc -I"$base/obj/src/include" -I"$base/src/src/include" -I"$src" -c "$f.c" -o "$f.o"
    cc "$f.o" "$obj/b3soi$f.o" "$obj/b3soi${f}set.o" "$obj/b3soi${f}temp.o" "$obj/b3soi${f}check.o" "$obj/b3soi${f}mpar.o" "$obj/b3soi${f}par.o" -lm -o "$f"
done
uv run --no-project python "$here/cases.py"
cp thermal.tsv "$here/thermal.tsv"
