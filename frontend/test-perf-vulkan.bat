@echo off
rem Run the transcription pipeline performance test with Vulkan GPU acceleration.
rem
rem Uses the same environment setup as run_vulkan_windows.bat:
rem   - vcvarsall puts Ninja on PATH (required by whisper-rs-sys cmake build)
rem   - CMAKE_GENERATOR=Ninja avoids VS instance selector conflicts
rem   - CL=/FS serialises PDB writes for parallel cl.exe
rem   - CARGO_TARGET_DIR=C:\mf avoids non-ASCII path failures in mspdbsrv.exe
rem
rem Prerequisite (one-time): create the junction
rem   mklink /J C:\mf <abs-path-to-repo>\target

call "C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvarsall.bat" amd64
if errorlevel 1 (
    echo ERROR: Failed to initialise Visual Studio environment.
    exit /b 1
)

set "CMAKE_GENERATOR=Ninja"
set "CL=/FS"
set "CARGO_TARGET_DIR=C:\mf"

cd /d "%~dp0src-tauri"

cargo test --test pipeline_perf --features vulkan -- --ignored --nocapture
