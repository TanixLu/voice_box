@echo off
setlocal EnableExtensions DisableDelayedExpansion
pushd "%~dp0"

set "PROFILE=debug"
set "CARGO_ARGS="
set "RUN_EXE=0"

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

echo Unknown argument: %~1
echo Usage: build.cmd [--release] [--run]
popd
exit /b 2

:build
set "CARGO_COMMON=--locked"

call :update_licenses
if errorlevel 1 goto :failed
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
    "target\%PROFILE%\voice_box.exe"
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
    echo Missing cargo-about. Install it with: cargo install --locked cargo-about
    exit /b 1
)
rem Compare content, not timestamps. Source-code edits do not change licenses.
for %%F in (Cargo.lock Cargo.toml about.toml about.hbs about-crates.hbs build.cmd scripts\render-licenses.ps1 LICENSE-MIT LICENSE-APACHE assets\ul_unas\README.md assets\ul_unas\LICENSE .cargo\config .cargo\config.toml) do if exist "%%F" (
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

echo [licenses 4/4] Appending model license and updating notice file...
>>"%LICENSE_TEMP%" echo.
>>"%LICENSE_TEMP%" echo ===== UL-UNAS DNS3 model and architecture: MIT =====
findstr /c:"https://github.com/Xiaobin-Rong/ul-unas/tree/" "assets\ul_unas\README.md" >>"%LICENSE_TEMP%"
if errorlevel 1 exit /b 1
>>"%LICENSE_TEMP%" echo VoiceBox includes converted DNS3 checkpoint weights and a native Rust implementation adapted from the upstream streaming architecture.
>>"%LICENSE_TEMP%" echo.
type "assets\ul_unas\LICENSE" >>"%LICENSE_TEMP%"
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
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0scripts\render-licenses.ps1" -Template "%~1" -OutputFile "%~2"
if errorlevel 1 exit /b 1
for %%L in ("target\license-report\about.log") do if %%~zL GTR 0 (
    echo License generation reported diagnostics; THIRD_PARTY_LICENSES was not replaced.
    exit /b 1
)
exit /b 0

:append_notice_files
for /f "delims=" %%N in ('dir /s /b /a-d "%~dp1NOTICE*" "%~dp1COPYRIGHT*" 2^>nul') do (
    echo [licenses] Appending %~2 / %%~nxN
    >>"%LICENSE_TEMP%" echo.
    >>"%LICENSE_TEMP%" echo ===== Additional notice: %~2 / %%~nxN =====
    type "%%N" >>"%LICENSE_TEMP%"
    if errorlevel 1 exit /b 1
    >>"%LICENSE_TEMP%" echo.
)
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
