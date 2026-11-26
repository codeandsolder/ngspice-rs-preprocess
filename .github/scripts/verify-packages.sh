#!/usr/bin/env bash
# Validate the production package graph, then qualify real registry-format
# archives in a disposable source tree. This script never publishes.
set -euo pipefail

metadata() {
    local graph
    graph=$(cargo metadata --locked --no-deps --format-version 1)
    jq -ce '
        def require($ok; $message): if $ok then . else error($message) end;
        . as $ws
        | [$ws.packages[] | select(.id as $id | $ws.workspace_default_members | index($id))] as $production
        | require(all($ws.packages[]; .publish == []); "publication requires explicit authorization")
        | require(([$ws.packages[] | select(.id as $id | ($ws.workspace_default_members | index($id)) == null) | .name] | sort)
                  == ["model-alb-experimental", "model-alb-xspice-ffi"];
                  "review nonproduction package membership")
        | [$production[]
           | . as $pkg
           | require((.description | length) > 0 and (.readme | length) > 0 and
                     (.repository | length) > 0; "missing package metadata: " + .name)
           | require((.license == null) != (.license_file == null);
                     "specify exactly one license or license-file: " + .name)
           | require(all(.dependencies[];
                  . as $dep
                  | if .path != null then
                      any($production[];
                          .name == $dep.name and
                          (.manifest_path | sub("/Cargo.toml$"; "")) == $dep.path and
                          $dep.req == ("=" + .version))
                    else .source == "registry+https://github.com/rust-lang/crates.io-index"
                    end);
                  "non-registry dependency or unversioned/nonproduction path dependency: " + .name)
          ]' <<<"$graph"
}

check_licenses() {
    local selection=$1 item path source
    while IFS= read -r item; do
        path=$(jq -r .manifest_path <<<"$item")
        source=$(jq -r '.license_file // "LICENSE"' <<<"$item")
        test -s "$(dirname "$path")/$source"
        case "$(jq -r .license <<<"$item")" in
            MIT) cmp -- LICENSE "$(dirname "$path")/$source" ;;
            'MIT AND BSD-3-Clause'|'MIT AND BSD-3-Clause AND Apache-2.0') cmp -- THIRD_PARTY_NOTICES.md "$(dirname "$path")/$source" ;;
        esac
        test -s "$(dirname "$path")/$(jq -r .readme <<<"$item")"
    done < <(jq -c '.[]' <<<"$selection")
}

if [[ ${1:-} == --metadata-only ]]; then
    [[ $# == 1 ]]
    selection=$(metadata)
    check_licenses "$selection"
    printf 'Production package graph: %s crates; license and dependency metadata OK\n' "$(jq length <<<"$selection")"
    exit 0
fi
[[ $# == 0 ]]
: "${RUSTC_WRAPPER:?mandatory managed sccache wrapper is required}"
: "${CARGO_TARGET_DIR:?isolated Cargo target directory is required}"
[[ "$CARGO_TARGET_DIR" == /* ]]
root=$(git rev-parse --show-toplevel)
mkdir -p "$CARGO_TARGET_DIR"
temporary=$(mktemp -d "${RUNNER_TEMP:-$(dirname "$CARGO_TARGET_DIR")}/preprocess-packages.XXXXXXXX")
trap 'rm -rf -- "$temporary"' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
mkdir "$temporary/source" "$temporary/vendor" "$temporary/install" "$temporary/package-target"
git -C "$root" archive --format=tar HEAD | tar -xf - -C "$temporary/source"
printf 'Qualifying revision %s\n' "$(git -C "$root" rev-parse HEAD)"
cd "$temporary/source"
selection=$(metadata)
check_licenses "$selection"

# Cargo will not place a publish=false crate in its temporary registry
# overlay. Change only the publication allow-list of the selected packages
# in this disposable copy. The actual source checkout stays unpublishable.
packages=()
while IFS= read -r item; do
    name=$(jq -r .name <<<"$item")
    manifest=$(jq -r .manifest_path <<<"$item")
    sed -i 's/^publish.workspace = true$/publish = ["crates-io"]/' "$manifest"
    packages+=(-p "$name")
done < <(jq -c '.[]' <<<"$selection")
# Require Cargo to assemble and build every tarball in dependency order.
# A dry run contacts the public registry but cannot upload anything.
cargo publish --dry-run --locked "${packages[@]}" \
    --target-dir "$temporary/package-target" --config "build.build-dir='$temporary/package-target'"

# A fresh directory source supplies the exact public locked dependencies,
# plus our registry-format archives. It is not a path-patched workspace.
# Installing offline from this source checks the generated package lockfile
# and ensures no unpublished Git/path dependencies are needed.
cargo vendor --locked --versioned-dirs "$temporary/vendor" > /dev/null
while IFS= read -r item; do
    name=$(jq -r .name <<<"$item")
    version=$(jq -r .version <<<"$item")
    manifest=$(jq -r .manifest_path <<<"$item")
    # Cargo multi-package publish stages the verified tarballs in its
    # temporary registry, not the cargo package output directory. The
    # fresh target above prevents any prior archive from being selected.
    archive="$temporary/package-target/package/tmp-registry/$name-$version.crate"
    extracted="$temporary/vendor/$name-$version"
    tar -xf "$archive" -C "$temporary/vendor"
    crate_path=$(dirname "${manifest#"$temporary/source/"}")
    while IFS= read -r -d '' tracked; do
        test -f "$extracted/${tracked#"$crate_path/"}"
    done < <(git -C "$root" ls-tree -r --name-only -z HEAD -- "$crate_path")
    packaged=$(cargo metadata --locked --no-deps --format-version 1 --manifest-path "$extracted/Cargo.toml")
    jq -e --arg name "$name" --arg version "$version" '
        .packages | length == 1 and
        (.[0] | .name == $name and .version == $version and
         all(.dependencies[]; .path == null and .source == "registry+https://github.com/rust-lang/crates.io-index"))
    ' <<<"$packaged" >/dev/null
    license=$(jq -r '.license_file // "LICENSE"' <<<"$item")
    packaged_license=$(jq -r '.packages[0].license_file // "LICENSE"' <<<"$packaged")
    cmp -- "$(dirname "$manifest")/$license" "$extracted/$packaged_license"
    cmp -- "$(dirname "$manifest")/$(jq -r .readme <<<"$item")" \
        "$extracted/$(jq -r '.packages[0].readme' <<<"$packaged")"

    # Cargo directory-source checksums bind both the crate archive and every
    # extracted file. No checksum exceptions and no path dependency patches.
    (
        cd "$extracted"
        find . -type f -print0 | sort -z |
            while IFS= read -r -d '' file; do
                digest=$(sha256sum < "$file")
                jq -cn --arg key "${file#./}" --arg value "${digest%% *}" '{key:$key,value:$value}'
            done
    ) | jq -s --arg package "$(sha256sum < "$archive" | cut -d ' ' -f 1)" \
        '{package:$package,files:from_entries}' > "$temporary/checksums.json"
    mv -- "$temporary/checksums.json" "$extracted/.cargo-checksum.json"
done < <(jq -c '.[]' <<<"$selection")

registry=(
    --config 'source.crates-io.replace-with="qualified-packages"'
    --config "source.qualified-packages.directory='$temporary/vendor'"
)
# Each archive must resolve its complete locked graph (including tests and
# target-specific dependencies) independently of the workspace and registry.
while IFS= read -r item; do
    name=$(jq -r .name <<<"$item")
    version=$(jq -r .version <<<"$item")
    cargo metadata --locked --offline --format-version 1 "${registry[@]}" \
        --manifest-path "$temporary/vendor/$name-$version/Cargo.toml" >/dev/null
done < <(jq -c '.[]' <<<"$selection")

version=$(jq -r '.[] | select(.name == "ngspice-rs-preprocess") | .version' <<<"$selection")
# This is registry installation, not cargo install --path from the workspace.
cargo install --locked --offline --root "$temporary/install" \
    --version "=$version" --target-dir "$CARGO_TARGET_DIR" \
    --config "build.build-dir='$CARGO_TARGET_DIR'" "${registry[@]}" \
    ngspice-rs-preprocess
cd "$temporary"
printf '* Package verification\nV1 in 0 1\nR1 in 0 1k\n.op\n.end\n' > input.cir
"$temporary/install/bin/ngspice-rs-preprocess" native-deck input.cir prepared
cmp -- input.cir prepared/deck.cir
jq -e '.primary == "deck.cir"' prepared/manifest.json >/dev/null
printf 'Qualified %s packages and installed standalone CLI %s; nothing published.\n' \
    "$(jq length <<<"$selection")" "$version"
