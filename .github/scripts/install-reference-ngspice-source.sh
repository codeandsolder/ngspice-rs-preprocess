#!/usr/bin/env bash
# The upstream release archive is pinned to the independently published Arch
# packaging checksum. Build locally for the host libc, with XSPICE and KLU.
set -euo pipefail
[[ $# == 1 && -n "$1" ]] || exit 64
: "${RUSTC_WRAPPER:?mandatory sccache wrapper required for source compilation}"
[[ -x "$RUSTC_WRAPPER" ]]
destination=$(realpath -m -- "$1")
[[ ! -e "$destination" && -d "$(dirname "$destination")" ]]
[[ ! "$destination" =~ [[:space:][:cntrl:]] && "$destination" != *'"'* ]]
mkdir -- "$destination"
complete=false
cleanup() {
    if [[ "$complete" == false ]]; then rm -rf -- "$destination"; fi
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
archive=ngspice-47.tar.gz
digest=0eec72a8450f6b654ea13ac62ea74c09a1f74093c8b3b88d098100bd0193df24f07ffe723dfe174dd576f6488f95484b2898934a20dee0865c3491b560dc6583
curl -fsSL --retry 3 --max-time 120 \
    "https://downloads.sourceforge.net/project/ngspice/ng-spice-rework/47/$archive" \
    -o "$destination/$archive"
printf '%s  %s\n' "$digest" "$destination/$archive" | sha512sum -c -
tar -xzf "$destination/$archive" -C "$destination"
rm -- "$destination/$archive"
mkdir "$destination/build"
(
    cd "$destination/build"
    ../ngspice-47/configure --prefix="$destination" --without-x --with-readline=no \
        --enable-xspice --enable-cider --enable-openmp --disable-debug \
        CC="$RUSTC_WRAPPER gcc" CXX="$RUSTC_WRAPPER g++"
    make -j"$(nproc)"
    make install
)
rm -rf -- "$destination/build" "$destination/ngspice-47"
complete=true
