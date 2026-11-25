#!/usr/bin/env bash
set -euo pipefail

if (( $# < 2 || $# > 3 )); then
    printf 'Usage: %s NGSPICE_47_BUILD_TREE OUTPUT_DIRECTORY [RUST_STATICLIB]\n' "$0" >&2
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

# Native ngspice's GNUmakefile reads original code-model sources from
# $(srcdir)/$(cm) and emits objects below the configured build directory.
# The source and build directories differ for an out-of-tree ngspice build.
# Keep only ephemeral module staging in each directory and remove both.
source_icm="$(sed -n 's/^srcdir[[:space:]]*=[[:space:]]*//p' "$icm/makedefs")"
[[ -n "$source_icm" ]] || {
    echo "Configured ngspice ICM tree has no srcdir in makedefs: $icm" >&2
    exit 1
}
source_icm="$(cd -- "$icm" && realpath -m -- "$source_icm")"
[[ -d "$source_icm" ]] || {
    echo "Configured ngspice ICM source directory is missing: $source_icm" >&2
    exit 1
}
stage="$(mktemp -d "$source_icm/ngspice_preprocess_alb_XXXXXXXX")"
module="$(basename -- "$stage")"
built_stage="$icm/$module"
cleanup() {
    rm -rf -- "$stage"
    if [[ "$built_stage" != "$stage" ]]; then
        rm -rf -- "$built_stage"
    fi
}
trap cleanup EXIT
cp -R -- "$source_dir/common" "$stage/common"
rust_archive="${3:-}"
if [[ -n "$rust_archive" ]]; then
    rust_archive="$(realpath -- "$rust_archive")"
    [[ -s "$rust_archive" ]] || { echo "Missing Rust library: $rust_archive" >&2; exit 1; }
fi
for name in alb_probe bundle_driver bundle_load family_adc family_dac lut_bit; do
    cp -R -- "$source_dir/$name" "$stage/$name"
done
printf 'alb_probe\nbundle_driver\nbundle_load\nfamily_adc\nfamily_dac\nlut_bit\n' > "$stage/modpath.lst"
if [[ -n "$rust_archive" ]]; then
    cp -R -- "$source_dir/stateful" "$stage/stateful"
    printf 'stateful\n' >> "$stage/modpath.lst"
fi
: > "$stage/udnpath.lst"
if [[ -n "$rust_archive" ]]; then
    make -C "$icm" -j1 CC="$sccache_wrapper gcc" cm="$module" \
        LIBS="-lm $rust_archive -ldl -lpthread" "$module/$module.cm"
else
    make -C "$icm" -j1 CC="$sccache_wrapper gcc" cm="$module" "$module/$module.cm"
fi
mkdir -p -- "$output_dir"
cp -- "$built_stage/$module.cm" "$output_dir/ngspice_preprocess_alb.cm"
printf 'Built %s (ngspice 47, existing sccache wrapper)\n' "$output_dir/ngspice_preprocess_alb.cm"
