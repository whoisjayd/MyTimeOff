# Puts this project's pinned Node (see .node-version) and pnpm on PATH.
# Usage:  . .\scripts\dev-env.ps1
$ErrorActionPreference = "Stop"

$version = (Get-Content "$PSScriptRoot\..\.node-version").Trim()
$install = "$env:APPDATA\fnm\node-versions\v$version*\installation"
$resolved = (Resolve-Path $install -ErrorAction SilentlyContinue | Select-Object -Last 1).Path

if (-not $resolved) {
  Write-Error "Node $version is not installed under fnm. Run: fnm install $version"
}

$env:Path = "$resolved;$resolved\node_modules\npm\bin;$env:Path"
& "$resolved\corepack.cmd" prepare --activate 2>&1 | Out-Null

Write-Host "node $(node -v) / pnpm $(pnpm --version)" -ForegroundColor Green
