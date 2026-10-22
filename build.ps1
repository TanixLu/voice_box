#Requires -Version 5.1
$ErrorActionPreference = 'Stop'
$release = $false
$run = $false
$skipLicenses = $false
foreach ($argument in $args) {
    switch ($argument) {
        '--release' { $release = $true }
        '--run' { $run = $true }
        '--skip-licenses' { $skipLicenses = $true }
        default {
            [Console]::Error.WriteLine("Unknown argument: $argument")
            [Console]::Error.WriteLine('Usage: ./build.ps1 [--release] [--run] [--skip-licenses]')
            exit 2
        }
    }
}

$utf8 = [System.Text.UTF8Encoding]::new($false, $true)
$reportDirectory = Join-Path $PSScriptRoot 'target/license-report'
$aboutLog = Join-Path $reportDirectory 'about.log'

function Test-SameFile($Left, $Right) {
    return (Test-Path -LiteralPath $Left -PathType Leaf) -and
        (Test-Path -LiteralPath $Right -PathType Leaf) -and
        ((Get-FileHash -LiteralPath $Left).Hash -eq (Get-FileHash -LiteralPath $Right).Hash)
}

function Render-Licenses($Template, $OutputPath) {
    Write-Host "[licenses] cargo-about: $Template (network verification may take time)"
    # Capture native stderr without PowerShell 5.1 treating it as a terminating error.
    $process = Start-Process -FilePath $cargoPath -NoNewWindow -Wait -PassThru `
        -WorkingDirectory $PSScriptRoot -RedirectStandardError $aboutLog `
        -ArgumentList @('about', 'generate', '--locked', '--fail', '--target',
            'x86_64-pc-windows-msvc', $Template, '-o', "`"$OutputPath`"")
    $diagnostics = [System.IO.File]::ReadAllText($aboutLog, $utf8)
    # cargo-about can succeed after a failed clarification; reject its diagnostics too.
    if ($process.ExitCode -ne 0 -or $diagnostics.Length -gt 0) {
        if ($diagnostics.Length -gt 0) { Write-Host $diagnostics }
        throw 'License generation failed or reported diagnostics; THIRD_PARTY_LICENSES was not replaced.'
    }
}

function Update-Licenses {
    [System.IO.Directory]::CreateDirectory($reportDirectory) | Out-Null
    $licenseInputs = Join-Path $reportDirectory 'inputs.current'
    $inputCache = Join-Path $reportDirectory 'inputs.cached'
    $outputCache = Join-Path $reportDirectory 'notices.cached'
    $licenseTemp = Join-Path $reportDirectory 'THIRD_PARTY_LICENSES.tmp'
    $licenseCrates = Join-Path $reportDirectory 'crates.txt'
    $licenseFile = Join-Path $PSScriptRoot 'THIRD_PARTY_LICENSES'

    Write-Host '[licenses] Checking notice inputs...'
    $process = Start-Process -FilePath $cargoPath -ArgumentList @('about', '--version') `
        -WorkingDirectory $PSScriptRoot -NoNewWindow -Wait -PassThru `
        -RedirectStandardOutput $licenseInputs -RedirectStandardError $aboutLog
    if ($process.ExitCode -ne 0) {
        throw 'Missing cargo-about. Install it with: cargo install --locked cargo-about --features cli'
    }

    $inputsText = [System.IO.File]::ReadAllText($licenseInputs, $utf8)
    foreach ($relativePath in @('Cargo.lock', 'Cargo.toml', 'about.toml', 'about.hbs',
            'about-crates.hbs', 'build.ps1', 'LICENSE-MIT', 'LICENSE-APACHE',
            '.cargo/config', '.cargo/config.toml')) {
        $path = Join-Path $PSScriptRoot $relativePath
        if (Test-Path -LiteralPath $path -PathType Leaf) {
            $inputsText += "===== $relativePath =====`n" +
                [System.IO.File]::ReadAllText($path, $utf8) + "`n"
        }
    }
    [System.IO.File]::WriteAllText($licenseInputs, $inputsText, $utf8)

    if ((Test-SameFile $licenseInputs $inputCache) -and
        (Test-SameFile $licenseFile $outputCache)) {
        Write-Host '[licenses] Up to date.'
        return
    }

    Write-Host '[licenses 1/4] Scanning dependency licenses and rendering notices...'
    Render-Licenses 'about.hbs' $licenseTemp
    Write-Host '[licenses 2/4] Generating dependency list...'
    Render-Licenses 'about-crates.hbs' $licenseCrates

    Write-Host '[licenses 3/4] Appending original third-party notices...'
    $report = [System.IO.File]::ReadAllText($licenseTemp, $utf8)
    foreach ($line in [System.IO.File]::ReadAllLines($licenseCrates, $utf8)) {
        if ([string]::IsNullOrWhiteSpace($line)) { continue }
        $parts = $line.Split(@([char]'|'), 2)
        if ($parts.Length -ne 2 -or [string]::IsNullOrWhiteSpace($parts[0]) -or
            -not (Test-Path -LiteralPath $parts[1] -PathType Leaf)) {
            throw "Invalid dependency entry: $line"
        }
        $crateDirectory = Split-Path -LiteralPath $parts[1]
        foreach ($noticeFile in Get-ChildItem -LiteralPath $crateDirectory -Recurse -File -Force |
                Where-Object { $_.Name -like 'NOTICE*' -or $_.Name -like 'COPYRIGHT*' }) {
            $original = $utf8.GetString([System.IO.File]::ReadAllBytes($noticeFile.FullName))
            $notice = $original.TrimStart([char]0xFEFF).Replace("`r`n", "`n").Trim()
            if ($notice.Length -eq 0 -or $report.Replace("`r`n", "`n").Contains($notice)) {
                Write-Host "[licenses] Skipping already included or empty notice: $($parts[0]) / $($noticeFile.Name)"
                continue
            }
            Write-Host "[licenses] Appending $($parts[0]) / $($noticeFile.Name)"
            $report += "`n===== Additional notice: $($parts[0]) / $($noticeFile.Name) =====`n$original`n"
        }
    }

    Write-Host '[licenses 4/4] Updating notice file...'
    [System.IO.File]::WriteAllText($licenseTemp, $report.Replace("`r`n", "`n"), $utf8)
    # Keep the timestamp for identical text; replace only after every step succeeds.
    if (-not (Test-SameFile $licenseTemp $licenseFile)) {
        if (Test-Path -LiteralPath $licenseFile -PathType Leaf) {
            [System.IO.File]::Replace($licenseTemp, $licenseFile, $null)
        } else {
            [System.IO.File]::Move($licenseTemp, $licenseFile)
        }
    }
    # Cache only a fully verified result; failures must not advance the input cache.
    [System.IO.File]::Copy($licenseFile, $outputCache, $true)
    [System.IO.File]::Copy($licenseInputs, $inputCache, $true)
    Write-Host '[licenses] Notices ready.'
}

$exitCode = 0
Push-Location -LiteralPath $PSScriptRoot
try {
    $cargoPath = (Get-Command cargo -CommandType Application | Select-Object -First 1).Source
    $cargoArguments = @('--locked')
    $buildProfile = 'debug'
    if ($release) {
        $buildProfile = 'release'
        $cargoArguments += '--release'
    }

    if ($skipLicenses) {
        Write-Host '[licenses] Skipping notice update.'
    } else {
        Update-Licenses
    }

    Write-Host "[1/2] Building APO DLL ($buildProfile)..."
    & $cargoPath build --lib @cargoArguments
    if ($LASTEXITCODE -ne 0) { throw 'DLL build failed.' }

    Write-Host "[2/2] Compressing DLL and building EXE ($buildProfile)..."
    & $cargoPath build --bin voice_box @cargoArguments
    if ($LASTEXITCODE -ne 0) { throw 'EXE build failed.' }

    Write-Host "`nDone:"
    Write-Host "  DLL: target/$buildProfile/voice_box_apo.dll"
    Write-Host "  EXE: target/$buildProfile/voice_box.exe"

    if ($run) {
        Write-Host "`nRunning target/$buildProfile/voice_box.exe..."
        $app = Start-Process -FilePath (Join-Path $PSScriptRoot "target/$buildProfile/voice_box.exe") -Verb RunAs -Wait -PassThru
        $exitCode = $app.ExitCode
        if ($exitCode -ne 0) { Write-Host "Program exited with code $exitCode." }
    }
} catch {
    [Console]::Error.WriteLine($_.Exception.Message)
    $exitCode = 1
} finally {
    Pop-Location
}
exit $exitCode
