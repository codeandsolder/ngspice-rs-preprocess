#!/usr/bin/env bash
# Reproduce ngspice-47 VBIC numerical oracle rows without leaving artifacts.
# Usage: generate_vbic_oracle.sh default|extended > /tmp/vbic-oracle.csv
set -euo pipefail

mode=${1:-default}
case "$mode" in
  default)
    variants='default'
    biases='0.50 -0.50;0.65 -0.30;0.80 -0.40;0.90 -0.20;0.95 -0.10;0.70 0.50;0.55 0.70;0.85 0.80'
    echo '# ngspice 47: qbm,external_vbe,external_vbc,internal_vbe,internal_vbc,@q1[ic],@q1[gm],@q1[go]'
    ;;
  extended)
    variants='smooth punchthrough double_smooth'
    biases='0.65 -0.80;0.80 -0.50;0.95 -0.10;0.70 0.50'
    echo '# ngspice 47: variant,qbm,external_vbe,external_vbc,internal_vbe,internal_vbc,@q1[ic],@q1[gm],@q1[go]'
    ;;
  *)
    echo 'usage: generate_vbic_oracle.sh default|extended' >&2
    exit 2
    ;;
esac

tmpdir=$(mktemp -d /tmp/vbic-oracle-XXXXXX)
trap 'rm -rf "$tmpdir"' EXIT

for variant in $variants; do
  case "$variant" in
    default) extra='' ;;
    smooth) extra='aje=0.02 ajc=0.03' ;;
    punchthrough) extra='aje=-0.5 ajc=-0.5 vrt=0.6' ;;
    double_smooth) extra='aje=0.02 ajc=0.03 vrt=0.6 art=0.04' ;;
  esac
  for qbm in 0 1; do
    IFS=';' read -r -a cases <<< "$biases"
    for entry in "${cases[@]}"; do
      read -r external_be external_bc <<< "$entry"
      vce=$(awk -v a="$external_be" -v b="$external_bc" 'BEGIN { printf "%.12g", a-b }')
      cat > "$tmpdir/input.cir" <<DECK
VBIC transport source comparison $variant qbm=$qbm
VBE b 0 DC $external_be
VCE c 0 DC $vce
Q1 c b 0 0 0 TRANSPORT
.model TRANSPORT NPN level=9 tnom=27 is=4.81e-17 nf=1.018 nr=1.01 isrr=1 vef=189 ver=5.3 ikf=0.009 ikr=0.01 nkf=0.5 qbm=$qbm
+ $extra
+ ibei=0 iben=0 ibci=0 ibcn=0 ibeip=0 ibenp=0 ibcip=0 ibcnp=0 cje=0 cjc=0 cjep=0 cjcp=0 cbeo=0 cbco=0
+ rcx=0 rci=0 re=0 rbx=0 rbi=0 rbp=0 rs=0 avc1=0
.options gmin=1e-18
.control
set numdgt=15
op
print @q1[vbe] @q1[vbc] @q1[ic] @q1[gm] @q1[go]
quit
.endc
.end
DECK
      if ! ngspice -b "$tmpdir/input.cir" > "$tmpdir/output" 2>&1; then
        cat "$tmpdir/output" >&2
        exit 1
      fi
      if rg -qi "unrecognized parameter|Warning: Model issue|fatal error" "$tmpdir/output"; then
        cat "$tmpdir/output" >&2
        exit 1
      fi
      if ! rg -q 'No. of Data Rows : 1' "$tmpdir/output"; then
        cat "$tmpdir/output" >&2
        exit 1
      fi
      if [[ "$mode" == default ]]; then
        prefix="$qbm,$external_be,$external_bc"
      else
        prefix="$variant,$qbm,$external_be,$external_bc"
      fi
      awk -v prefix="$prefix" '
        BEGIN { printf "%s", prefix }
        /@q1\[vbe\] =/ { printf ",%s", $3 }
        /@q1\[vbc\] =/ { printf ",%s", $3 }
        /@q1\[ic\] =/ { printf ",%s", $3 }
        /@q1\[gm\] =/ { printf ",%s", $3 }
        /@q1\[go\] =/ { printf ",%s", $3 }
        END { print "" }
      ' "$tmpdir/output"
    done
  done
done
