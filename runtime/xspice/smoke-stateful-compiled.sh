#!/usr/bin/env bash
# Compile original clocked ALB source with its original electrical
# parameters and compare to independent source-identical vendor oracles.
set -euo pipefail
if (( $# != 4 )); then
    printf 'usage: %s CODEMODEL_CM RUST_BIN_DIR SCRATCH_ROOT EXTERNAL_MODEL_JSON\n' "$0" >&2
    exit 2
fi
here="$(cd -- "$(dirname -- "$0")" && pwd)"
cm="$(realpath -- "$1")"
bins="$(realpath -- "$2")"
scratch="$(realpath -- "$3")"
models="$(realpath -e -- "$4")"
work="$(mktemp -d "$scratch/ngspice_preprocess_stateful_compiled_XXXXXXXX")"
trap 'rm -rf -- "$work"' EXIT
printf 'codemodel %s\n' "$cm" > "$work/.spiceinit"
vendor="$here/fixtures/vendor-stateful"
family="$here/fixtures/family"
for name in edge-basic level-basic edge-hold-minclock level-setup edge-async-reset controls/edge-minclock-only controls/edge-hold-only controls/edge-no-qualifiers controls/level-setup-no-setup; do
    awk '
        tolower($1)==".print" { next }
        tolower($1)==".end" { print ".control\nrun\nwrdata native.dat v(q)\nquit\n.endc" }
        { print }
    ' "$vendor/$name.net" > "$work/original.cir"
    "$bins/alb-boundary-discover" "$work/original.cir" "$family/auto-discovery-univ-5v.json" "$work/boundaries.json"
    "$bins/alb-family-compile" "$work/original.cir" "$work/boundaries.json" "$models" "$work/circuit.cir" "$work/generated.inc"
    rg -q '^Asr_stateful \[sr_in0.*\] \[sr_out0\] 0 \[sr_out0_sink\] \[sr_out0_source\] Msr_stateful$' "$work/generated.inc"
    (cd "$work" && ngspice -b circuit.cir > sim.log 2>&1) || {
        tail -n 70 "$work/sim.log" >&2
        exit 1
    }
    if rg -i 'error:|fatal error|timestep too small|no such model' "$work/sim.log"; then
        exit 1
    fi
    awk -v name="$name" -f "$here/check-stateful-edges.awk" "$vendor/vendor-q-2p5v-events.csv" "$work/native.dat"
done
echo '9/9 automatically compiled original EDGE/LEVEL circuits passed vendor transition comparison'
