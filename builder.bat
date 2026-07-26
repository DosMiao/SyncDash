@echo off
setlocal
title SyncDash Builder
set "PROJECT_DIR=%~dp0"
set PATH=%USERPROFILE%\.cargo\bin;%PATH%

echo.
echo   =========================================
echo       S Y N C D A S H    B U I L D E R
echo   =========================================
echo.
echo     [1] Dev       tauri dev (HMR)
echo     [2] Desktop   release exe (vite + cargo)
echo     [3] CLI       syncdash.exe release
echo     [4] All       2 + 3
echo     [Q] Quit
echo.
choice /c 1234Q /n /m "   Choice: "
set "MODE=%errorlevel%"
if "%MODE%"=="5" goto :end
if "%MODE%"=="0" goto :end

if "%MODE%"=="1" goto :dev
if "%MODE%"=="2" goto :desktop
if "%MODE%"=="3" goto :cli
if "%MODE%"=="4" goto :all
goto :end

:dev
cd /d "%PROJECT_DIR%"
call npx tauri dev
goto :end

:desktop
cd /d "%PROJECT_DIR%"
echo   [1/2] Frontend (vite) ...
call npm run build
if errorlevel 1 goto :fail
echo   [2/2] Cargo (syncdash-desktop, release) ...
cargo build --release -p syncdash-desktop
if errorlevel 1 goto :fail
echo.
echo   ===== OK =====  %PROJECT_DIR%target\release\syncdash-desktop.exe
start "" "%PROJECT_DIR%target\release\syncdash-desktop.exe"
goto :end

:cli
cd /d "%PROJECT_DIR%"
cargo build --release -p syncdash
if errorlevel 1 goto :fail
echo   ===== OK =====  %PROJECT_DIR%target\release\syncdash.exe
goto :end

:all
call "%~f0" 2 <nul
call "%~f0" 3 <nul
goto :end

:fail
echo.
echo   ===== BUILD FAILED =====
goto :end

:end
echo.
endlocal
