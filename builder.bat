@echo off
chcp 65001 >nul
setlocal EnableExtensions DisableDelayedExpansion

for %%I in ("%~dp0.") do set "PROJECT_ROOT=%%~fI"
set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"

if defined BUILDER_HOME (
    for %%I in ("%BUILDER_HOME%\.") do set "BUILDER_ROOT=%%~fI"
) else (
    set "SEARCH_ROOT=%PROJECT_ROOT%"
    call :find_builder
)

if not defined BUILDER_ROOT (
    echo Builder General not found. Move the project under the Code root or set BUILDER_HOME.
    if "%~1"=="" pause
    exit /b 1
)
if not exist "%BUILDER_ROOT%\Cargo.toml" (
    echo Invalid BUILDER_HOME: "%BUILDER_ROOT%"
    if "%~1"=="" pause
    exit /b 1
)
set "BUILDER_HOME=%BUILDER_ROOT%"
set "CARGO_TARGET_DIR=%BUILDER_ROOT%\target"

where cargo >nul 2>&1
if errorlevel 1 (
    echo cargo not found. Install Rust from https://rustup.rs
    if "%~1"=="" pause
    exit /b 1
)

set "BUILDER_LAUNCHER_CLEAN_RECEIPT=%PROJECT_ROOT%\tools\builder\.builder-general-clean-receipt-%RANDOM%-%RANDOM%"
if exist "%BUILDER_LAUNCHER_CLEAN_RECEIPT%" (
    echo Refusing to reuse cleanup receipt: "%BUILDER_LAUNCHER_CLEAN_RECEIPT%"
    exit /b 1
)
cargo run --quiet --manifest-path "%BUILDER_ROOT%\Cargo.toml" -p builder-general -- --project-root-hint "%PROJECT_ROOT%" syncdash %*
set "BUILDER_RC=%errorlevel%"
if exist "%BUILDER_LAUNCHER_CLEAN_RECEIPT%" (
    call :handle_cleanup_receipt
)
if "%~1"=="" pause
exit /b %BUILDER_RC%

:find_builder
if exist "%SEARCH_ROOT%\Experience\builder\Cargo.toml" (
    set "BUILDER_ROOT=%SEARCH_ROOT%\Experience\builder"
    exit /b 0
)
for %%I in ("%SEARCH_ROOT%\..") do set "PARENT_ROOT=%%~fI"
if /I "%PARENT_ROOT%"=="%SEARCH_ROOT%" exit /b 0
set "SEARCH_ROOT=%PARENT_ROOT%"
goto find_builder

:clean_builder_target
for /L %%N in (1,1,8) do (
    cargo clean --quiet --manifest-path "%BUILDER_ROOT%\Cargo.toml"
    if not errorlevel 1 exit /b 0
    powershell -NoProfile -NonInteractive -Command "Start-Sleep -Milliseconds 250" >nul 2>&1
)
exit /b 1

:handle_cleanup_receipt
if not "%BUILDER_RC%"=="0" goto discard_cleanup_receipt
findstr /x /c:"builder-general-clean-v1" "%BUILDER_LAUNCHER_CLEAN_RECEIPT%" >nul 2>&1
if errorlevel 1 goto invalid_cleanup_receipt
del /q "%BUILDER_LAUNCHER_CLEAN_RECEIPT%" >nul 2>&1
call :clean_builder_target
if errorlevel 1 set "BUILDER_RC=1"
exit /b 0

:invalid_cleanup_receipt
echo Invalid Builder cleanup receipt; shared cache was preserved.
set "BUILDER_RC=1"
:discard_cleanup_receipt
del /q "%BUILDER_LAUNCHER_CLEAN_RECEIPT%" >nul 2>&1
exit /b 0
