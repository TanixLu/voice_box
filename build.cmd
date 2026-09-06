@echo off
setlocal EnableExtensions
pushd "%~dp0"

set "PROFILE=debug"
set "CARGO_ARGS="

if "%~1"=="" goto :build

if /I "%~1"=="--release" (
    set "PROFILE=release"
    set "CARGO_ARGS=--release"
    goto :build
)

echo Usage: build.cmd [--release]
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
popd
exit /b 0

:failed
echo.
echo Build failed.
popd
exit /b 1
