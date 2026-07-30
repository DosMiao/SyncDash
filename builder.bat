@echo off
chcp 65001 >nul
setlocal

set "PROJECT_ROOT=%~dp0"
set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"

where cargo >nul 2>&1
if errorlevel 1 (
    echo cargo not found. Install Rust from https://rustup.rs
    if "%~1"=="" pause
    exit /b 1
)

cargo run --quiet --manifest-path "%PROJECT_ROOT%tools\builder\Cargo.toml" -- %*
set "BUILDER_RC=%errorlevel%"
if "%~1"=="" pause
exit /b %BUILDER_RC%
