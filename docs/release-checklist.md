# Release Checklist

Use this checklist before cutting a nanocodex release or merging a release
branch. It is intentionally boring: the goal is to make the Rust release line
repeatable even when the local Windows machine is missing part of the toolchain.

## 1. Branch And Diff

- Work from the intended release branch, usually `rust-capability` or an
  integration branch based on it.
- Run `git status --short --branch` and keep unrelated untracked files out of
  the release commit.
- Review conflicts against active GUI/Rust integration branches before merging.
- Confirm README, README.zh-CN, and HANDOFF describe the current feature count,
  slash status surfaces, and verification gaps.

## 2. Local Verification

Preferred Windows command:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\verify-rust.ps1
```

If Rust is installed outside PATH:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\verify-rust.ps1 -Cargo C:\path\to\cargo.exe
```

The script checks Rust formatting, Rust workspace tests for
`x86_64-pc-windows-gnu`, and Tauri backend formatting/checks. If `cargo` is
missing, install Rust and the GNU target:

```powershell
winget install Rustlang.Rustup
rustup target add x86_64-pc-windows-gnu
```

Python offline tests:

```powershell
python -m pytest -q
```

## 3. Remote CI Gate

- Push the branch and wait for the GitHub Actions `CI` workflow.
- The Rust job runs on Windows, installs the MinGW linker, and verifies the
  `x86_64-pc-windows-gnu` target.
- The Python job runs the offline pytest suite on Ubuntu.
- Treat green CI as the release gate when the local Windows machine cannot run
  `cargo`.

## 4. Release Build

Preferred command:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\build-rust-release.ps1
```

Useful variants:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\build-rust-release.ps1 -Cargo C:\path\to\cargo.exe
powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\build-rust-release.ps1 -SkipTauri
```

Only use `-SkipTests` after the same target has passed locally or in CI.

## 5. Artifacts

Confirm `releases\` contains:

- `nanocodex-<version>-x86_64-pc-windows-gnu.zip`
- the Tauri NSIS installer `.exe`, unless this is a CLI-only release
- `SHA256SUMS.txt`
- `release-manifest.json`

Open `release-manifest.json` and verify package name, version, target, commit,
artifact paths, sizes, and SHA-256 hashes.

## 6. Smoke Checks

For the CLI package:

```powershell
.\releases\<unzipped>\ncx.exe --help
.\releases\<unzipped>\ncx.exe --dump-genome
```

For the Tauri installer:

- Install on a clean Windows profile or VM.
- Confirm the Settings panel opens on first launch when no API key is present.
- Confirm Sessions, Usage, Memory, and checkpoint panels open.
- Confirm the CLI status surfaces remain documented: `/budget`, `/context`,
  `/tools`, `/memory`, `/mcp`, and `/usage`.

## 7. Release Notes

Include:

- branch and commit SHA
- local verification command or CI run used as the release gate
- artifact names and SHA-256 hashes
- known gaps, especially any test skipped because a local toolchain was missing
- Claude Code / Fable gap calibration changes, if the README estimate moved
