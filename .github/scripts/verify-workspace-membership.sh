#!/usr/bin/env bash
# Verify membership in a clean source archive, without relying on Git metadata.
set -euo pipefail
root=$(pwd -P)
manifests=$(rg --files --hidden --no-ignore -g Cargo.toml --null | jq -Rs 'split("\u0000") | map(select(length > 0)) | sort')
cargo metadata --locked --no-deps --format-version 1 |
    jq -e --arg root "$root" --argjson manifests "$manifests" '
        if .workspace_root != $root then
            error("not running at the archive workspace root")
        else
            .workspace_members as $members |
            (["Cargo.toml"] + [
                .packages[] |
                select(.id as $id | $members | index($id)) |
                .manifest_path |
                if startswith($root + "/") then ltrimstr($root + "/")
                else error("workspace member escapes the source archive: " + .) end
            ] | unique) as $bound |
            ($manifests - $bound) as $unbound |
            ($bound - $manifests) as $missing |
            if ($unbound | length) > 0 or ($missing | length) > 0 then
                error("unbound manifests: " + ($unbound | join(", ")) +
                      "; missing manifests: " + ($missing | join(", ")))
            else
                "Verified " + ($members | length | tostring) +
                " workspace packages against every archive Cargo.toml"
            end
        end
    ' -r
