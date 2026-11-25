#!/usr/bin/env bash
# Overload-before-delay preserves distinct logical request identity.
set -euo pipefail
here="$(cd -- "$(dirname -- "$0")" && pwd)"
# shellcheck source=runtime/xspice/prepare-original-smoke.sh
source "$here/prepare-original-smoke.sh"
prepare_original_smoke "overload_identity" "$@"
vendor="$here/fixtures/vendor-overload-identity"
for name in asymmetric-overload-interrupted overload-request-identity startup-disabled; do
    width=$(jq -er --arg name "$name" '.[] | select(.case == $name) | .outputs' "$vendor/oracle-manifest.json")
    pins=""
    for ((pin=width-1; pin>=0; pin--)); do
        pins="${pins:+$pins,}i(vforce$pin)"
    done
    "$here/run-compiled-vendor-physical.sh" "$vendor/$name.net" "$pins" "$cm" "$bins" "$work"
    rg -q '^\.model [^ ]+ sr_alb_lut_bit\(' "$work/generated.inc"
    if [[ $name == overload-request-identity ]]; then
        rg -q '^\.model [^ ]+ sr_alb_stateful\(' "$work/generated.inc"
    fi
    awk -v name="$name" -v pins="$pins" -v sample_only=1 -v sample_tolerance=.0011 -v unit=A \
        -f "$here/check-stateful-physical-events.awk" "$vendor/vendor-current.csv" "$work/native.dat"
done
