@echo off
rem Run one transcription perf cycle with configurable model / VAD / run ID.
rem
rem Usage:
rem   run-perf-cycle.bat <RUN_ID> [MODEL] [VAD_MS] [RELEASE]
rem
rem   RUN_ID   label for output files (required)
rem   MODEL    whisper model name (default: large-v3-turbo-q5_0)
rem   VAD_MS   VAD redemption window in ms (default: 2000)
rem   RELEASE  pass "release" as 4th arg to build with --release flag
rem
rem Examples:
rem   run-perf-cycle.bat cycle1-debug
rem   run-perf-cycle.bat cycle2-release large-v3-turbo-q5_0 2000 release
rem   run-perf-cycle.bat cycle3-vad500 large-v3-turbo-q5_0 500
rem   run-perf-cycle.bat cycle5-small small-q5_1 2000

call "C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvarsall.bat" amd64
if errorlevel 1 (
    echo ERROR: Failed to initialise Visual Studio environment.
    exit /b 1
)

set "CMAKE_GENERATOR=Ninja"
set "CL=/FS"
set "CARGO_TARGET_DIR=C:\mf"

set "PERF_RUN_ID=%~1"
if "%PERF_RUN_ID%"=="" (
    echo ERROR: RUN_ID required as first argument.
    exit /b 1
)

set "PERF_MODEL=%~2"
if "%PERF_MODEL%"=="" set "PERF_MODEL=large-v3-turbo-q5_0"

set "PERF_VAD_MS=%~3"
if "%PERF_VAD_MS%"=="" set "PERF_VAD_MS=2000"

set "PERF_RESULTS_DIR=C:\Users\CarlosRuizMartínez\Music\meetily-recordings\perf-sweep"

set "RELEASE_FLAG="
if /i "%~4"=="release" set "RELEASE_FLAG=--release"

cd /d "%~dp0src-tauri"

echo.
echo === Perf cycle: %PERF_RUN_ID% ===
echo   Model  : %PERF_MODEL%
echo   VAD ms : %PERF_VAD_MS%
echo   Build  : %RELEASE_FLAG%
echo   Results: %PERF_RESULTS_DIR%
echo.

cargo test --test pipeline_perf --features vulkan %RELEASE_FLAG% -- --ignored --nocapture
