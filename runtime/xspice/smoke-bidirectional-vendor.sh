#!/usr/bin/env bash
# Real Elements 9.40d HC-family analog output conductance oracles.
# Vendor files are normalized byte-identical to the proprietary simulations.
# The separate ~1ns MON propagation discrepancy is tracked in Notion; this
# proves analog plateaus and independent high-Z/strong-drive behavior only.
set -euo pipefail
if (( $# != 3 )); then
    printf 'usage: %s CODEMODEL_CM RUST_BIN_DIR SCRATCH_ROOT\n' "$0" >&2
    exit 2
fi
here="$(cd -- "$(dirname -- "$0")" && pwd)"
cm="$(realpath -- "$1")"
bins="$(realpath -- "$2")"
root="$(realpath -- "$3")"
work="$(mktemp -d "$root/vendor-bidir-XXXXXXXX")"
trap 'rm -rf -- "$work"' EXIT
count=0
for name in strong-high strong-low high-out-res20 high-out-res200; do
    case "$name" in
        strong-low) original="$here/fixtures/vendor-bidir/drive-low-pullup.net" ;;
        *) original="$here/fixtures/vendor-bidir/single-pin-tristate.net" ;;
    esac
    dir="$work/$name"
    mkdir -p "$dir"
    # The proprietary original uses .PRINT, whereas ngspice wrdata allows
    # numeric midpoint and plateau validation without parsing simulator logs.
    awk 'toupper($1)!=".END" { print }' "$original" >"$dir/original.cir"
    case "$name" in
        high-out-res20) sed -i 's/out_res=40/out_res=20/' "$dir/original.cir" ;;
        high-out-res200) sed -i 's/out_res=40/out_res=200/' "$dir/original.cir" ;;
    esac
    cat >>"$dir/original.cir" <<'CIR'
.control
run
wrdata result.dat v(pin) v(observe) v(oe) v(ext)
quit
.endc
.end
CIR
    "$bins/alb-boundary-discover" \
        "$dir/original.cir" "$here/fixtures/family/auto-discovery-5v.json" \
        "$dir/discovered.json"
    "$bins/alb-logical-expand" \
        "$dir/original.cir" "$dir/discovered.json" \
        "$dir/lowered.cir" "$dir/generated.inc"
    "$bins/alb-family-autowire" \
        "$dir/lowered.cir" "$dir/generated.inc" \
        "$dir/discovered.json" \
        "$here/fixtures/family/auto-models.json" "$dir/circuit.cir"
    [[ $(rg -F -c 'sr_family_adc(' "$dir/circuit.cir") == 2 ]] ||
        { echo 'Vendor bidirectional source requires 2 independent ADCs' >&2; exit 1; }
    [[ $(rg -F -c 'sr_family_dac(' "$dir/circuit.cir") == 2 ]] ||
        { echo 'Vendor bidirectional source requires 2 independent DACs' >&2; exit 1; }
    rg -q 'g_pullup=2.4e-2 g_pulldown=3.4e-2' "$dir/circuit.cir" ||
        { echo 'HC fixed physical family conductances are not preserved' >&2; exit 1; }
    printf 'codemodel %s\n' "$cm" >"$dir/.spiceinit"
    (
        cd "$dir"
        ngspice -b circuit.cir >run.log 2>&1 || {
            tail -n 70 run.log >&2; exit 1;
        }
        if rg -i 'fatal error|timestep too small|error:|no such model' run.log; then
            echo "Unexpected native error: $name" >&2; exit 1
        fi
        test -s result.dat
    )
    if [[ $name == strong-low ]]; then
        awk '
          $1>3.8e-9 && $1<4.1e-9 && $2>4.95 && $2<5.01 {released=1}
          $1>21.9e-9 && $1<22.1e-9 && $2>.139 && $2<.148 {driven=1}
          $1>38.9e-9 && $1<39.1e-9 && $2>4.7 {restored=1}
          END {exit !(released && driven && restored)}
        ' "$dir/result.dat" ||
            { echo 'Original HC LOW drive / 5V pull-up / release mismatch' >&2; exit 1; }
    else
        awk '
          $1>2e-9 && $1<2.2e-9 && $2<.1 {released=1}
          $1>24.9e-9 && $1<25.1e-9 && $2>4.795 && $2<4.805 {driven=1}
          $1>38.9e-9 && $1<39.1e-9 && $2<.16 {restored=1}
          END {exit !(released && driven && restored)}
        ' "$dir/result.dat" ||
            { echo "Original HC HIGH drive / 1k pull-down / release mismatch: $name" >&2; exit 1; }
    fi
    count=$((count+1))
    printf 'VENDOR PHYSICAL HC PASS %s\n' "$name"
done
printf '%s/%s original HC analog output conductance/release cases passed\n' "$count" "$count"
