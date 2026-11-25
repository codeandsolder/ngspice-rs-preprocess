#!/usr/bin/env bash
set -euo pipefail

if (( $# != 2 )); then
    printf 'Usage: %s NGSPICE_47_BUILD_TREE OUTPUT_DIRECTORY\n' "$0" >&2
    exit 2
fi
source_dir="$(cd -- "$(dirname -- "$0")" && pwd)"
ngspice_tree="$(realpath -- "$1")"
output_dir="$(realpath -m -- "$2")"
icm="$ngspice_tree/src/xspice/icm"
cmpp="$ngspice_tree/src/xspice/cmpp/cmpp"
sccache_wrapper=/usr/local/libexec/sccache-client
if [[ -v SCCACHE_WRAPPER ]]; then sccache_wrapper="$SCCACHE_WRAPPER"; fi

if [[ ! -f "$icm/makedefs" || ! -x "$cmpp" ]]; then
    echo "Expected configured ngspice 47 tree with built cmpp: $ngspice_tree" >&2
    exit 1
fi
if [[ ! -x "$sccache_wrapper" ]]; then
    echo "Required existing sccache wrapper missing: $sccache_wrapper" >&2
    exit 1
fi
command -v gcc >/dev/null || { echo "gcc is required" >&2; exit 1; }

# The native ngspice ICM makefile expects its module below the ICM build tree.
# Stage copies, compile via sccache, and remove the staging tree on every exit.
stage="$(mktemp -d "$icm/ngspice_preprocess_alb_XXXXXXXX")"
cleanup() { rm -rf -- "$stage"; }
trap cleanup EXIT
module="$(basename -- "$stage")"
cp -R -- "$source_dir/common" "$stage/common"
for name in alb_probe bundle_driver bundle_load family_adc family_dac lut_bit; do
    cp -R -- "$source_dir/$name" "$stage/$name"
done
printf 'alb_probe\nbundle_driver\nbundle_load\nfamily_adc\nfamily_dac\nlut_bit\n' > "$stage/modpath.lst"
: > "$stage/udnpath.lst"
make -C "$icm" -j1 CC="$sccache_wrapper gcc" cm="$module" "$module/$module.cm"
mkdir -p -- "$output_dir"
cp -- "$stage/$module.cm" "$output_dir/ngspice_preprocess_alb.cm"
printf 'Built %s (ngspice 47, existing sccache wrapper)\n' "$output_dir/ngspice_preprocess_alb.cm"
