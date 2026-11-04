@echo off
rem Runs one test of Poczta in the signed-in user's session (started by scripts/win-test.sh through
rem a scheduled task: programs started straight from SSH have no desktop).
rem   win-run.cmd <folder>
rem <folder>\env.cmd (written by win-test.sh) sets the test's variables; whatever it sets, the test
rem uses its own copy of the mailbox (POCZTA_DATA_DIR), doesn't connect anywhere (POCZTA_OFFLINE)
rem and doesn't run the search, whose meaning files live in the real profile (POCZTA_NO_SEARCH).
setlocal
if exist "%~1\env.cmd" call "%~1\env.cmd"
set "DIR=%~1"
set "POCZTA_DATA_DIR=%DIR%\data"
set POCZTA_OFFLINE=1
set POCZTA_NO_SEARCH=1
cd /d "%DIR%"
del /q "%DIR%\done.txt" 2>nul
"%DIR%\mailclient.exe"
echo %ERRORLEVEL%>"%DIR%\done.txt"
