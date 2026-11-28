#!/usr/bin/env bash
set -euo pipefail
if (( $# != 2 )); then
    printf 'Usage: %s STATEFUL_CODEMODEL_CM SCRATCH_ROOT\n' "$0" >&2
    exit 2
fi
here="$(cd -- "$(dirname -- "$0")" && pwd)"
model="$(realpath -- "$1")"
root="$(realpath -- "$2")"
test -s "$model"
work="$(mktemp -d "$root/stateful-comb-XXXXXXXX")"
trap 'rm -rf -- "$work"' EXIT
printf 'codemodel %s\n' "$model" > "$work/.spiceinit"
count=0
for fixture_dir in "$here"/fixtures/vendor-{comb,initial,race,feedback,bitwise,boundary,release-race,port-delay,internal-async,internal-release}; do
    refs="$fixture_dir/vendor-q-2p5v-events.csv"
    startup_refs="$fixture_dir/vendor-q-initial-voltages.csv"
    test -s "$refs"
    test -s "$startup_refs"
    # Optional source-controlled pair oracle: a vendor-confirmed pair may
    # require identical full physical traces, or a deliberately different
    # positive control. Keep outputs only inside this trap-cleaned scratch.
    pair_manifest="$fixture_dir/vendor-waveform-pairs.csv"
    waveform_dir="$work/waveforms/$(basename "$fixture_dir")"
    if [[ -f "$pair_manifest" ]]; then mkdir -p -- "$waveform_dir"; fi
    for source in "$fixture_dir"/*.net; do
        [[ -f "$source" ]] || { echo "No source fixtures in $fixture_dir" >&2; exit 1; }
        name="$(basename "$source" .net)"
        count=$((count+1))
        "$here/run-stateful-vendor-case.sh" "$source" "$work" 1p
        pin_count="$(< "$work/output-pin-count")"
        awk -v name="$name" -v pins="$pin_count" \
            -f "$here/check-stateful-extended-edges.awk" "$refs" "$startup_refs" "$work/native.dat"
        if [[ -f "$pair_manifest" ]]; then
            cp -- "$work/native.dat" "$waveform_dir/$name.dat"
        fi
    done
    if [[ -f "$pair_manifest" ]]; then
        {
            IFS=, read -r header_left header_right header_relation header_extra
            [[ "$header_left" == source && "$header_right" == control &&
               "$header_relation" == expected_relation && -z "$header_extra" ]] || {
                echo "Malformed vendor waveform-pair header: $pair_manifest" >&2
                exit 1
            }
            pairs=0
            while IFS=, read -r source_name control_name relation extra; do
                [[ -n "$source_name" && -n "$control_name" && -z "$extra" &&
                   "$source_name" != */* && "$control_name" != */* &&
                   "$source_name" != *..* && "$control_name" != *..* ]] || {
                    echo "Malformed vendor waveform-pair entry: $pair_manifest" >&2
                    exit 1
                }
                source_wave="$waveform_dir/$source_name.dat"
                control_wave="$waveform_dir/$control_name.dat"
                test -s "$source_wave" && test -s "$control_wave" || {
                    echo "Missing vendor waveform-pair source: $source_name / $control_name" >&2
                    exit 1
                }
                case "$relation" in
                    identical)
                        cmp -s -- "$source_wave" "$control_wave" || {
                            echo "Vendor-identical native waveforms differ: $source_name / $control_name" >&2
                            exit 1
                        }
                        ;;
                    different)
                        if cmp -s -- "$source_wave" "$control_wave"; then
                            echo "Vendor-different control has identical native waveforms: $source_name / $control_name" >&2
                            exit 1
                        fi
                        ;;
                    *)
                        echo "Invalid vendor waveform-pair relationship: $relation" >&2
                        exit 1
                        ;;
                esac
                pairs=$((pairs+1))
                printf 'Original-vendor waveform pair PASS %s / %s: %s\n' \
                    "$source_name" "$control_name" "$relation"
            done
            (( pairs > 0 )) || { echo "Empty vendor waveform-pair manifest" >&2; exit 1; }
        } < "$pair_manifest"
    fi
done
printf '%d original-source SIMetrix COMB, initialization, race, BITWISE, output-delay, and timing-boundary physical differential scenarios passed\n' "$count"
