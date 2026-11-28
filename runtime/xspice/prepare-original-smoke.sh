#!/usr/bin/env bash
# Shared fixture setup only; source-specific electrical, digital event,
# ABI, and poisoned-model oracles remain independent.
prepare_original_smoke() {
    if (( $# != 4 )); then
        printf 'usage: %s CODEMODEL_CM RUST_BIN_DIR SCRATCH_ROOT\n' "$0" >&2
        return 2
    fi
    local prefix="$1"
    cm="$(realpath -- "$2")"
    bins="$(realpath -- "$3")"
    scratch="$(realpath -- "$4")"
    for cmd in ngspice rg jq awk; do
        command -v "$cmd" >/dev/null || {
            printf 'Missing original smoke dependency: %s\n' "$cmd" >&2
            return 1
        }
    done
    for bin in alb-boundary-discover alb-logical-expand alb-family-autowire; do
        test -x "$bins/$bin" || {
            printf 'Missing original smoke compiler: %s\n' "$bin" >&2
            return 1
        }
    done
    local helper_dir
    helper_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
    fixtures="$helper_dir/fixtures/family"
    work="$(mktemp -d "$scratch/ngspice_preprocess_$prefix"_XXXXXXXX)"
    trap 'rm -rf -- "$work"' EXIT
}

# A single original source circuit with both stock and Rust-backed netlists.
# Source-specific IFS/stimuli are supplied by the caller.
stage_original_family_circuit() {
    if (( $# != 1 )); then
        echo 'usage: stage_original_family_circuit FAMILY' >&2
        return 2
    fi
    local family="$1" file
    config="$fixtures/auto-discovery-original-$family-x.json"
    for file in "$config" "$fixtures/original-$family-reference.cir" \
        "$fixtures/logical-original-$family-x.cir" \
        "$fixtures/native-original-$family.lib"; do
        test -s "$file" || {
            printf 'Missing original family fixture: %s\n' "$file" >&2
            return 1
        }
    done
    mkdir -p "$work/stock" "$work/native"
    cp -- "$fixtures/original-$family-reference.cir" "$work/stock/original.cir"
    cp -- "$fixtures/logical-original-$family-x.cir" "$work/native/original.cir"
    cp -- "$fixtures/native-original-$family.lib" "$work/native/"
    printf 'codemodel %s\n' "$cm" >"$work/stock/.spiceinit"
    cp -- "$work/stock/.spiceinit" "$work/native/"
}
