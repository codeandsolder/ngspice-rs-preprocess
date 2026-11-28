#!/usr/bin/env bash
# Source-original Elements current probes for independent logic/strength ramps.
set -euo pipefail
here="$(cd -- "$(dirname -- "$0")" && pwd)"
# shellcheck source=runtime/xspice/prepare-original-smoke.sh
source "$here/prepare-original-smoke.sh"
prepare_original_smoke "dac_ramps" "$@"
vendor="$here/fixtures/vendor-dac-ramps"
for name in asymmetric-isolated asymmetric-interrupted asymmetric-opposite asymmetric-overload; do
    case "$name" in
        asymmetric-overload) pins='i(vforce11),i(vforce10),i(vforce9),i(vforce8),i(vforce7),i(vforce6),i(vforce5),i(vforce4),i(vforce3),i(vforce2),i(vforce1),i(vforce0)' ;;
        *) pins='i(vforce8),i(vforce7),i(vforce6),i(vforce5),i(vforce4),i(vforce3),i(vforce2),i(vforce1),i(vforce0)' ;;
    esac
    "$here/run-compiled-vendor-physical.sh" "$vendor/$name.net" "$pins" "$cm" "$bins" "$work"
    awk -v name="$name" -v pins="$pins" -v sample_only=1 -v sample_tolerance=.0011 -v unit=A \
        -f "$here/check-stateful-physical-events.awk" "$vendor/vendor-current.csv" "$work/native.dat"
done
