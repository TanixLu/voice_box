@echo off
setlocal EnableExtensions DisableDelayedExpansion
pushd "%~dp0"

set "PROFILE=debug"
set "CARGO_ARGS="
set "RUN_EXE=0"
set "SKIP_LICENSES=0"

:parse_args
if "%~1"=="" goto :build

if /I "%~1"=="--release" (
    set "PROFILE=release"
    set "CARGO_ARGS=--release"
    shift
    goto :parse_args
)

if /I "%~1"=="--run" (
    set "RUN_EXE=1"
    shift
    goto :parse_args
)

if /I "%~1"=="--skip-licenses" (
    set "SKIP_LICENSES=1"
    shift
    goto :parse_args
)

echo Unknown argument: %~1
echo Usage: build.cmd [--release] [--run] [--skip-licenses]
popd
exit /b 2

:build
set "CARGO_COMMON=--locked"

if "%SKIP_LICENSES%"=="1" (
    echo [licenses] Skipping notice update.
) else (
    call :update_licenses
    if errorlevel 1 goto :failed
)
echo [1/3] Building DLL (%PROFILE%)...
cargo build --lib %CARGO_ARGS% %CARGO_COMMON%
if errorlevel 1 goto :failed

echo [2/3] Copying DLL to target\voice_box.dll...
copy /Y "target\%PROFILE%\voice_box.dll" "target\voice_box.dll" >nul
if errorlevel 1 goto :failed

echo [3/3] Building EXE (%PROFILE%)...
cargo build --bin voice_box %CARGO_ARGS% %CARGO_COMMON%
if errorlevel 1 goto :failed

echo.
echo Done:
echo   DLL: target\%PROFILE%\voice_box.dll
echo   Embedded input: target\voice_box.dll
echo   EXE: target\%PROFILE%\voice_box.exe

if "%RUN_EXE%"=="1" (
    echo.
    echo Running target\%PROFILE%\voice_box.exe...
    powershell -NoProfile -Command "$ErrorActionPreference = 'Stop'; $app = Start-Process -FilePath ('target\' + $env:PROFILE + '\voice_box.exe') -Wait -PassThru; exit $app.ExitCode"
    if errorlevel 1 goto :run_failed
)

popd
exit /b 0

:update_licenses
if not exist "target\license-report" mkdir "target\license-report"
if errorlevel 1 exit /b 1
set "LICENSE_INPUTS=target\license-report\inputs.current"
set "LICENSE_INPUT_CACHE=target\license-report\inputs.cached"
set "LICENSE_OUTPUT_CACHE=target\license-report\notices.cached"
set "LICENSE_TEMP=target\license-report\THIRD_PARTY_LICENSES.tmp"
set "LICENSE_CRATES=target\license-report\crates.txt"

echo [licenses] Checking notice inputs...
cargo about --version >"%LICENSE_INPUTS%" 2>nul
if errorlevel 1 (
    echo Missing cargo-about. Install it with: cargo install --locked cargo-about --features cli
    exit /b 1
)
rem Compare content, not timestamps. Source-code edits do not change licenses.
for %%F in (Cargo.lock Cargo.toml about.toml about.hbs about-crates.hbs build.cmd LICENSE-MIT LICENSE-APACHE .cargo\config .cargo\config.toml) do if exist "%%F" (
    >>"%LICENSE_INPUTS%" echo ===== %%F =====
    type "%%F" >>"%LICENSE_INPUTS%"
    if errorlevel 1 exit /b 1
    >>"%LICENSE_INPUTS%" echo.
)

if not exist "%LICENSE_INPUT_CACHE%" goto :generate_licenses
if not exist "%LICENSE_OUTPUT_CACHE%" goto :generate_licenses
if not exist "THIRD_PARTY_LICENSES" goto :generate_licenses
fc /b "%LICENSE_INPUTS%" "%LICENSE_INPUT_CACHE%" >nul
if errorlevel 1 goto :generate_licenses
fc /b "THIRD_PARTY_LICENSES" "%LICENSE_OUTPUT_CACHE%" >nul
if errorlevel 1 goto :generate_licenses
echo [licenses] Up to date.
exit /b 0

:generate_licenses
echo [licenses] Generating notices from Cargo.lock...
echo [licenses 1/4] Scanning dependency licenses and rendering notices...
call :render_licenses "about.hbs" "%LICENSE_TEMP%"
if errorlevel 1 exit /b 1
echo [licenses 2/4] Generating dependency list...
call :render_licenses "about-crates.hbs" "%LICENSE_CRATES%"
if errorlevel 1 exit /b 1

echo [licenses 3/4] Appending original third-party notices...
rem Read the generated package list as data; append original notice files byte-for-byte.
for /f "usebackq tokens=1,* delims=|" %%A in ("%LICENSE_CRATES%") do (
    call :append_notice_files "%%B" "%%A"
    if errorlevel 1 exit /b 1
)

echo [licenses 4/4] Updating notice file...
rem ul_unas_rs supplies the upstream model attribution in its NOTICE, appended above.

rem Normalize CRLF after all original notices have been appended.
powershell -NoProfile -Command "$ErrorActionPreference = 'Stop'; $path = $env:LICENSE_TEMP; $encoding = [System.Text.UTF8Encoding]::new($false, $true); $text = [System.IO.File]::ReadAllText($path, $encoding); [System.IO.File]::WriteAllText($path, $text.Replace(([string][char]13 + [char]10), [string][char]10), $encoding)"
if errorlevel 1 exit /b 1

rem Keep its timestamp if regeneration produced identical text.
if exist "THIRD_PARTY_LICENSES" (
    fc /b "%LICENSE_TEMP%" "THIRD_PARTY_LICENSES" >nul
    if not errorlevel 1 goto :cache_licenses
)

rem Replace the tracked file only after every generation step succeeds.
move /Y "%LICENSE_TEMP%" "THIRD_PARTY_LICENSES" >nul
if errorlevel 1 exit /b 1

:cache_licenses
rem Cache only a fully verified result; failures must not advance the cache.
copy /Y "THIRD_PARTY_LICENSES" "%LICENSE_OUTPUT_CACHE%" >nul
if errorlevel 1 exit /b 1
copy /Y "%LICENSE_INPUTS%" "%LICENSE_INPUT_CACHE%" >nul
if errorlevel 1 exit /b 1
echo [licenses] Notices ready.
exit /b 0

:render_licenses
rem cargo-about can warn and fall back after a failed clarification. Reject that
rem result so a network failure or changed license cannot drop original notices.
echo [licenses] cargo-about: %~1 (network verification may take time)
cargo about generate --locked --fail --target x86_64-pc-windows-msvc "%~1" -o "%~2" 2>"target\license-report\about.log"
if errorlevel 1 (
    type "target\license-report\about.log"
    exit /b 1
)
for %%L in ("target\license-report\about.log") do if %%~zL GTR 0 (
    type "target\license-report\about.log"
    echo License generation reported diagnostics; THIRD_PARTY_LICENSES was not replaced.
    exit /b 1
)
exit /b 0

:append_notice_files
for /f "delims=" %%N in ('dir /s /b /a-d "%~dp1NOTICE*" "%~dp1COPYRIGHT*" 2^>nul') do (
    set "NOTICE_FILE=%%N"
    rem Compare the complete notice, normalizing line endings and outer whitespace.
    rem Exit code 2 means the notice is already included or empty; 1 means failure.
    powershell -NoProfile -Command ^
        "$ErrorActionPreference = 'Stop'; $encoding = [System.Text.UTF8Encoding]::new($false, $true);" ^
        "$notice = [System.IO.File]::ReadAllText($env:NOTICE_FILE, $encoding).Replace(([string][char]13 + [char]10), [string][char]10).Trim();" ^
        "$report = [System.IO.File]::ReadAllText($env:LICENSE_TEMP, $encoding).Replace(([string][char]13 + [char]10), [string][char]10);" ^
        "if ($notice.Length -eq 0 -or $report.Contains($notice)) { exit 2 }"
    if errorlevel 3 exit /b 1
    if errorlevel 2 (
        echo [licenses] Skipping already included or empty notice: %~2 / %%~nxN
    ) else (
        if errorlevel 1 exit /b 1
        echo [licenses] Appending %~2 / %%~nxN
        >>"%LICENSE_TEMP%" echo.
        >>"%LICENSE_TEMP%" echo ===== Additional notice: %~2 / %%~nxN =====
        type "%%N" >>"%LICENSE_TEMP%"
        if errorlevel 1 exit /b 1
        >>"%LICENSE_TEMP%" echo.
    )
)
rem A skipped final notice must not leave errorlevel 2 set for the caller.
exit /b 0

:failed
echo.
echo Build or license update failed.
popd
exit /b 1

:run_failed
set "RUN_EXIT_CODE=%ERRORLEVEL%"
echo.
echo Program exited with code %RUN_EXIT_CODE%.
popd
exit /b %RUN_EXIT_CODE%
