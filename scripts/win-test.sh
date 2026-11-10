#!/usr/bin/env bash
# Tests Poczta on the Windows laptop over Tailscale + SSH, from this Linux machine. The access
# (OpenSSH on the laptop, this machine's key) is described in Hindsight: "Dostęp SSH z NUC do
# laptopa Windows". Each run starts the app on the laptop's desktop through a scheduled task,
# on a copy of the mailbox, offline, closes it after the given time, and brings back its log and
# a CPU / memory trace.
#
#   scripts/win-test.sh build                 # build the EXE here and send it (with win-run.cmd, win-probe.ps1)
#   scripts/win-test.sh fresh                 # copy the real mailbox for the tests (Poczta closed!)
#   scripts/win-test.sh run <ms> [VAR=value]  # one run, closed after <ms>; extra variables for the app
#                                             # (POCZTA_ARGS=--minimized: arguments for the EXE)
#   scripts/win-test.sh probe <ms> [VAR=value] # the same run, watched by win-probe.ps1: proves it stayed unseen
#
# The laptop is the owner's working machine, so runs are INVISIBLE by default: every window of the
# test copy is DWM-cloaked (drawn as usual, never shown, never active, no taskbar button), it has
# no tray icon, sound or new-mail note, and the task's console is "conhost --headless". The app
# does this for --screenshot and for a run with POCZTA_CLOSE_AFTER_MS and its own POCZTA_DATA_DIR
# (this script sets the first, win-run.cmd the second). VISIBLE=1 scripts/win-test.sh run ...
# shows everything as before (tray tests with --minimized / --tray need it); keep such
# runs few and short. `probe` writes probe.txt; its last line says NIEWIDOCZNY or not, and
# "SESJA ZABLOKOWANA" when the screen was locked (then it proves nothing about the foreground).
# A run still going after <ms> + 2 min is stopped (the test copy, found by its path).
# `run` prints how long the run took on the laptop (win-run.cmd's start to done.txt).
# POCZTA_ARGS=--screenshot <WDIR>\shot.png: the picture comes back as shot.png; give <ms> longer
# than the shot (e.g. 20000), or the run closes before it. A picture ignores the test folder's
# saved window (app.ron), so it always has the default size.
# CPU seconds of invisible runs are lower than of the older visible ones and vary a lot from run
# to run: compare only runs made the same way, back to back, best of 2-3.
#
# Results: ~/mailrust-tests/<time>/ (sync.log of the run, cpu-mem.csv, exit.txt, czas.txt). Env:
# WIN (the user@address of the laptop), WDIR (the test folder there, e.g. C:\Users\<name>\poczta-test);
# both usually in ~/.config/poczta/build.env (see scripts/publish.sh). win-run.cmd deletes env.cmd
# once read, so the tasks' leftover 23:59 triggers do nothing.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/.." && pwd)
conf=${POCZTA_CONF:-${XDG_CONFIG_HOME:-$HOME/.config}/poczta/build.env}
if [[ -f $conf ]]; then set -a; . "$conf"; set +a; fi
win=${WIN:?brak WIN (plik $conf)}
wdir=${WDIR:?brak WDIR (plik $conf)}
wdir_scp=${wdir//\\//}
target=x86_64-pc-windows-msvc
ps() { ssh -o BatchMode=yes -o ConnectTimeout=10 "$win" "powershell -NoProfile -Command \"\$ErrorActionPreference = 'Stop'; $1\""; }

case ${1:-} in
build)
  cd "$repo"
  nice -n 10 cargo xwin build --release --target "$target"
  ps "New-Item -ItemType Directory -Force '$wdir\\data' | Out-Null"
  scp -q "target/$target/release/mailclient.exe" "$win:$wdir_scp/mailclient.exe"
  # cmd needs CRLF line ends for its labels and blocks
  sed 's/\r*$/\r/' scripts/win-run.cmd > "/tmp/win-run.$$.cmd"
  scp -q "/tmp/win-run.$$.cmd" "$win:$wdir_scp/win-run.cmd" && rm -f "/tmp/win-run.$$.cmd"
  scp -q scripts/win-probe.ps1 "$win:$wdir_scp/win-probe.ps1"
  # conhost --headless: the task's console never shows on the owner's screen
  ssh -o BatchMode=yes "$win" "schtasks /create /tn PocztaTest /tr \"conhost.exe --headless cmd.exe /c $wdir\\win-run.cmd $wdir\" /sc once /st 23:59 /it /f" >/dev/null
  echo "wysłano $(git rev-parse --short HEAD)$(git diff --quiet HEAD || echo -dirty) i zadanie PocztaTest"
  ;;
fresh)
  ps "if (Get-Process mailclient -ErrorAction SilentlyContinue) { throw 'Poczta działa: zamknij ją (Zakończ) przed kopiowaniem' }; Copy-Item \$env:LOCALAPPDATA\\Poczta\\mail.redb '$wdir\\data\\mail.redb' -Force; Remove-Item '$wdir\\data\\sync.log' -ErrorAction SilentlyContinue; 'skopiowano ' + [int]((Get-Item '$wdir\\data\\mail.redb').Length/1MB) + ' MB'"
  ;;
run)
  ms=${2:?czas do zamknięcia w ms}
  # anything else would start a visible copy that never closes (the app reads only a number)
  [[ $ms =~ ^[0-9]+$ ]] || { echo "czas w ms: sama liczba" >&2; exit 1; }
  shift 2
  out=${OUT:-~/mailrust-tests/$(date +%Y%m%d-%H%M%S)}
  mkdir -p "$out"
  { echo "set POCZTA_CLOSE_AFTER_MS=$ms"; [[ ${VISIBLE:-} != 1 ]] || echo "set POCZTA_VISIBLE=1"; for v in "$@"; do echo "set \"$v\""; done; } | sed 's/$/\r/' > "$out/env.cmd"
  # an env.cmd the task never used (it did not start, or this script failed, even inside scp) would run at 23:59
  trap "ps \"Remove-Item '$wdir\\\\env.cmd' -ErrorAction SilentlyContinue\" || true" EXIT
  scp -q "$out/env.cmd" "$win:$wdir_scp/env.cmd"
  ps "'done.txt','start.txt','shot.png' | ForEach-Object { Join-Path '$wdir' \$_ } | Where-Object { Test-Path \$_ } | Remove-Item; \$n = (Get-Content '$wdir\\data\\sync.log' -ErrorAction SilentlyContinue | Measure-Object -Line).Lines; Set-Content '$wdir\\log-start.txt' ([int]\$n)"
  ssh -o BatchMode=yes "$win" "schtasks /run /tn PocztaTest" >/dev/null
  # CPU seconds and memory every second until the run ends (at most the given time + 2 min); a test
  # copy still running then has no window to close it by, so it is stopped (by its path, never by name)
  limit=$(( ms / 1000 + 120 ))
  ps "\$end = (Get-Date).AddSeconds($limit); 'czas_s,cpu_s,ram_mb'; \$t0 = Get-Date; while (-not (Test-Path '$wdir\\done.txt') -and (Get-Date) -lt \$end) { \$p = Get-Process mailclient -ErrorAction SilentlyContinue | Where-Object { \$_.Path -like '$wdir*' } | Select-Object -First 1; if (\$p) { [string]::Format([Globalization.CultureInfo]::InvariantCulture, '{0:F1},{1:F2},{2:F0}', ((Get-Date) - \$t0).TotalSeconds, \$p.CPU, (\$p.WorkingSet64/1MB)) }; Start-Sleep -Milliseconds 1000 }; if (-not (Test-Path '$wdir\\done.txt')) { 'NIE ZAKONCZONO W CZASIE'; Get-Process mailclient -ErrorAction SilentlyContinue | Where-Object { \$_.Path -eq '$wdir\\mailclient.exe' } | ForEach-Object { Stop-Process -Id \$_.Id -Force; 'zatrzymano kopie testowa pid ' + \$_.Id } }" | tr -d '\r' > "$out/cpu-mem.csv"
  ps "\$s = [int](Get-Content '$wdir\\log-start.txt'); Get-Content '$wdir\\data\\sync.log' | Select-Object -Skip \$s" | tr -d '\r' > "$out/sync.log"
  ps "Get-Content '$wdir\\done.txt' -ErrorAction SilentlyContinue" | tr -d '\r' > "$out/exit.txt" || true
  scp -q "$win:$wdir_scp/shot.png" "$out/shot.png" 2>/dev/null || true
  echo "== $out (kod wyjścia: $(tr -d ' \n' < "$out/exit.txt"))"
  grep -aE "startup:|database: (file opened|opened)|closing:|ERROR|search index built" "$out/sync.log" || true
  ps "if ((Test-Path '$wdir\\start.txt') -and (Test-Path '$wdir\\done.txt')) { [string]::Format([Globalization.CultureInfo]::InvariantCulture, 'czas przebiegu na laptopie: {0:F1} s', ((Get-Item '$wdir\\done.txt').LastWriteTime - (Get-Item '$wdir\\start.txt').LastWriteTime).TotalSeconds) } else { 'czas przebiegu na laptopie: nieznany (brak start.txt lub done.txt)' }" | tr -d '\r' | tee "$out/czas.txt"
  tail -n 3 "$out/cpu-mem.csv"
  ;;
probe)
  ms=${2:?czas do zamknięcia w ms}
  # anything else would start a visible copy that never closes (the app reads only a number)
  [[ $ms =~ ^[0-9]+$ ]] || { echo "czas w ms: sama liczba" >&2; exit 1; }
  out=~/mailrust-tests/$(date +%Y%m%d-%H%M%S)-sonda
  mkdir -p "$out"
  scp -q "$here/win-probe.ps1" "$win:$wdir_scp/win-probe.ps1"
  # left behind (also when the run fails, or ssh drops after the task was made), the task would
  # start the probe again at 23:59
  trap 'ssh -o BatchMode=yes "$win" "schtasks /delete /tn PocztaProbe /f" >/dev/null 2>&1 || true' EXIT
  # hidden like the run itself; it watches for at most the run's limit + the wait for it to get ready
  ssh -o BatchMode=yes "$win" "schtasks /create /tn PocztaProbe /tr \"conhost.exe --headless powershell.exe -NoProfile -ExecutionPolicy Bypass -File $wdir\\win-probe.ps1 $wdir $(( ms / 1000 + 150 ))\" /sc once /st 23:59 /it /f" >/dev/null
  ps "'probe.txt','probe-ready.txt','probe-done.txt' | ForEach-Object { Join-Path '$wdir' \$_ } | Where-Object { Test-Path \$_ } | Remove-Item"
  ssh -o BatchMode=yes "$win" "schtasks /run /tn PocztaProbe" >/dev/null
  ps "\$end = (Get-Date).AddSeconds(60); while (-not (Test-Path '$wdir\\probe-ready.txt') -and (Get-Date) -lt \$end) { Start-Sleep -Milliseconds 300 }; if (-not (Test-Path '$wdir\\probe-ready.txt')) { 'sonda nie wystartowala'; exit 1 }"
  OUT=$out "$0" run "${@:2}"
  ps "\$end = (Get-Date).AddSeconds(30); while (-not (Test-Path '$wdir\\probe-done.txt') -and (Get-Date) -lt \$end) { Start-Sleep -Milliseconds 300 }"
  scp -q "$win:$wdir_scp/probe.txt" "$out/probe.txt" && tr -d '\r' < "$out/probe.txt" | tail -n 1
  ;;
*)
  sed -n '2,33p' "$0"
  exit 1
  ;;
esac
