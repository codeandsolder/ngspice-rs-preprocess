#!/usr/bin/env bash
# The upstream release archive is pinned to the independently published Arch
# packaging checksum. Build the simulator and supplemental runtime from
# the same configured source tree so their XSPICE ABIs are identical.
set -euo pipefail
[[ $# -ge 1 && $# -le 3 && -n "$1" ]] || exit 64
script_dir=$(cd -- "$(dirname -- "$0")" && pwd)
wrapper=$(realpath -- "${2:-/usr/local/libexec/sccache-client}")
jobs=${3:-$(nproc)}
[[ -x "$wrapper" && "$jobs" =~ ^[1-9][0-9]*$ ]]
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
(
    cd "$destination/ngspice-47"
    # cmpp parses multiple interfaces using one scanner; restart it on
    # the new stream rather than assigning yyin after an EOF restart.
    # This only changes the interface generator, not simulation semantics.
    sed -i 's/^   ifs_yyin = fp;$/   ifs_yyrestart(fp);/' src/xspice/cmpp/read_ifs.c
    rg -q '^   ifs_yyrestart\(fp\);$' src/xspice/cmpp/read_ifs.c
    ./configure --prefix="$destination" --without-x --with-readline=no \
        --enable-xspice --enable-cider --enable-openmp --disable-debug \
        CC="$wrapper gcc" CXX="$wrapper g++"
    make -j"$jobs"
    make install
)
bash "$script_dir/../../runtime/logic-xspice/build.sh" \
    "$destination/ngspice-47" "$destination/lib/ngspice" "$wrapper"
rm -rf -- "$destination/ngspice-47"
complete=true
