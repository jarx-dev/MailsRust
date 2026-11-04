#!/usr/bin/env bash
# Tests Poczta on the Windows laptop over Tailscale + SSH, from this Linux machine. The access
# (OpenSSH on the laptop, this machine's key) is described in Hindsight: "Dostęp SSH z NUC do
# laptopa Windows". Each run starts the app on the laptop's desktop through a scheduled task,
# on a copy of the mailbox, offline, closes it after the given time, and brings back its log and
# a CPU / memory trace.
#
#   scripts/win-test.sh build                 # build the EXE here and send it (with win-run.cmd)
#   scripts/win-test.sh fresh                 # copy the real mailbox for the tests (Poczta closed!)
#   scripts/win-test.sh run <ms> [VAR=value]  # one run, closed after <ms>; extra variables for the app
#
# Results: ~/mailrust-tests/<time>/ (sync.log of the run, cpu-mem.csv). Env: WIN (default
# jarek@100.100.1.3), WDIR (default C:\Users\user\poczta-test).
set -euo pipefail

repo=$(cd "$(dirname "$0")/.." && pwd)
win=${WIN:-jarek@100.100.1.3}
wdir=${WDIR:-'C:\Users\user\poczta-test'}
wdir_scp=${wdir//\\//}
target=x86_64-pc-windows-msvc
ps() { ssh -o BatchMode=yes "$win" "powershell -NoProfile -Command \"$1\""; }

case ${1:-} in
build)
  cd "$repo"
  nice -n 10 cargo xwin build --release --target "$target"
  ps "New-Item -ItemType Directory -Force '$wdir\\data' | Out-Null"
  scp -q "target/$target/release/mailclient.exe" "$win:$wdir_scp/mailclient.exe"
  # cmd needs CRLF line ends for its labels and blocks
  sed 's/\r*$/\r/' scripts/win-run.cmd > "/tmp/win-run.$$.cmd"
  scp -q "/tmp/win-run.$$.cmd" "$win:$wdir_scp/win-run.cmd" && rm -f "/tmp/win-run.$$.cmd"
  ssh -o BatchMode=yes "$win" "schtasks /create /tn PocztaTest /tr \"$wdir\\win-run.cmd $wdir\" /sc once /st 23:59 /it /f" >/dev/null
  echo "wysłano $(git rev-parse --short HEAD) i zadanie PocztaTest"
  ;;
fresh)
  ps "if (Get-Process mailclient -ErrorAction SilentlyContinue) { throw 'Poczta działa: zamknij ją (Zakończ) przed kopiowaniem' }; Copy-Item \$env:LOCALAPPDATA\\Poczta\\mail.redb '$wdir\\data\\mail.redb' -Force; Remove-Item '$wdir\\data\\sync.log' -ErrorAction SilentlyContinue; 'skopiowano ' + [int]((Get-Item '$wdir\\data\\mail.redb').Length/1MB) + ' MB'"
  ;;
run)
  ms=${2:?czas do zamknięcia w ms}
  shift 2
  out=~/mailrust-tests/$(date +%Y%m%d-%H%M%S)
  mkdir -p "$out"
  { echo "set POCZTA_CLOSE_AFTER_MS=$ms"; for v in "$@"; do echo "set \"$v\""; done; } | sed 's/$/\r/' > "$out/env.cmd"
  scp -q "$out/env.cmd" "$win:$wdir_scp/env.cmd"
  ps "Remove-Item '$wdir\\done.txt' -ErrorAction SilentlyContinue; \$n = (Get-Content '$wdir\\data\\sync.log' -ErrorAction SilentlyContinue | Measure-Object -Line).Lines; Set-Content '$wdir\\log-start.txt' ([int]\$n)"
  ssh -o BatchMode=yes "$win" "schtasks /run /tn PocztaTest" >/dev/null
  # CPU seconds and memory every second until the run ends (at most the given time + 2 min)
  limit=$(( ms / 1000 + 120 ))
  ps "\$end = (Get-Date).AddSeconds($limit); 'czas_s,cpu_s,ram_mb'; \$t0 = Get-Date; while (-not (Test-Path '$wdir\\done.txt') -and (Get-Date) -lt \$end) { \$p = Get-Process mailclient -ErrorAction SilentlyContinue | Where-Object { \$_.Path -like '$wdir*' } | Select-Object -First 1; if (\$p) { [string]::Format([Globalization.CultureInfo]::InvariantCulture, '{0:F1},{1:F2},{2:F0}', ((Get-Date) - \$t0).TotalSeconds, \$p.CPU, (\$p.WorkingSet64/1MB)) }; Start-Sleep -Milliseconds 1000 }; if (-not (Test-Path '$wdir\\done.txt')) { 'NIE ZAKOŃCZONO W CZASIE' }" | tr -d '\r' > "$out/cpu-mem.csv"
  ps "\$s = [int](Get-Content '$wdir\\log-start.txt'); Get-Content '$wdir\\data\\sync.log' | Select-Object -Skip \$s" | tr -d '\r' > "$out/sync.log"
  echo "== $out"
  grep -E "startup:|database: (file opened|opened)|closing:|ERROR|search index built" "$out/sync.log" || true
  tail -n 3 "$out/cpu-mem.csv"
  ;;
*)
  sed -n '2,15p' "$0"
  exit 1
  ;;
esac
