@echo off
REM nanocodex desktop launcher (Windows).
REM Double-click to open the GUI in the current folder, or drag a folder onto
REM this file to use that folder as the workspace.
REM
REM Prerequisite: run `pip install -e .` in the nanocodex project once so the
REM `nanocodex-gui` command (and Python deps) are available.

setlocal

REM If a folder was dropped onto this script, %1 is that path; otherwise use CWD.
set "WORKDIR=%~1"
if "%WORKDIR%"=="" set "WORKDIR=%CD%"

REM Prefer the installed console script; fall back to `python -m`.
where nanocodex-gui >nul 2>nul
if %errorlevel%==0 (
    nanocodex-gui --cd "%WORKDIR%"
) else (
    python -m nanocodex.gui --cd "%WORKDIR%"
)

if %errorlevel% neq 0 (
    echo.
    echo nanocodex-gui exited with an error. Make sure you ran: pip install -e .
    pause
)

endlocal
