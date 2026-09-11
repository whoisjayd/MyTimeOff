<#
.SYNOPSIS
  Builds the `mytimeoff` command line and puts it where the installer will find it.

.DESCRIPTION
  The window and the command line cannot both sit in the install directory: Windows
  filenames are case-insensitive, so `MyTimeOff.exe` and `mytimeoff.exe` are one file,
  and whichever the installer writes second silently replaces the first. The command
  line therefore installs into a `bin` subdirectory - the same shape VS Code uses, and
  the reason `...\Microsoft VS Code\bin` shows up on so many PATHs. `bin` is what goes
  on PATH; the window stays at the top where people look for it.

  Tauri copies it there as a bundle resource, which unlike an externalBin sidecar does
  not demand the Rust target triple in the filename. It does still have to be on disk
  before the shell crate builds at all - tauri-build checks every declared resource - so
  run this once after a fresh clone or `cargo build -p mytimeoff-shell` will stop with
  "resource path binaries\mytimeoff.exe doesn't exist".

  `tauri build` runs this itself through `beforeBuildCommand`, so there is normally no
  reason to call it by hand.
#>
[CmdletBinding()]
param(
    # `tauri build` produces a release bundle; anything else is for trying the plumbing.
    # Deliberately not named after the cargo profile: that name is an automatic
    # PowerShell variable, and shadowing it is a trap for whoever edits this next.
    [ValidateSet('release', 'debug')]
    [string]$Configuration = 'release'
)

$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $PSScriptRoot
$destination = Join-Path $root 'crates\shell\binaries'

Write-Host "building the mytimeoff command line ($Configuration)" -ForegroundColor Cyan
$arguments = @('build', '-p', 'mytimeoff-daemon', '--bin', 'mytimeoff')
if ($Configuration -eq 'release') { $arguments += '--release' }
& cargo @arguments
if ($LASTEXITCODE -ne 0) { throw "cargo build failed with $LASTEXITCODE" }

$target = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $root 'target' }
$built = Join-Path $target "$Configuration\mytimeoff.exe"
if (-not (Test-Path $built)) { throw "cargo said it succeeded but $built is not there" }

New-Item -ItemType Directory -Force -Path $destination | Out-Null
$staged = Join-Path $destination 'mytimeoff.exe'
Copy-Item $built $staged -Force

Write-Host "-> $staged" -ForegroundColor Green
