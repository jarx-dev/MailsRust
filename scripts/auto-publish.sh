#!/usr/bin/env bash
# Publishes every new commit on main for the app's self-updater, so a merged PR reaches the
# laptops without a manual build. Run every minute by the systemd user timer
# mailrust-autopublish: it fetches origin/main into a worktree of its own (~/mailrust-publish,
# so it never touches the tree being worked in) and, when that commit is not the build in
# latest.json yet, builds and publishes it with scripts/publish.sh. A commit whose build failed
# is not tried again (the next commit on main is).
#
#   scripts/auto-publish.sh --install   # worktree + timer (once, from the main tree)
#   journalctl --user -u mailrust-autopublish -f
#
# Env: POCZTA_PUBLISH_TREE (default ~/mailrust-publish), POCZTA_UPDATE_DIR (as publish.sh).
set -euo pipefail

repo=$(cd "$(dirname "$0")/.." && pwd)
tree=${POCZTA_PUBLISH_TREE:-$HOME/mailrust-publish}
out=${POCZTA_UPDATE_DIR:-$HOME/hyperrust-updates/mailrust}
failed=${XDG_STATE_HOME:-$HOME/.local/state}/mailrust-autopublish-failed

if [[ ${1:-} == --install ]]; then
  # (plain git: the shell's git wrapper moves new worktrees under ~/.t3, where they get cleaned up)
  [[ -d $tree ]] || /usr/bin/git -C "$repo" worktree add -q --detach "$tree" origin/main
  # the same Rust as the main tree (its version is set per folder with rustup)
  rustup override set --path "$tree" "$(cd "$repo" && rustup show active-toolchain | cut -d' ' -f1)"
  units=$HOME/.config/systemd/user
  mkdir -p "$units"
  cat > "$units/mailrust-autopublish.service" <<EOF
[Unit]
Description=Poczta: build and publish new commits on main (scripts/auto-publish.sh)
After=network-online.target

[Service]
Type=oneshot
Environment=PATH=%h/.cargo/bin:/usr/local/bin:/usr/bin:/bin
ExecStart=$tree/scripts/auto-publish.sh
Nice=10
EOF
  cat > "$units/mailrust-autopublish.timer" <<'EOF'
[Unit]
Description=Poczta: check main for a new build every minute

[Timer]
OnBootSec=2min
OnUnitInactiveSec=1min

[Install]
WantedBy=timers.target
EOF
  export XDG_RUNTIME_DIR=${XDG_RUNTIME_DIR:-/run/user/$(id -u)}
  systemctl --user daemon-reload
  systemctl --user enable --now mailrust-autopublish.timer
  echo "zainstalowano: $tree, mailrust-autopublish.timer"
  exit 0
fi

cd "$tree"
git fetch -q origin main
sha=$(git rev-parse --short origin/main)
published=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["build"].split("-")[0])' "$out/latest.json" 2>/dev/null || true)
[[ $sha == "$published" ]] && exit 0
[[ -f $failed && $(cat "$failed") == "$sha" ]] && exit 0
git checkout -q --detach origin/main
echo "nowy commit na main: $sha (opublikowany: ${published:-brak}), buduję"
if ! scripts/publish.sh; then
  mkdir -p "$(dirname "$failed")"
  echo "$sha" > "$failed"
  echo "build $sha nieudany, nie będzie ponawiany" >&2
  exit 1
fi
