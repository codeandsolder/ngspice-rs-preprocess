#!/usr/bin/env bash
set -euo pipefail
if (( $# != 3 )); then
    printf 'usage: %s CODEMODEL_CM RUST_BIN_DIRECTORY SCRATCH_ROOT\n' "$0" >&2
    exit 2
fi
here="$(cd -- "$(dirname -- "$0")" && pwd)"
cm="$(realpath -- "$1")"
bins="$(realpath -- "$2")"
scratch="$(realpath -- "$3")"
for name in alb-xspice-compile alb-family-autowire; do
    test -x "$bins/$name" || { echo "Missing Rust compiler $name" >&2; exit 1; }
done
for tool in ngspice rg awk; do command -v "$tool" >/dev/null; done
work="$(mktemp -d "$scratch/ngspice_preprocess_autowire_XXXXXXXX")"
trap 'rm -rf -- "$work"' EXIT
printf 'codemodel %s\n' "$cm" >"$work/.spiceinit"
"$bins/alb-xspice-compile" \
    "$here/fixtures/bundle-buffer.alb" \
    "$here/fixtures/family/buffer-40r.json" \
    "$work/normal.inc" ALB_BUFFER
"$bins/alb-xspice-compile" \
    "$here/fixtures/bundle-buffer.alb" \
    "$here/fixtures/family/buffer-zero-current.json" \
    "$work/zero.inc" ALB_BUFFER
for variant in direct-3v3 direct-5v nested-3v3 shared; do
    dir="$work/$variant"
    mkdir -p "$dir"
    cp -- "$work/.spiceinit" "$dir/"
    if [[ $variant == shared ]]; then
        source=auto-shared.cir
        bindings=auto-bindings-shared.json
        cp -- "$work/zero.inc" "$dir/generated.inc"
    else
        source=auto-direct.cir
        bindings=auto-bindings-3v3.json
        if [[ $variant == nested-3v3 ]]; then
            source=auto-nested.cir
        elif [[ $variant == direct-5v ]]; then
            bindings=auto-bindings-5v.json
        fi
        cp -- "$work/normal.inc" "$dir/generated.inc"
    fi
    "$bins/alb-family-autowire" \
        "$here/fixtures/family/$source" "$dir/generated.inc" \
        "$here/fixtures/family/$bindings" \
        "$here/fixtures/family/auto-models.json" "$dir/circuit.cir"
    if [[ $variant == shared ]]; then
        [[ $(rg -c 'sr_family_adc\(' "$dir/circuit.cir") == 1 ]] ||
            { echo "Shared auto ADC was not grouped" >&2; exit 1; }
        if rg '^Asr_load' "$dir/circuit.cir"; then
            echo 'Autowire duplicated physical ALB input loading' >&2
            exit 1
        fi
    else
        rg -q 'sr_family_adc\(' "$dir/circuit.cir"
        rg -q 'sr_family_dac\(' "$dir/circuit.cir"
        rg -q 'g_pullup=2\.5e-2 g_pulldown=2\.5e-2' "$dir/circuit.cir"
    fi
    (
        cd "$dir"
        if ! ngspice -b circuit.cir >run.log 2>&1; then
            tail -n 75 run.log >&2
            exit 1
        fi
        if rg -i 'fatal error|timestep too small|error:|no such model' run.log; then
            echo "Native autowired simulation failed: $variant" >&2
            exit 1
        fi
        [[ -s result.dat ]] || { echo "Missing native waveform: $variant" >&2; exit 1; }
    )
done
for variant in direct-3v3 nested-3v3; do
    awk '$1>10e-9 && $1<11e-9 && $2>3.1 && $2<3.31 {pass=1}
         END {exit !pass}' "$work/$variant/result.dat" ||
        { echo "$variant failed analog supply-scaled HIGH" >&2; exit 1; }
done
awk '$1>10e-9 && $1<11e-9 && $2>=-.01 && $2<.1 {pass=1}
     END {exit !pass}' "$work/direct-5v/result.dat" ||
    { echo 'Nominal-supply auto bridge incorrectly interpreted 1.5V as HIGH' >&2; exit 1; }
awk '$1>4.99e-9 && $1<5.01e-9 && $2>3.04 && $2<3.29 {pass=1}
     END {exit !pass}' "$work/shared/result.dat" ||
    { echo 'Shared hierarchy autowire did not reflect 2 x 2pF input load' >&2; exit 1; }
echo '4/4 automatically discovered family bridge ngspice simulations passed'
