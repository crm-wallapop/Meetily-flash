@echo off
rem Build Meetily with Vulkan GPU acceleration on Windows.
rem
rem Three environment variables work around build failures on paths with non-ASCII
rem characters (e.g. accented letters in the Windows username):
rem
rem   CMAKE_GENERATOR=Ninja  — prevents CMake from trying to use the VS instance
rem                            selector, which conflicts with Ninja on Windows
rem   CL=/FS                 — serialises PDB writes so parallel cl.exe processes
rem                            do not fight over the same .pdb file
rem   CARGO_TARGET_DIR=C:\mf — redirects all build output through an ASCII-only
rem                            junction path; mspdbsrv.exe fails silently when the
rem                            PDB path contains non-ASCII characters (C1041 error)
rem
rem Prerequisite: create the junction once with:
rem   mklink /J C:\mf <abs-path-to-repo>\target
rem
rem If the VS environment fails to initialise below, check that VS 2022 Build Tools
rem are installed. Community/Professional editions use a different path — adapt line 20.

call "C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvarsall.bat" amd64
if errorlevel 1 (
    echo ERROR: Failed to initialise Visual Studio environment.
    echo Make sure VS 2022 Build Tools are installed, or adapt the path above
    echo for Community/Professional editions.
    exit /b 1
)

set "CMAKE_GENERATOR=Ninja"
set "CL=/FS"
set "CARGO_TARGET_DIR=C:\mf"
for /d /r "..\target\debug\build" %%d in (vulkan-shaders-gen-build) do rd /s /q "%%d" 2>nul
pnpm run tauri:dev:vulkan
