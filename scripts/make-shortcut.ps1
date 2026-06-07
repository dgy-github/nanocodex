# Creates a desktop shortcut for the nanocodex GUI, with an optional hotkey.
#
# Why this exists: double-clicking the .cmd flashes a console window first.
# This shortcut targets pythonw.exe (no console) so the GUI opens cleanly, like
# a normal desktop app. It also assigns a global hotkey (Ctrl+Alt+N) that works
# once the shortcut lives on the Desktop or in the Start Menu.
#
# Prereq: run `pip install -e .` in the nanocodex project once, so the
# `nanocodex` package is importable by pythonw.
#
# Usage (from the nanocodex project folder, in PowerShell):
#   powershell -ExecutionPolicy Bypass -File scripts\make-shortcut.ps1
#   powershell -ExecutionPolicy Bypass -File scripts\make-shortcut.ps1 -Workdir "D:\my-project"
#   powershell -ExecutionPolicy Bypass -File scripts\make-shortcut.ps1 -StartMenu

param(
    # Workspace the GUI opens in. EMPTY by default: the shortcut then omits
    # --cd, so the GUI reopens your last project (remembered across launches),
    # falling back to the current directory the first time. Set -Workdir to pin
    # a fixed folder instead.
    [string]$Workdir = "",
    # Hotkey, in WScript.Shell format. "" disables the hotkey.
    [string]$Hotkey = "CTRL+ALT+N",
    # Place the shortcut in the Start Menu instead of (well, in addition to) Desktop.
    [switch]$StartMenu,
    [string]$Name = "nanocodex"
)

$ErrorActionPreference = "Stop"

# Locate pythonw.exe next to the active python.exe.
$python = (Get-Command python -ErrorAction SilentlyContinue).Source
if (-not $python) {
    Write-Error "python.exe not found on PATH. Install Python or activate your venv first."
    exit 1
}
$pythonw = Join-Path (Split-Path $python) "pythonw.exe"
if (-not (Test-Path $pythonw)) {
    Write-Warning "pythonw.exe not found; falling back to python.exe (a console window will flash)."
    $pythonw = $python
}

# Verify the package is importable, so the shortcut won't silently fail.
& $python -c "import nanocodex.gui" 2>$null
if ($LASTEXITCODE -ne 0) {
    Write-Warning "Could not import nanocodex.gui. Run 'pip install -e .' in the project first, or the shortcut will do nothing."
}

$shell = New-Object -ComObject WScript.Shell

function New-NanocodexShortcut([string]$dir) {
    if (-not (Test-Path $dir)) { New-Item -ItemType Directory -Path $dir -Force | Out-Null }
    $lnkPath = Join-Path $dir "$Name.lnk"
    $sc = $shell.CreateShortcut($lnkPath)
    $sc.TargetPath = $pythonw
    if ([string]::IsNullOrEmpty($Workdir)) {
        # No fixed folder: omit --cd so the GUI reopens the last project.
        $sc.Arguments = "-m nanocodex.gui"
        $sc.WorkingDirectory = $env:USERPROFILE
    } else {
        $sc.Arguments = "-m nanocodex.gui --cd `"$Workdir`""
        $sc.WorkingDirectory = $Workdir
    }
    $sc.Description = "nanocodex desktop (Codex-style coding agent)"
    $sc.IconLocation = "$pythonw,0"
    if ($Hotkey -ne "") { $sc.Hotkey = $Hotkey }
    $sc.Save()
    Write-Host "Created shortcut: $lnkPath"
}

$desktop = [Environment]::GetFolderPath("Desktop")
New-NanocodexShortcut $desktop

if ($StartMenu) {
    $startDir = Join-Path $env:APPDATA "Microsoft\Windows\Start Menu\Programs"
    New-NanocodexShortcut $startDir
}

Write-Host ""
$hotkeyNote = ""
if (-not [string]::IsNullOrEmpty($Hotkey)) { $hotkeyNote = ", or press $Hotkey" }
Write-Host "Done. Double-click '$Name' on your Desktop$hotkeyNote."
if ([string]::IsNullOrEmpty($Workdir)) {
    Write-Host "Workspace: remembers your last opened project (use the 'Open project' button in the window)."
} else {
    Write-Host "Workspace pinned to: $Workdir  (re-run without -Workdir to remember the last project instead)."
}
