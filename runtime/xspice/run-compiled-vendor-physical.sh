#!/usr/bin/env bash
# Original source vendor netlist -> automatically compiled native waveform.
set -euo pipefail
if (( $# != 6 )); then
    printf 'usage: %s SOURCE.net PINS CODEMODEL_CM RUST_BIN_DIR WORK_DIR EXTERNAL_MODEL_JSON\n' "$0" >&2
    exit 2
fi
here="$(cd -- "$(dirname -- "$0")" && pwd)"
source="$(realpath -- "$1")"
pins="$2"
cm="$(realpath -- "$3")"
bins="$(realpath -- "$4")"
work="$(realpath -- "$5")"
family="$here/fixtures/family"
printf 'codemodel %s\n' "$cm" > "$work/.spiceinit"
rm -f -- "$work/native.dat"
explicit=0
if rg -q '^\.model [^ ]+ dac_bridge\(' "$source"; then explicit=1; fi
: > "$work/dac.inc"
: > "$work/models.txt"
awk -v pins="$pins" -v explicit="$explicit" -v wrappers="$work/dac.inc" -v models="$work/models.txt" '
    NR==1 { print; if (explicit) print ".include \"dac.inc\""; next }
    tolower($1)==".model" && $3 ~ /^dac_bridge\(/ {
        model=$2
        native="SR_EXPLICIT_" model
        print model >> models
        print ".subckt " native " D D_SINK D_SOURCE Q" >> wrappers
        print "Adac D D_SINK D_SOURCE %gd(Q 0) MDAC" >> wrappers
        params=$0
        if (!sub(/^\.model [^ ]+ dac_bridge\(in_family="HC" /, ".model MDAC sr_family_dac(", params)) exit 2
        if (params !~ /[ (,]sink_current=/) sub(/\)$/, " sink_current=0)",params)
        if (params !~ /[ (,]source_current=/) sub(/\)$/, " source_current=0)",params)
        print params >> wrappers
        print ".ends " native >> wrappers
        next
    }
    explicit && $1 ~ /^Adac/ {
        if (NF!=4) exit 2
        print "X" substr($1,2),$2,$3,$4
        next
    }
    tolower($1)==".print" {next}
    tolower($1)==".end" {
        count=split(pins,names,",")
        print ".control\nset numdgt=15\nrun"
        printf "wrdata native.dat"
        for(i=1;i<=count;i++){
            if(names[i] ~ /^i\([a-zA-Z0-9_]+\)$/) printf " %s",names[i]
            else if(names[i] ~ /^[a-zA-Z0-9_]+$/) printf " v(%s)",names[i]
            else exit 2
        }
        print "\nquit\n.endc"
    }
    {print}
' "$source" > "$work/original.cir"
jq -s --rawfile models "$work/models.txt" '
    .[0] + {bridge_models: (map(.bridge_models) | add), opaque_x_interfaces: ($models | split("\n") | map(select(length>0) |
        {model:.,pins:["digital","analog"],native_model:("SR_EXPLICIT_"+.),
         native_formals:["D","D_SINK","D_SOURCE","Q"]}))}
' "$family/auto-discovery-5v.json" "$family/auto-discovery-univ-5v.json" > "$work/electrical.json"
jq -s '{models: (map(.models) | add)}' "$family/auto-models.json" "$(realpath -e -- "$6")" > "$work/native-models.json"
"$bins/alb-boundary-discover" "$work/original.cir" "$work/electrical.json" "$work/boundaries.json"
"$bins/alb-family-compile" "$work/original.cir" "$work/boundaries.json" "$work/native-models.json" "$work/circuit.cir" "$work/generated.inc"
(cd "$work" && ngspice -b circuit.cir > sim.log 2>&1) || {
    tail -n 80 "$work/sim.log" >&2; exit 1;
}
if rg -i 'error:|fatal error|timestep too small|no such model' "$work/sim.log"; then exit 1; fi
test -s "$work/native.dat"
