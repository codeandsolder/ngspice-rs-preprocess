#!/usr/bin/env bash
# Opt-in complete native regression suite. No simulator or compiler builds.
set -euo pipefail
if (( $# < 3 )); then
    printf 'Usage: %s STATEFUL_CODEMODEL_CM RUST_BIN_DIR SCRATCH_ROOT [SUITE ...]\n' "$0" >&2
    exit 2
fi
here="$(cd -- "$(dirname -- "$0")" && pwd)"
cm="$(realpath -- "$1")"
bins="$(realpath -- "$2")"
scratch="$(realpath -- "$3")"
shift 3

# Individual suite names make it possible to reproduce a failure without
# replaying the entire numerical corpus. Nested oracles run only once in
# the default suite; their own standalone scripts remain available.
two_arg=(
    smoke smoke-family-adc smoke-family-dac
    smoke-adc-domains-vendor smoke-adc-order-vendor
    smoke-adc-schmitt-order-vendor smoke-dac-domains-vendor
    smoke-dac-rails-vendor smoke-stateful smoke-stateful-array
    smoke-stateful-device-multi smoke-stateful-expressions
    smoke-stateful-extended smoke-stateful-mixed smoke-stateful-parameters
)
three_arg=(
    smoke-exported-family smoke-family-autowire smoke-family-bidirectional
    smoke-family-cross-cut-receivers-vendor smoke-family-dac-ramps
    smoke-family-default-current-limits-vendor smoke-family-dynamic-feedback
    smoke-family-implicit-globals-vendor smoke-family-inline-def-vendor
    smoke-family-multi-driver smoke-family-nested-bidirectional
    smoke-family-occurrence smoke-family-passive-witness-vendor
    smoke-family-recursive smoke-family-signed-domains-vendor
    smoke-family-supply-vendor smoke-family-vendor-univ
    smoke-bidirectional-vendor smoke-hc-dac-vendor smoke-hc-unknown-history
    smoke-logical-alb smoke-logical-alb-auto smoke-overload-request-identity
    smoke-private-family smoke-stateful-compiled
    smoke-stateful-directional-drive smoke-stateful-output-load
    smoke-stateful-timing-defaults smoke-stateful-unknown-history
)
suites=("${two_arg[@]}" "${three_arg[@]}" smoke-generated smoke-family-materialized
        smoke-producer-identity-drivers smoke-promoted-x-formal-drivers)
if (( $# > 0 )); then suites=("$@"); fi

declare -A kind=()
for suite in "${two_arg[@]}"; do kind["$suite"]=two; done
for suite in "${three_arg[@]}"; do kind["$suite"]=three; done
kind[smoke-generated]=generated
kind[smoke-family-materialized]=materialized
kind[smoke-producer-identity-drivers]=multi_driver
kind[smoke-promoted-x-formal-drivers]=multi_driver
for suite in "${suites[@]}"; do
    if [[ ! -v 'kind[$suite]' ]]; then
        printf 'Unknown suite: %s\n' "$suite" >&2
        exit 2
    fi
done
test -f "$cm"
test -d "$bins"
test -d "$scratch"

work="$(mktemp -d "$scratch/ngspice_preprocess_suite_XXXXXXXX")"
trap 'rm -rf -- "$work"' EXIT
fixtures="$here/fixtures"
compile() {
    "$bins/alb-xspice-compile" "$fixtures/$1.alb" "$fixtures/$2.json" "$work/$3.inc" "$4"
}
run_suite() {
    case "${kind[$1]}" in
        two) "$here/$1.sh" "$cm" "$work" ;;
        three) "$here/$1.sh" "$cm" "$bins" "$work" ;;
        multi_driver) "$here/smoke-family-multi-driver.sh" "$cm" "$bins" "$work" "${1#smoke-}" ;;
        generated)
            compile timed-enable timed-enable timed-enable ALB_TIMED &&
                compile constant constant constant ALB_CONSTANT &&
                compile bundle-buffer bundle-buffer bundle-buffer ALB_BUFFER &&
                compile bundle-buffer bundle-buffer-low bundle-buffer-low ALB_BUFFER &&
                "$here/$1.sh" "$cm" "$work" "$work"
            ;;
        materialized)
            compile bundle-buffer family/buffer-40r buffer-family ALB_BUFFER &&
                compile bundle-buffer family/buffer-zero-current buffer-zero ALB_BUFFER &&
                "$here/$1.sh" "$cm" "$bins" "$work/buffer-family.inc" "$work/buffer-zero.inc" "$work"
            ;;
    esac
}
failed=()
for suite in "${suites[@]}"; do
    printf '\n=== %s ===\n' "$suite"
    if run_suite "$suite"; then
        printf 'SUITE PASS %s\n' "$suite"
    else
        printf 'SUITE FAIL %s\n' "$suite" >&2
        failed+=("$suite")
    fi
done
printf '\n%d/%d native regression suites passed\n' "$(("${#suites[@]}" - "${#failed[@]}"))" "${#suites[@]}"
if (( ${#failed[@]} != 0 )); then
    printf 'Failed: %s\n' "${failed[@]}" >&2
    exit 1
fi
