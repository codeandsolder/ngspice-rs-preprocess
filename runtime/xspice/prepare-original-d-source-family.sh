#!/usr/bin/env bash
# Sourced by original-model d_source physical suites. Only fixture
# preparation is shared: family-specific original IFS, event, electrical,
# and mutation oracles remain in their respective smoke scripts.
# shellcheck source=runtime/xspice/prepare-original-smoke.sh
source "$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)/prepare-original-smoke.sh"

prepare_original_d_source_family() {
    if (( $# != 5 )); then
        printf 'usage: prepare_original_d_source_family FAMILY SOURCE_FILE CODEMODEL_CM RUST_BIN_DIR SCRATCH_ROOT\n' >&2
        return 2
    fi
    local family="$1" source_file="$2"
    prepare_original_smoke "original_source" "$3" "$4" "$5"
    stage_original_family_circuit "$family"
    test -s "$fixtures/$source_file" || {
        printf 'Missing original source fixture: %s\n' "$source_file" >&2
        return 1
    }
    cp -- "$fixtures/$source_file" "$work/stock/"
    cp -- "$fixtures/$source_file" "$work/native/"
    cp -- "$fixtures/georgiatech-d_source-original.ifs" "$work/native/"
    local model
    for model in d_to_real real_gain real_to_v; do
        cp -- "$fixtures/original-xtraevt-$model.ifs" "$work/native/"
    done
}
