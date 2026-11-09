#!/usr/bin/env bash
# Publishes a GitHub release of origin/main: Poczta-<build>.exe + latest.json, built by publish.sh
# without this machine's private addresses. Copies from a release update themselves from the
# newest release (releases/latest/download/latest.json). The tag is the version the app shows
# in ☰ → „O programie” (v26.1109.1215: year, month and day, hour and minute).
#
#   scripts/release-github.sh [notes.md]     # release notes from a file, else from the commits
set -euo pipefail

repo=$(cd "$(dirname "$0")/.." && pwd)
gh_repo=jarx-dev/MailsRust
cd "$repo"
git fetch -q origin main
[[ $(git rev-parse HEAD) == $(git rev-parse origin/main) ]] || { echo "HEAD to nie origin/main" >&2; exit 1; }
git diff --quiet HEAD || { echo "niezacommitowane zmiany" >&2; exit 1; }

out=$(mktemp -d)
POCZTA_CONF=/dev/null POCZTA_UPDATE_DIR="$out" \
  POCZTA_DEFAULT_UPDATE_URL="https://github.com/$gh_repo/releases/latest/download/latest.json" \
  scripts/publish.sh
exe=$(ls "$out"/Poczta-*.exe)
# the repository is public: nothing from the private network may be built in
if grep -aqE 'http://100\.[0-9]+\.[0-9]+\.[0-9]+' "$exe"; then echo "EXE zawiera prywatny adres: $exe" >&2; exit 1; fi

tag=$(date -d "@$(jq -r .built_at "$out/latest.json")" +v%y.%m%d.%H%M)
notes=(--generate-notes)
[[ -n ${1:-} ]] && notes=(--notes-file "$1")
gh release create "$tag" -R "$gh_repo" --target "$(git rev-parse HEAD)" --title "Poczta $tag" "${notes[@]}" \
  "$exe" "$out/latest.json"
echo "wydano $tag: https://github.com/$gh_repo/releases/tag/$tag"
