#!/usr/bin/env bash
# Builds the Windows EXE on this Linux machine (cargo-xwin) and publishes it for the app's
# self-updater (src/update.rs): <dir>/Poczta-<build>.exe + <dir>/latest.json. HyperRust's update
# server (systemd user unit hyperrust-updates, ~/hyperrust-updates on the Tailscale address,
# port 8790) serves <dir>; running copies pick the build up within about a minute and swap it in
# when the app is minimized or untouched for 10 minutes.
#
#   scripts/publish.sh
#
# Every merge to main is published by itself (scripts/auto-publish.sh, systemd timer); by hand
# only for a build outside main.
#
# Env: POCZTA_UPDATE_DIR (default ~/hyperrust-updates/mailrust), KEEP (default 5 builds).
set -euo pipefail

repo=$(cd "$(dirname "$0")/.." && pwd)
out=${POCZTA_UPDATE_DIR:-$HOME/hyperrust-updates/mailrust}
keep=${KEEP:-5}
target=x86_64-pc-windows-msvc

cd "$repo"
built_at=$(date +%s)
sha=$(git rev-parse --short HEAD)
git diff --quiet HEAD || sha="$sha-dirty"
export POCZTA_BUILT_AT=$built_at POCZTA_BUILD="$sha-$built_at"
notes=$(git log -1 --format=%s)

nice -n 10 cargo xwin build --release --target "$target"

exe="target/$target/release/mailclient.exe"
[[ -f $exe ]] || { echo "brak $exe" >&2; exit 1; }

mkdir -p "$out"
name="Poczta-$POCZTA_BUILD.exe"
cp "$exe" "$out/$name.part" && mv "$out/$name.part" "$out/$name"
sum=$(sha256sum "$out/$name" | cut -d' ' -f1)
size=$(stat -c %s "$out/$name")
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | tr -d '\r' | head -1)
jq -n --arg version "$version" --arg build "$POCZTA_BUILD" --argjson built_at "$built_at" \
  --arg sha256 "$sum" --arg url "$name" --argjson size "$size" --arg notes "$notes" \
  '{version:$version, build:$build, built_at:$built_at, sha256:$sha256, url:$url, size:$size, notes:$notes}' \
  > "$out/latest.json.part"
mv "$out/latest.json.part" "$out/latest.json"

# keep the newest $keep builds
ls -1t "$out"/Poczta-*.exe 2>/dev/null | tail -n +"$((keep + 1))" | while read -r old; do rm -f -- "$old"; done

echo "opublikowano $name ($((size / 1024)) KiB) → http://100.100.1.2:8790/mailrust/latest.json"
