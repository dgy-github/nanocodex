param(
    [string]$Target = "x86_64-pc-windows-gnu",
    [string]$Cargo = "",
    [switch]$SkipFmt,
    [switch]$SkipTauri
)

$ErrorActionPreference = "Stop"

function Find-Executable {
    param(
        [string]$Name,
        [string]$ExplicitPath
    )
    if (-not [string]::IsNullOrWhiteSpace($ExplicitPath)) {
        if (Test-Path -LiteralPath $ExplicitPath) {
            return (Resolve-Path -LiteralPath $ExplicitPath).Path
        }
        throw "$Name was passed explicitly but was not found: $ExplicitPath"
    }

    $cmd = Get-Command $Name -ErrorAction SilentlyContinue
    if ($cmd) {
        return $cmd.Source
    }

    $candidates = @()
    if ($env:CARGO_HOME) {
        $candidates += (Join-Path $env:CARGO_HOME "bin\$Name.exe")
    }
    if ($env:USERPROFILE) {
        $candidates += (Join-Path $env:USERPROFILE ".cargo\bin\$Name.exe")
        $candidates += (Join-Path $env:USERPROFILE "scoop\shims\$Name.exe")
    }
    foreach ($path in $candidates) {
        if (Test-Path -LiteralPath $path) {
            return (Resolve-Path -LiteralPath $path).Path
        }
    }

    throw @"
$Name was not found on PATH or in common Windows Rust locations.
Install Rust with rustup, then reopen PowerShell:
  winget install Rustlang.Rustup
  rustup target add $Target
"@
}

function Invoke-Step {
    param(
        [string]$Name,
        [scriptblock]$Block
    )
    Write-Host ""
    Write-Host "==> $Name"
    & $Block
}

$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$rustManifest = Join-Path $repoRoot "rust\Cargo.toml"
$tauriManifest = Join-Path $repoRoot "rust\gui\src-tauri\Cargo.toml"
$cargoExe = Find-Executable -Name "cargo" -ExplicitPath $Cargo

Write-Host "cargo: $cargoExe"
Write-Host "target: $Target"

if (-not $SkipFmt) {
    Invoke-Step "Rust fmt check" {
        & $cargoExe fmt --manifest-path $rustManifest --all --check
    }
}

Invoke-Step "Rust workspace tests" {
    & $cargoExe test --manifest-path $rustManifest --workspace --target $Target
}

if (-not $SkipTauri) {
    if (-not $SkipFmt) {
        Invoke-Step "Tauri backend fmt check" {
            & $cargoExe fmt --manifest-path $tauriManifest --all --check
        }
    }
    Invoke-Step "Tauri backend check" {
        & $cargoExe check --manifest-path $tauriManifest --target $Target
    }
}

Write-Host ""
Write-Host "Rust verification complete."
