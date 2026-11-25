#!/usr/bin/env bash
# Original explicit ADC/DAC bridge netlist with native physical triples.
set -euo pipefail
if (( $# != 4 )); then
    printf 'Usage: %s SOURCE.net PINS MODEL_CM WORK_DIR\n' "$0" >&2
    exit 2
fi
source="$(realpath -- "$1")"
pins="$2"
cm="$(realpath -- "$3")"
work="$(realpath -- "$4")"
printf 'codemodel %s\n' "$cm" >"$work/.spiceinit"
awk -v pins="$pins" '
    {
        lines[NR]=$0
        if (tolower($1)==".model") {
            kind=tolower($3)
            if (kind ~ /^adc_schmitt\(/) model_kind[tolower($2)]="schmitt"
            else if (kind ~ /^adc_bridge\(/) model_kind[tolower($2)]="adc"
            else if (kind ~ /^dac_bridge\(/) model_kind[tolower($2)]="dac"
        }
    }
    END {
        # Discover all bridge models before lowering instances: SPICE
        # permits model declarations after the instances that use them.
        for (line=1;line<=NR;line++) {
            $0=lines[line]
            model=model_kind[tolower($NF)]
            if (tolower($1)==".model") {
                if (model_kind[tolower($2)]=="schmitt") {
                    sub(/adc_schmitt\(/,"sr_family_adc(schmitt=true ")
                    # Elements adc_schmitt has no IC model parameter;
                    # a supplied IC is explicitly warned-and-ignored.
                    sub(/ ic=[^ )]*/,"")
                    sub(/ out_family="HC"/,"")
                } else if (model_kind[tolower($2)]=="adc") {
                    sub(/adc_bridge\(/,"sr_family_adc(")
                    # Elements adc_bridge likewise warns-and-ignores IC.
                    sub(/ ic=[^ )]*/,"")
                    sub(/ out_family="HC"/,"")
                } else if (model_kind[tolower($2)]=="dac") {
                    sub(/dac_bridge\(/,"sr_family_dac(")
                    sub(/in_family="HC" /,"")
                }
            }
            if (tolower(substr($1,1,1))=="a" && model!="") {
                if (NF!=4) {
                    print "Bridge instance must have scalar input, output, model: " lines[line] > "/dev/stderr"
                    exit 1
                }
                if (model=="dac")
                    print $1,$2,$2 "_sink",$2 "_source","%gd(" $3 " 0)",$4
                else
                    print $1,"%gd(" $2 " 0)",$3,$3 "_sink",$3 "_source",$4
                continue
            }
            if (tolower($1)==".print") continue
            if (tolower($1)==".end") {
                print ".control\nset numdgt=15\nrun"
                n=split(pins,selected,",")
                printf "wrdata native.dat"
                for (i=1;i<=n;i++) printf " %s",selected[i]
                print "\nquit\n.endc"
            }
            print
        }
    }
' "$source" >"$work/circuit.cir"
rm -f -- "$work/native.dat"
(cd "$work" && ngspice -b circuit.cir >run.log 2>&1) || { tail -80 "$work/run.log" >&2; exit 1; }
if rg -i 'error:|fatal error|timestep too small|no such model' "$work/run.log"; then exit 1; fi
test -s "$work/native.dat"
