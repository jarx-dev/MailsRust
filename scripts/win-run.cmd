@echo off
rem Runs one test of Poczta in the signed-in user's session (started by scripts/win-test.sh through
rem a scheduled task: programs started straight from SSH have no desktop).
rem   win-run.cmd <folder>
rem <folder>\env.cmd (written by win-test.sh) sets the test's variables; the test always uses its
rem own copy of the mailbox (POCZTA_DATA_DIR) and doesn't connect anywhere (POCZTA_OFFLINE).
setlocal
set "DIR=%~1"
set "POCZTA_DATA_DIR=%DIR%\data"
set POCZTA_OFFLINE=1
if exist "%DIR%\env.cmd" call "%DIR%\env.cmd"
cd /d "%DIR%"
del /q "%DIR%\done.txt" 2>nul
"%DIR%\mailclient.exe"
echo %ERRORLEVEL%>"%DIR%\done.txt"
