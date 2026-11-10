@echo off
rem Runs one test of Poczta in the signed-in user's session (started by scripts/win-test.sh through
rem a scheduled task: programs started straight from SSH have no desktop).
rem   win-run.cmd <folder>
rem <folder>\env.cmd (written by win-test.sh) sets the test's variables; whatever it sets, the test
rem uses its own copy of the mailbox (POCZTA_DATA_DIR), doesn't connect anywhere (POCZTA_OFFLINE)
rem and doesn't run the search, whose meaning files live in the real profile (POCZTA_NO_SEARCH).
rem The task starts this through "conhost --headless" (no console window); POCZTA_CLOSE_AFTER_MS with POCZTA_DATA_DIR
rem makes the app's windows unseen unless env.cmd sets POCZTA_VISIBLE=1. start.txt and done.txt
rem time the run.
setlocal
rem env.cmd is used up by its run: the task's leftover 23:59 trigger then finds none and does nothing
if not exist "%~1\env.cmd" exit /b 9
call "%~1\env.cmd"
del /q "%~1\env.cmd"
set "DIR=%~1"
set "POCZTA_DATA_DIR=%DIR%\data"
set POCZTA_OFFLINE=1
set POCZTA_NO_SEARCH=1
cd /d "%DIR%"
del /q "%DIR%\done.txt" 2>nul
echo.>"%DIR%\start.txt"
"%DIR%\mailclient.exe" %POCZTA_ARGS%
rem redirection first: "echo 0>file" would redirect handle 0 and leave done.txt empty
>"%DIR%\done.txt" echo %ERRORLEVEL%
