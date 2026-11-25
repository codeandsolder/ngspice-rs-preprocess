#!/usr/bin/env bash
# A passing native/stock XSPICE oracle must both simulate successfully
# and emit its expected waveform. Negative assertions belong to the
# independent waveform/contract comparators, not to simulator status.
set -euo pipefail
if (( $# < 3 )); then
    printf 'Usage: %s SIMULATION_DIRECTORY NETLIST_BASENAME OUTPUT_TRACE [OUTPUT_TRACE...]\n' "$0" >&2
    exit 2
fi
work="$(realpath -- "$1")"
deck="$2"
shift 2
if [[ "$deck" == /* || "$deck" == */* ]]; then
    echo 'Netlist must be a name relative to the simulation directory' >&2
    exit 2
fi
for trace in "$@"; do
    if [[ "$trace" == /* || "$trace" == */* ]]; then
        echo 'Output traces must be names relative to the simulation directory' >&2
        exit 2
    fi
done
test -s "$work/$deck" || { echo "Missing original simulator deck: $work/$deck" >&2; exit 1; }
command -v ngspice >/dev/null
command -v rg >/dev/null
# A rerun must produce every requested trace; stale output cannot pass.
for trace in "$@"; do
    rm -f -- "$work/$trace"
done
(
    cd "$work"
    if ! ngspice -b "$deck" >run.log 2>&1; then
        echo "ngspice exited nonzero for $work/$deck" >&2
        tail -n 85 run.log >&2
        exit 1
    fi
    if rg -iq 'error:|fatal error|timestep too small|no such model' run.log; then
        echo "ngspice logged a fatal simulation error for $work/$deck" >&2
        tail -n 85 run.log >&2
        exit 1
    fi
    for trace in "$@"; do
        test -s "$trace" || {
            echo "ngspice did not produce expected waveform: $work/$trace" >&2
            tail -n 85 run.log >&2
            exit 1
        }
    done
)
