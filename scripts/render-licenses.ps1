param(
    [Parameter(Mandatory = $true)][string]$Template,
    [Parameter(Mandatory = $true)][string]$OutputFile
)

$ErrorActionPreference = 'Stop'
$logPath = Join-Path (Get-Location) 'target/license-report/about.log'
$log = $null
$process = [System.Diagnostics.Process]::new()
$timer = [System.Diagnostics.Stopwatch]::StartNew()
$exitCode = 1
try {
    $log = [System.IO.StreamWriter]::new($logPath, $false)
    $log.AutoFlush = $true
    $process.StartInfo.FileName = (Get-Command cargo -CommandType Application | Select-Object -First 1).Source
    $process.StartInfo.Arguments = 'about generate --locked --fail --target x86_64-pc-windows-msvc "{0}" -o "{1}"' -f $Template, $OutputFile
    $process.StartInfo.UseShellExecute = $false
    $process.StartInfo.CreateNoWindow = $true
    $process.StartInfo.RedirectStandardError = $true
    Write-Host "[licenses] cargo-about: $Template (network verification may take time)"
    Write-Host "[licenses] Live diagnostics: $logPath"
    if (-not $process.Start()) { throw 'Failed to start cargo-about' }

    # Drain stderr asynchronously so a quiet network request still gets a heartbeat.
    # Progress goes to stdout; only cargo-about diagnostics enter the strict log check.
    $nextProgress = 5
    $lineTask = $process.StandardError.ReadLineAsync()
    while ($true) {
        if ($lineTask.Wait(1000)) {
            $line = $lineTask.Result
            if ($null -eq $line) { break }
            $log.WriteLine($line)
            Write-Host $line
            $lineTask = $process.StandardError.ReadLineAsync()
        }
        if ($timer.Elapsed.TotalSeconds -ge $nextProgress) {
            Write-Host ("[licenses] Still running: {0}, elapsed {1}s..." -f $Template, [int]$timer.Elapsed.TotalSeconds)
            $nextProgress = $timer.Elapsed.TotalSeconds + 5
        }
    }
    # stderr can close before the process exits; keep reporting until it finishes.
    while (-not $process.WaitForExit(1000)) {
        if ($timer.Elapsed.TotalSeconds -ge $nextProgress) {
            Write-Host ("[licenses] Still running: {0}, elapsed {1}s..." -f $Template, [int]$timer.Elapsed.TotalSeconds)
            $nextProgress = $timer.Elapsed.TotalSeconds + 5
        }
    }
    $exitCode = $process.ExitCode
    if ($exitCode -eq 0) {
        Write-Host ("[licenses] Finished {0} in {1:N1}s" -f $Template, $timer.Elapsed.TotalSeconds)
    } else {
        Write-Host "[licenses] cargo-about failed (exit $exitCode). See $logPath"
    }
} catch {
    $exitCode = 1
    Write-Host "[licenses] $($_.Exception.Message)"
} finally {
    if ($null -ne $log) { $log.Dispose() }
    $process.Dispose()
}
exit $exitCode
