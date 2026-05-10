@echo off
call "C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvarsall.bat" amd64
set "CMAKE_GENERATOR=Ninja"
set "CL=/FS"
set "CARGO_TARGET_DIR=C:\mf"
for /d /r "..\target\debug\build" %%d in (vulkan-shaders-gen-build) do rd /s /q "%%d" 2>nul
pnpm run tauri:dev:vulkan
