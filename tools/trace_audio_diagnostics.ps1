param(
    [string]$Output = 'target/audio-diagnostics-trace',
    [ValidateRange(1, 30)][int]$Seconds = 2,
    [ValidateRange(1, 10)][int]$Cycles = 2
)

$ErrorActionPreference = 'Stop'
$workspace = Split-Path -Parent $PSScriptRoot
Set-Location -LiteralPath $workspace
$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = [Security.Principal.WindowsPrincipal]::new($identity)
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'ETW tracing requires an elevated PowerShell. Run this script as administrator.'
}
$binary = Join-Path $workspace 'target/debug/audio_diagnostics.exe'
if (-not (Test-Path -LiteralPath $binary)) {
    throw 'Build the probe first: cargo build --bin audio_diagnostics'
}
New-Item -ItemType Directory -Path $Output -Force | Out-Null
$outputPath = (Resolve-Path -LiteralPath $Output).Path
$session = 'VoiceBoxAudioDiagnostic-' + $PID
$etl = Join-Path $outputPath 'apo.etl'
$runnerLog = Join-Path $outputPath 'trace-run.log'
$started = Get-Date
$traceStarted = $false
$testExit = 2
$providers = Join-Path $outputPath 'providers.txt'
@(
    '{d75e13f3-18c2-45f2-9397-9a6641423354} 0xffffffffffffffff 5',
    '{ae4bd3be-f36f-45b6-8d21-bdd6fb832853} 0xffffffffffffffff 5'
) | Set-Content -LiteralPath $providers -Encoding ascii
try {
    & logman create trace $session -pf $providers -o $etl -ets *> $runnerLog
    if ($LASTEXITCODE -ne 0) { throw "logman start failed: $LASTEXITCODE" }
    $traceStarted = $true
    & $binary --output $outputPath --seconds $Seconds --cycles $Cycles *>> $runnerLog
    $testExit = $LASTEXITCODE
}
finally {
    if ($traceStarted) {
        & logman stop $session -ets *>> $runnerLog
        & tracerpt $etl -of XML -o (Join-Path $outputPath 'apo.xml') -y *>> $runnerLog
    }
    Get-WinEvent -FilterHashtable @{ LogName = 'Application'; StartTime = $started.AddMinutes(-30); Id = 1000,1001 } -ErrorAction SilentlyContinue |
        Where-Object { $_.Message -match 'audiodg|SoundRecorder|voice_box|audio_diagnostics' } |
        Format-List TimeCreated, Id, Message |
        Out-File -LiteralPath (Join-Path $outputPath 'application-events.txt') -Encoding utf8
    "Diagnostic exit code: $testExit" | Add-Content -LiteralPath $runnerLog
}
exit $testExit
