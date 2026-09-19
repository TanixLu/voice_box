@echo off
setlocal EnableExtensions
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
echo [1/3] Building DLL (%PROFILE%)...
cargo build --lib %CARGO_ARGS%
if errorlevel 1 goto :failed

echo [2/3] Copying DLL to target\voice_box.dll...
copy /Y "target\%PROFILE%\voice_box.dll" "target\voice_box.dll" >nul
if errorlevel 1 goto :failed

echo [3/3] Building EXE (%PROFILE%)...
cargo build --bin voice_box %CARGO_ARGS%
if errorlevel 1 goto :failed

echo.
echo Done:
echo   DLL: target\%PROFILE%\voice_box.dll
echo   Embedded input: target\voice_box.dll
echo   EXE: target\%PROFILE%\voice_box.exe

if "%RUN_EXE%"=="1" (
    echo.
    echo Running target\%PROFILE%\voice_box.exe...
    set "VOICE_BOX_LOG=1"
    "target\%PROFILE%\voice_box.exe"
    if errorlevel 1 goto :run_failed
)

popd
exit /b 0

:failed
echo.
echo Build failed.
popd
exit /b 1

:run_failed
set "RUN_EXIT_CODE=%ERRORLEVEL%"
echo.
echo Program exited with code %RUN_EXIT_CODE%.
popd
exit /b %RUN_EXIT_CODE%
