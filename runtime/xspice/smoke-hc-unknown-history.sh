#!/usr/bin/env bash
# Independent actual SIMetrix Elements 9.40d ALB UNKNOWN history oracles:
# original complete physical netlists, no manually annotated bridge nodes.
set -euo pipefail
if (( $# != 3 )); then
    printf 'usage: %s CODEMODEL_CM RUST_BIN_DIR SCRATCH_ROOT\n' "$0" >&2
    exit 2
fi
here="$(cd -- "$(dirname -- "$0")" && pwd)"
cm="$(realpath -- "$1")"
bins="$(realpath -- "$2")"
scratch="$(realpath -- "$3")"
for binary in alb-boundary-discover alb-logical-expand alb-family-autowire; do
    test -x "$bins/$binary" || { echo "Missing $binary" >&2; exit 1; }
done
for tool in ngspice awk rg; do command -v "$tool" >/dev/null; done
work="$(mktemp -d "$scratch/ngspice_preprocess_hc_history_XXXXXXXX")"
trap 'rm -rf -- "$work"' EXIT
count=0
for variant in descending ascending initial-unknown ramp; do
    dir="$work/$variant"
    mkdir -p "$dir"
    case "$variant" in
        ramp)
            source="$here/fixtures/vendor-bidir/hc-input-threshold-ramp.net" ;;
        *)
            source="$here/fixtures/vendor-bidir/plateau/$variant.net" ;;
    esac
    awk 'toupper($1)!=".END" && toupper($1)!=".PRINT" {print}' "$source" >"$dir/original.cir"
    cat >> "$dir/original.cir" <<'CIR'
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
    rg -q 'unknown_as_zero=true unknown_as_last=true' "$dir/generated.inc" ||
        { echo "Compiled ALB did not retain known input history: $variant" >&2; exit 1; }
    printf 'codemodel %s\n' "$cm" > "$dir/.spiceinit"
    (
        cd "$dir"
        ngspice -b circuit.cir >sim.log 2>&1 || { tail -n 90 sim.log >&2; exit 1; }
        if rg -i 'fatal error|timestep too small|error:|no such model' sim.log; then
            echo "Physical ALB UNKNOWN-history simulation failed: $variant" >&2
            exit 1
        fi
        test -s result.dat
    )
    if [[ $variant == descending ]]; then
        awk '
          $1>14.9e-9 && $1<15.1e-9 && $2>2.2 && $2<2.4 && $4>4.9 {approach=1}
          $1>26.9e-9 && $1<27.1e-9 && $2>2.14 && $2<2.16 && $4>4.9 {retained=1}
          $1>32.9e-9 && $1<33.1e-9 && $4<.1 {released=1}
          END {exit !(approach && retained && released)}
        ' "$dir/result.dat" ||
            { echo 'Vendor ALB did not retain prior HIGH through ADC UNKNOWN band' >&2; exit 1; }
    elif [[ $variant == ascending ]]; then
        awk '
          $1>14.9e-9 && $1<15.1e-9 && $2>2.0 && $2<2.2 && $4<.1 {approach=1}
          $1>26.9e-9 && $1<27.1e-9 && $2>2.14 && $2<2.16 && $4<.1 {retained=1}
          $1>32.9e-9 && $1<33.1e-9 && $4>4.9 {raised=1}
          END {exit !(approach && retained && raised)}
        ' "$dir/result.dat" ||
            { echo 'Vendor ALB did not retain prior LOW through ADC UNKNOWN band' >&2; exit 1; }
    elif [[ $variant == initial-unknown ]]; then
        awk '
          $1>1e-9 && $1<1.1e-9 && $2>2.14 && $2<2.16 && $4<.1 {dc=1}
          $1>14.9e-9 && $1<15.1e-9 && $2>2.14 && $2<2.16 && $4<.1 {held=1}
          END {exit !(dc && held)}
        ' "$dir/result.dat" ||
            { echo 'Vendor ALB initial UNKNOWN must evaluate LOW' >&2; exit 1; }
    else
        # These two original vendor crossing times were extracted independently
        # from the real 9.40d 2,101-point reference, *not* from native output.
        refs="$here/fixtures/vendor-bidir/vendor-extra-midpoints.csv"
        expected_rise="$(awk -F, '$1=="hc-input-threshold-ramp" && $2=="observe" && $3=="rise" {print $4}' "$refs")"
        expected_fall="$(awk -F, '$1=="hc-input-threshold-ramp" && $2=="observe" && $3=="fall" {print $4}' "$refs")"
        test -n "$expected_rise" && test -n "$expected_fall"
        awk -v er="$expected_rise" -v ef="$expected_fall" '
          {
            t=$3*1e9; voltage=$4
            if (NR>1 && prior<2.5 && voltage>=2.5)
                rise=last_time+(t-last_time)*(2.5-prior)/(voltage-prior)
            if (NR>1 && prior>=2.5 && voltage<2.5)
                fall=last_time+(t-last_time)*(2.5-prior)/(voltage-prior)
            prior=voltage; last_time=t
          }
          END {
            printf "Original vendor HC slow-ramp: rise vendor %.6f / native %.6f ns; fall vendor %.6f / native %.6f ns\n",er,rise,ef,fall
            if (!rise || !fall || (rise-er > .2 || er-rise > .2) ||
                (fall-ef > .2 || ef-fall > .2))
                exit 1
          }
        ' "$dir/result.dat" ||
            { echo 'Original 9.40d slow UNKNOWN transition timing mismatched' >&2; exit 1; }
    fi
    count=$((count+1))
    printf 'VENDOR UNKNOWN HISTORY PASS %s\n' "$variant"
done
printf '%s/%s independent original-source HC ALB input-history cases passed\n' "$count" "$count"
