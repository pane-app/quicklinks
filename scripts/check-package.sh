#!/usr/bin/env bash
# Checks this repository's package: cargo has built the component, its name
# is the one pane.json names, and the images pane.json names are here.
# Run by CI and by the Release workflow; run it yourself after a build.
set -euo pipefail

crate=$(sed -n 's/^name = "\(.*\)"$/\1/p' Cargo.toml | head -n 1)
built="target/wasm32-wasip2/release/${crate//-/_}.wasm"
test -f "$built" || { echo "missing built component: $built" >&2; exit 1; }

components=$(jq -r '([.commands[]?.component] + [.operations[]?.component]) | unique | .[]' pane.json)
test -n "$components" || { echo "pane.json names no component" >&2; exit 1; }
for component in $components; do
  expected="${crate//-/_}.wasm"
  test "$(basename "$component")" = "$expected" \
    || { echo "pane.json names $component, but crate $crate builds $expected" >&2; exit 1; }
done

images=$(jq -r '([.icon?] + [.commands[]?.icon?]) | map(select(type == "string")) | unique | .[]' pane.json)
for image in $images; do
  test -f "$image" || { echo "pane.json names a missing image: $image" >&2; exit 1; }
done

echo "package checks out: crate $crate builds $built"
for component in $components; do echo "  pane.json names $component"; done
for image in $images; do echo "  image $image"; done
