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
for bin in alb-logical-expand alb-family-autowire; do
    test -x "$bins/$bin" || { echo "Missing $bin" >&2; exit 1; }
done
work="$(mktemp -d "$scratch/ngspice_preprocess_private_scope_XXXXXXXX")"
trap 'rm -rf -- "$work"' EXIT
for variant in private-3v3 private-5v nested-3v3 shared; do
    dir="$work/$variant"
    mkdir -p "$dir"
    case "$variant" in
        private-3v3) source=scoped-private.cir; config=scoped-private-3v3.json ;;
        private-5v) source=scoped-private.cir; config=scoped-private-5v.json ;;
        nested-3v3) source=scoped-nested.cir; config=scoped-nested-3v3.json ;;
        shared) source=scoped-shared.cir; config=scoped-shared.json ;;
    esac
    "$bins/alb-logical-expand" \
        "$here/fixtures/family/$source" \
        "$here/fixtures/family/$config" \
        "$dir/lowered.cir" "$dir/generated.inc"
    "$bins/alb-family-autowire" \
        "$dir/lowered.cir" "$dir/generated.inc" \
        "$here/fixtures/family/$config" \
        "$here/fixtures/family/auto-models.json" "$dir/circuit.cir"
    [[ $(rg -c '^AsrFamily0' "$dir/circuit.cir") == 1 ]] ||
        { echo "One physical private ADC expected for $variant" >&2; exit 1; }
    if [[ $variant == shared ]]; then
        if rg 'sr_family_dac\(' "$dir/circuit.cir"; then
            echo 'Shared private ADC unexpectedly emitted a DAC' >&2
            exit 1
        fi
        [[ $(rg -c '^XSR_A[12] D_IN D_IN_SR_SINK D_IN_SR_SOURCE' "$dir/circuit.cir") == 2 ]] ||
            { echo 'Two private physical input loads did not share the same digital bundle' >&2; exit 1; }
    else
        [[ $(rg -c '^AsrFamily1' "$dir/circuit.cir") == 1 ]] ||
            { echo "Expected local DAC for $variant" >&2; exit 1; }
    fi
    printf 'codemodel %s\n' "$cm" >"$dir/.spiceinit"
    (
        cd "$dir"
        if ! ngspice -b circuit.cir >sim.log 2>&1; then
            tail -n 100 sim.log >&2
            exit 1
        fi
        if rg -i 'fatal error|timestep too small|error:|no such model' sim.log; then
            echo "Native ngspice $variant failed" >&2
            exit 1
        fi
        [[ -s result.dat ]] || { echo "Missing native waveform: $variant" >&2; exit 1; }
    )
done
for variant in private-3v3 nested-3v3; do
    awk '$1>10e-9 && $1<11e-9 && $2>3.1 && $2<3.31 {yes=1}
         END {exit !yes}' "$work/$variant/result.dat" ||
        { echo "$variant did not achieve scaled HIGH" >&2; exit 1; }
done
awk '$1>10e-9 && $1<11e-9 && $2>=-.01 && $2<.1 {yes=1}
     END {exit !yes}' "$work/private-5v/result.dat" ||
    { echo 'Private 5V nominal bridge incorrectly accepted 1.5V as HIGH' >&2; exit 1; }
awk '$1>4.99e-9 && $1<5.01e-9 && $2>3.04 && $2<3.29 {yes=1}
     END {exit !yes}' "$work/shared/result.dat" ||
    { echo 'Private shared ADC did not reflect 4pF input loading' >&2; exit 1; }
# A subcircuit definition with two callers has the same locally inserted
# bridge but each ngspice instance must have its own physical state.
awk '$1>10e-9 && $1<11e-9 && $2>3.1 && $2<3.31 && $4>3.1 && $4<3.31 {both=1}
     END {exit !both}' "$work/private-3v3/result.dat" ||
    { echo 'Private subcircuit bridge did not independently serve two callers' >&2; exit 1; }
echo '4/4 private-scope original-SIMetrix ngspice native simulations passed'
