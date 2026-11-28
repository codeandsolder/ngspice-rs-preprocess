#!/usr/bin/env bash
set -euo pipefail
if (( $# != 2 )); then
    printf 'Usage: %s STATEFUL_CODEMODEL_CM SCRATCH_ROOT\n' "$0" >&2
    exit 2
fi
here="$(cd -- "$(dirname -- "$0")" && pwd)"
model="$(realpath -- "$1")"
root="$(realpath -- "$2")"
test -s "$model"
work="$(mktemp -d "$root/stateful-oracle-XXXXXXXX")"
trap 'rm -rf -- "$work"' EXIT
printf 'codemodel %s\n' "$model" > "$work/.spiceinit"
refs="$here/fixtures/vendor-stateful/vendor-q-2p5v-events.csv"
for name in edge-basic level-basic edge-hold-minclock level-setup edge-async-reset controls/edge-minclock-only controls/edge-hold-only controls/edge-no-qualifiers controls/level-setup-no-setup; do
    "$here/run-stateful-vendor-case.sh" "$here/fixtures/vendor-stateful/$name.net" "$work" 20p
    awk -v name="$name" -f "$here/check-stateful-edges.awk" "$refs" "$work/native.dat"
done
echo '9/9 native Rust-XSPICE vendor timing oracle scenarios passed'
