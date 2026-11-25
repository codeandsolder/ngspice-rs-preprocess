#!/usr/bin/env bash
set -euo pipefail
if (( $# != 3 )); then
    printf 'usage: %s CODEMODEL_CM RUST_BIN_DIR SCRATCH_ROOT\n' "$0" >&2
    exit 2
fi
here="$(cd -- "$(dirname -- "$0")" && pwd)"
cm="$(realpath -- "$1")"
bins="$(realpath -- "$2")"
scratch="$(realpath -- "$3")"
for tool in ngspice rg awk; do command -v "$tool" >/dev/null; done
for tool in alb-logical-expand alb-family-autowire; do
    test -x "$bins/$tool" || { echo "Missing $tool" >&2; exit 1; }
done
work="$(mktemp -d "$scratch/ngspice_preprocess_recursive_XXXXXXXX")"
trap 'rm -rf -- "$work"' EXIT
for variant in root lexical fanout; do
    dir="$work/$variant"
    mkdir -p "$dir"
    "$bins/alb-logical-expand" \
        "$here/fixtures/family/occurrence-nested-$variant.cir" \
        "$here/fixtures/family/occurrence-nested-$variant.json" \
        "$dir/lowered.cir" "$dir/generated.inc"
    "$bins/alb-family-autowire" \
        "$dir/lowered.cir" "$dir/generated.inc" \
        "$here/fixtures/family/occurrence-nested-$variant.json" \
        "$here/fixtures/family/auto-models.json" "$dir/circuit.cir"
    [[ $(rg -c '^\.subckt INNER_SRV' "$dir/circuit.cir") == 2 ]] ||
        { echo "Expected two native INNER variants: $variant" >&2; exit 1; }
    if [[ $variant == fanout ]]; then
        rg -q '^Xleft INPUT_A OUTPUT_A INNER_SRV0$' "$dir/circuit.cir"
        rg -q '^Xright INPUT_B OUTPUT_B INNER_SRV1$' "$dir/circuit.cir"
        [[ $(rg -c '^\.subckt OUTER_SRV' "$dir/circuit.cir") == 1 ]] ||
            { echo 'Single parent must specialize once for two different children' >&2; exit 1; }
    else
        rg -q '^Xone IN_A OUT_A OUTER_SRV0$' "$dir/circuit.cir"
        rg -q '^Xtwo IN_B OUT_B OUTER_SRV1$' "$dir/circuit.cir"
        rg -q '^Xthree IN_C OUT_C OUTER_SRV0$' "$dir/circuit.cir"
        [[ $(rg -c '^\.subckt OUTER_SRV' "$dir/circuit.cir") == 2 ]] ||
            { echo 'Expected two distinct parent variants' >&2; exit 1; }
    fi
    if [[ $variant == lexical ]]; then
        [[ $(rg -c '^\.param RMON=100meg' "$dir/circuit.cir") == 2 ]] ||
            { echo 'Cloning lost lexical parameter declarations' >&2; exit 1; }
    fi
    printf 'codemodel %s\n' "$cm" >"$dir/.spiceinit"
    (
        cd "$dir"
        ngspice -b circuit.cir >sim.log 2>&1 || { tail -n 95 sim.log >&2; exit 1; }
        if rg -i 'fatal error|timestep too small|error:|no such model' sim.log; then
            echo "Native recursive simulation failed: $variant" >&2; exit 1
        fi
        test -s result.dat
    )
    if [[ $variant == fanout ]]; then
        awk '$1>10e-9 && $1<11e-9 && $2>3.1 && $2<3.31 && $4>=-.01 && $4<.1 {ok=1}
             END {exit !ok}' "$dir/result.dat" ||
            { echo 'Two differently supplied inner calls in one parent failed' >&2; exit 1; }
    else
        awk '$1>10e-9 && $1<11e-9 && $2>3.1 && $2<3.31 &&
             $4>=-.01 && $4<.1 && $6>3.1 && $6<3.31 {ok=1}
             END {exit !ok}' "$dir/result.dat" ||
            { echo 'Nested HIGH/LOW/HIGH electrical specialization failed' >&2; exit 1; }
    fi
done
echo '3/3 recursive per-occurrence native ngspice 47 circuits passed'
