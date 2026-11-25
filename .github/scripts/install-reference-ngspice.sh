#!/usr/bin/env bash
# Install the pinned ngspice 47 reference simulator without system changes.
set -euo pipefail
[[ $# == 1 && -n "$1" ]] || { echo "Usage: $0 NEW_DIRECTORY" >&2; exit 64; }
destination=$(realpath -m -- "$1")
[[ ! -e "$destination" && -d "$(dirname "$destination")" ]]
[[ "$(uname -m)" == x86_64 ]]
# ngspice's codemodel command does not handle quoted/whitespace-containing
# filenames. Require a plain temporary installation path explicitly.
[[ ! "$destination" =~ [[:space:][:cntrl:]] && "$destination" != *'"'* ]]
complete=false
cleanup() {
    if [[ "$complete" == false ]]; then rm -rf -- "$destination"; fi
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
if [[ ! -f /etc/arch-release ]]; then
    bash "$(dirname "$0")/install-reference-ngspice-source.sh" "$destination"
else
package=ngspice-47-1-x86_64.pkg.tar.zst
digest=4dfaa2ad51f6724b65482b71e300190a495d5c50b658bb42bec4b84559c210d9
mkdir -- "$destination"
# Signed package was verified against Arch's official keyring before pinning.
# The immutable digest authenticates the entire archive including XSPICE models.
curl -fsSL --retry 3 --max-time 120 \
    "https://archive.archlinux.org/packages/n/ngspice/$package" \
    -o "$destination/$package"
printf '%s  %s\n' "$digest" "$destination/$package" | sha256sum -c -
bsdtar -xf "$destination/$package" -C "$destination" usr/bin/ngspice usr/share/ngspice usr/lib/ngspice
rm -- "$destination/$package"
# Stock spinit hardcodes system codemodel paths. Replace only those paths;
# simulator options, compatibility handling and model binaries stay identical.
escaped=$(printf '%s' "$destination/usr/lib/ngspice" | sed 's/[\\&@]/\\&/g')
sed -i -E "s@^([[:space:]]*(codemodel|osdi)[[:space:]]+)/usr/lib/ngspice/([^[:space:]]+)@\\1$escaped/\\3@" \
    "$destination/usr/share/ngspice/scripts/spinit"
mkdir -- "$destination/bin"
cat > "$destination/bin/ngspice" <<'WRAPPER'
#!/usr/bin/env bash
set -euo pipefail
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
# These are the upstream simulator's data-directory inputs, not build settings.
export SPICE_LIB_DIR="$root/usr/share/ngspice"
export SPICE_SCRIPTS="$SPICE_LIB_DIR/scripts"
export SPICE_HELP_DIR="$SPICE_LIB_DIR/helpdir"
exec "$root/usr/bin/ngspice" "$@"
WRAPPER
chmod +x "$destination/bin/ngspice"
fi
# The managed Linux hosts supply the ordinary ELF runtime libraries. Fail
# explicitly instead of silently falling back to a system ngspice binary.
version=$("$destination/bin/ngspice" --version)
printf '%s\n' "$version"
[[ "$version" == *"ngspice-47 :"* && "$version" == *"KLU Direct Linear Solver"* ]]
# Check loading and execution of the relocated analog XSPICE model, not
# merely the program version. Do not depend on a host-global spinit.
mkdir "$destination/probe"
cat > "$destination/probe/test.cir" <<'DECK'
* Reference ngspice XSPICE probe
V1 in 0 1
A1 in out gain_model
.model gain_model gain(gain=2)
R1 out 0 1k
.control
op
print v(out)
quit
.endc
.end
DECK
result=$(cd "$destination/probe" && "$destination/bin/ngspice" -b test.cir 2>&1)
printf '%s\n' "$result"
[[ "$result" == *"v(out) = 2.000000e+00"* ]]
[[ "$result" != *"Error"* && "$result" != *"not been loaded"* ]]
rm -rf -- "$destination/probe"
complete=true
