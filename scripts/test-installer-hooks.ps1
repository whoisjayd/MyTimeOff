<#
.SYNOPSIS
  Compiles and runs crates/shell/tests/path-hooks.nsi, which tests the installer's PATH
  editing.

.DESCRIPTION
  The installer adds its own directory to the user's PATH and takes it out again on
  uninstall. That is the one thing in this project that can damage a machine it did not
  install anything on, and NSIS has no test runner, so this is it: compile the real hook
  file, run the cases, fail on anything that is not "ok".

  makensis comes from wherever Tauri put it, so no separate NSIS install is needed - but
  that means `pnpm tauri build` has to have run at least once on this machine.
#>
[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $PSScriptRoot
$script = Join-Path $root 'crates\shell\tests\path-hooks.nsi'

$onPath = Get-Command makensis -ErrorAction SilentlyContinue
$makensis = if ($onPath) { $onPath.Source } else { Join-Path $env:LOCALAPPDATA 'tauri\NSIS\makensis.exe' }
if (-not (Test-Path $makensis)) {
    throw "no makensis on PATH or at $makensis - install it (e.g. ``choco install nsis``) or run ``pnpm tauri build`` once so Tauri fetches it"
}

$work = Join-Path ([System.IO.Path]::GetTempPath()) "mytimeoff-hooks-$PID"
New-Item -ItemType Directory -Force -Path $work | Out-Null
$exe = Join-Path $work 'path-hooks.exe'
try {
    # -V2 keeps the compile quiet; the warnings it does print are about the test harness
    # (an uninstaller section that exists only so the un. copies compile), not the hooks.
    # OUTPUT is absolute: NSIS resolves a relative OutFile against the script's own
    # directory, which is to say into the repository.
    & $makensis -V2 "-DOUTPUT=$exe" $script
    if ($LASTEXITCODE -ne 0) { throw "makensis failed with $LASTEXITCODE" }

    & $exe | Out-Null
    $results = Join-Path $work 'result.txt'
    if (-not (Test-Path $results)) { throw "the test ran but wrote no results" }

    $lines = Get-Content $results
    $lines | Write-Host
    $failed = $lines | Where-Object { $_ -like 'FAIL*' }
    if ($failed) { throw "$($failed.Count) case(s) failed" }
    if ($lines[-1] -ne 'done') { throw "the test stopped early" }

    Write-Host "installer PATH hooks: $($lines.Count - 1) cases passed" -ForegroundColor Green
}
finally {
    Remove-Item $work -Recurse -Force -ErrorAction SilentlyContinue
}
