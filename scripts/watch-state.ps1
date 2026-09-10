<#
.SYNOPSIS
  Prints every state change the daemon makes, so a live agent turn is observable.

.DESCRIPTION
  Run this in its own terminal while Claude Code drives the hooks. Polling is fine
  here: this is an observation tool, not part of the product. The daemon itself
  never polls - it sleeps to the grace deadline.
#>
[CmdletBinding()]
param(
    [int]$Port = 8787,
    # Invoke-RestMethod opens a fresh connection per poll, so a tighter interval just
    # piles up TIME_WAIT sockets. Every transition worth seeing is seconds-scale.
    [int]$IntervalMs = 1000
)

$ErrorActionPreference = 'Stop'

$tokenPath = Join-Path $env:LOCALAPPDATA 'MyTimeOff\hook-token'
if (-not (Test-Path $tokenPath)) {
    throw "No token at $tokenPath. Start the daemon once to create it."
}
$token = (Get-Content $tokenPath -Raw).Trim()
$headers = @{ Authorization = "Bearer $token" }
$uri = "http://127.0.0.1:$Port/state"

Write-Host "watching $uri  (Ctrl+C to stop)" -ForegroundColor DarkGray

$last = $null
$seen = 0
while ($true) {
    try {
        $report = Invoke-RestMethod -Uri $uri -Headers $headers -TimeoutSec 5
    } catch {
        # The daemon may not be up yet, or may be restarting. Keep watching.
        if ($last -ne 'down') {
            Write-Host ("{0}  daemon unreachable" -f (Get-Date -Format 'HH:mm:ss')) -ForegroundColor DarkYellow
            $last = 'down'
        }
        Start-Sleep -Milliseconds $IntervalMs
        continue
    }

    # Written without ?? so this runs under Windows PowerShell 5.1 as well as pwsh.
    $sess  = if ($report.session) { $report.session } else { '-' }
    $alert = if ($report.alert)   { $report.alert }   else { '-' }
    $line  = '{0,-8} session={1,-40} alert={2}' -f $report.state, $sess, $alert

    # Deliveries first: a hook that never arrived and one that arrived and was correctly
    # ignored look identical from the state alone.
    if ($report.received.Count -lt $seen) { $seen = 0 }   # daemon restarted
    if ($report.received.Count -gt $seen) {
        foreach ($entry in $report.received[$seen..($report.received.Count - 1)]) {
            $arrow = if ($entry -match '-> ignored$') { 'DarkGray' } else { 'White' }
            Write-Host ("          recv: {0}" -f $entry) -ForegroundColor $arrow
        }
        $seen = $report.received.Count
    }

    if ($line -ne $last) {
        $color = switch ($report.state) {
            'reading' { 'Cyan' }
            'ready'   { if ($report.alert -eq 'needs_input') { 'Yellow' } else { 'Green' } }
            'gate'    { 'Magenta' }
            default   { 'Gray' }
        }
        Write-Host ("{0}  {1}" -f (Get-Date -Format 'HH:mm:ss'), $line) -ForegroundColor $color
        if ($report.issued.Count -gt 0) {
            Write-Host ("          issued: {0}" -f ($report.issued -join ' -> ')) -ForegroundColor DarkGray
        }
        $last = $line
    }
    Start-Sleep -Milliseconds $IntervalMs
}
