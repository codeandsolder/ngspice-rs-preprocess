#!/usr/bin/env bash
# Build against the exact configured reference simulator ABI.
set -euo pipefail
if (( $# < 2 || $# > 3 )); then
    printf 'Usage: %s NGSPICE_47_BUILD_TREE OUTPUT_DIRECTORY [MANAGED_COMPILER_WRAPPER]\n' "$0" >&2
    exit 2
fi
source_dir="$(cd -- "$(dirname -- "$0")" && pwd)"
ngspice_tree="$(realpath -- "$1")"
output_dir="$(realpath -m -- "$2")"
icm="$ngspice_tree/src/xspice/icm"
cmpp="$ngspice_tree/src/xspice/cmpp/cmpp"
wrapper=$(realpath -- "${3:-/usr/local/libexec/sccache-client}")
if [[ ! -f "$icm/makedefs" || ! -x "$cmpp" || ! -x "$wrapper" ]]; then
    echo "Configured ngspice 47 tree, built cmpp and managed compiler wrapper required" >&2
    exit 1
fi
stage="$(mktemp -d "$icm/preprocess_logic_XXXXXXXX")"
trap 'rm -rf -- "$stage"' EXIT
module="$(basename -- "$stage")"
cp -R -- "$source_dir"/{modpath.lst,udnpath.lst,logic_event,logic_voltage} "$stage/"
make -C "$icm" -j1 CC="$wrapper gcc" cm="$module" "$module/$module.cm"
mkdir -p -- "$output_dir"
cp -- "$stage/$module.cm" "$output_dir/ngspice_preprocess_logic.cm"
printf 'Built %s\n' "$output_dir/ngspice_preprocess_logic.cm"
