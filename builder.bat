@echo off
setlocal
title SyncDash Builder

REM Keep this file pure ASCII: cmd parses .bat in the OEM codepage, and UTF-8
REM text corrupts enough bytes to break goto/label parsing outright.

set "PROJECT_DIR=%~dp0"
set "REL=%PROJECT_DIR%target\release"
set "APP=%REL%\syncdash-desktop.exe"
set "APP_DBG=%PROJECT_DIR%target\debug\syncdash-desktop.exe"
set "CLI=%REL%\syncdash.exe"
set "VITE_PORT=5173"
REM Explorer-launched cmd does not always inherit cargo's bin dir. Add it.
set PATH=%USERPROFILE%\.cargo\bin;%PATH%

REM Detect ESC once (menu colors + clickable file links).
REM Windows Terminal (WT_SESSION) renders VT natively, so the instant cmd
REM prompt-trick suffices and the menu appears with zero spawn delay; legacy
REM conhost keeps the slower powershell spawn - that spawn also flips the
REM console into VT mode as a side effect, which plain cmd cannot do.
set "ESC="
if defined WT_SESSION goto :esc_prompt_trick
for /f %%a in ('powershell -NoProfile -Command "[char]27" 2^>nul') do set "ESC=%%a"
goto :esc_done
:esc_prompt_trick
REM Every line the child cmd echoes starts with the #$E# prompt, so token 1
REM under delims=# is the bare ESC char.
for /f "delims=#" %%a in ('"prompt #$E# & echo on & for %%b in (1) do rem"') do set "ESC=%%a"
:esc_done

REM Dim is CDM, not CD: %CD% is cmd's dynamic current-directory variable - when
REM the no-ANSI path unsets it, %CD% expands to the working directory instead of
REM "" and the menu prints paths mid-text.
if not defined ESC goto :colors_empty
set "C0=%ESC%[0m"
set "CB=%ESC%[96m"
set "CT=%ESC%[1;93m"
set "CG=%ESC%[92m"
set "CY=%ESC%[93m"
set "CR=%ESC%[91m"
set "CW=%ESC%[97m"
set "CDM=%ESC%[90m"
goto :colors_done
:colors_empty
set "C0="
set "CB="
set "CT="
set "CG="
set "CY="
set "CR="
set "CW="
set "CDM="
:colors_done

echo.
echo   %CB%=================================================================%C0%
echo   %CB%    %CT%S Y N C D A S H    B U I L D E R%C0%
echo   %CB%=================================================================%C0%
echo   %CDM%%PROJECT_DIR%%C0%
echo.
echo   %CDM%Build%C0%                                    %CDM%Run (kill + launch)%C0%
echo     %CG%[1]%C0% %CW%Dev%C0%          %CDM%tauri dev, HMR%C0%         %CG%[R]%C0% %CW%Desktop%C0%
echo     %CG%[2]%C0% %CW%Desktop%C0%      %CDM%vite + cargo%C0%
echo     %CG%[3]%C0% %CW%CLI%C0%          %CDM%syncdash.exe, no node%C0%
echo     %CG%[4]%C0% %CW%All%C0%          %CDM%[2] + [3]%C0%
echo                      %CDM%2 / 4 auto-launch on success%C0%
echo.
echo   %CDM%Utility%C0%
echo     %CY%[5]%C0% %CW%Kill app%C0%    %CY%[6]%C0% %CW%Kill + unlock exe%C0%    %CY%[7]%C0% %CW%Clean artifacts%C0%    %CR%[Q]%C0% %CW%Quit%C0%
echo.
choice /c 1234567RQ /n /t 3 /d 2 /m "   Choice [1-7 R Q]  (default = 2 Desktop in 3s): "
set "MODE=%errorlevel%"

REM choice: 0 = Ctrl+C / break, 255 = error - bail out instead of falling
REM through the if-ladder.
if "%MODE%"=="0" goto :end
if "%MODE%"=="255" goto :end
REM Q is the LAST character in the /c list, so its errorlevel is the list length.
REM Adding an option moves both it and R - keep these two lines in step.
if "%MODE%"=="9" goto :end
if "%MODE%"=="8" goto :run_app

if "%MODE%"=="1" goto :dev
if "%MODE%"=="2" goto :desktop
if "%MODE%"=="3" goto :cli
if "%MODE%"=="4" goto :all
if "%MODE%"=="5" goto :kill_only
if "%MODE%"=="6" goto :kill_unlock
if "%MODE%"=="7" goto :clean_all
goto :end

:dev
call :need_node
if errorlevel 1 goto :prereq_fail
title SyncDash Builder - Dev
REM tauri dev relinks target\debug\syncdash-desktop.exe and Vite holds
REM %VITE_PORT% with strictPort, so a leftover instance of either kills the run
REM before it starts.
call :free_desktop
call :free_port %VITE_PORT%
echo.
echo   %CG%[Dev]%C0% npx tauri dev  %CDM%vite + tauri dev, this terminal%C0%
cd /d "%PROJECT_DIR%"
call npx tauri dev
goto :end

:desktop
call :need_node
if errorlevel 1 goto :prereq_fail
title SyncDash Builder - Desktop
REM Nothing here renames its output: cargo links straight over
REM target\release\syncdash-desktop.exe, and a running app holds its own image
REM file. That is exactly what "Access is denied. (os error 5)" at the end of a
REM compile means - so the app goes first, and the exe is confirmed free before
REM anything long starts.
call :free_desktop
call :wait_unlocked "%APP%"
call :total_start
call :step_frontend "[1/2]"
if errorlevel 1 goto :build_fail
call :step_desktop "[2/2]"
if errorlevel 1 goto :build_fail
call :report "%APP%" "DESKTOP OK - syncdash-desktop.exe"
if errorlevel 1 goto :build_fail
call :total_end
call :launch_app
goto :end

:cli
call :need_cargo
if errorlevel 1 goto :prereq_fail
title SyncDash Builder - CLI
REM The CLI needs no node: dist/ is a committed artifact and this target does
REM not touch the frontend at all.
call :free_cli
call :wait_unlocked "%CLI%"
call :total_start
call :step_cli "[1/1]"
if errorlevel 1 goto :build_fail
call :report "%CLI%" "CLI OK - syncdash.exe"
if errorlevel 1 goto :build_fail
call :total_end
goto :end

:all
call :need_node
if errorlevel 1 goto :prereq_fail
title SyncDash Builder - All
REM One workspace, one target\release: both binaries have to be free before the
REM two cargo builds, so both get asked about up front rather than failing at
REM the second link three minutes in.
call :free_desktop
call :free_cli
call :wait_unlocked "%APP%"
call :wait_unlocked "%CLI%"
call :total_start
call :step_frontend "[1/3]"
if errorlevel 1 goto :build_fail
call :step_desktop "[2/3]"
if errorlevel 1 goto :build_fail
call :step_cli "[3/3]"
if errorlevel 1 goto :build_fail
call :report "%APP%" "DESKTOP OK - syncdash-desktop.exe"
if errorlevel 1 goto :build_fail
call :report "%CLI%" "CLI OK - syncdash.exe"
if errorlevel 1 goto :build_fail
call :total_end
call :launch_app
goto :end

:run_app
if not exist "%APP%" (
    echo.
    echo   %CR%[Run] ERROR:%C0% not built yet:
    call :print_link "%APP%"
    echo   Build it with %CW%[2]%C0% first.
    goto :end
)
call :free_desktop
call :launch_app
goto :end

:kill_only
call :free_desktop
call :free_port %VITE_PORT%
call :free_cli
goto :end

:kill_unlock
call :free_desktop
call :free_port %VITE_PORT%
call :free_cli
echo.
echo   %CY%[Unlock]%C0% Checking file locks ...
call :wait_unlocked "%APP%"
call :wait_unlocked "%APP_DBG%"
call :wait_unlocked "%CLI%"
goto :end

:clean_all
REM One target/ for the whole workspace, so a single cargo clean covers both
REM crates. dist/ is deliberately left alone: it is a committed artifact, and
REM the Mac builds with pure cargo and has no node to regenerate it.
call :free_desktop
call :free_cli
echo.
echo   %CY%[Clean]%C0% cargo clean over the workspace, dist/ untouched ...
cargo clean --manifest-path "%PROJECT_DIR%Cargo.toml" && echo     target/ cleaned || echo     %CR%Clean FAILED%C0% - exe still locked? try [6]
goto :end

:step_frontend
REM %1 = phase counter. Unlike a `tauri build`, bare cargo never runs
REM beforeBuildCommand, so the frontend has to be bundled explicitly here.
call :phase_start "%~1 FRONTEND - vite build"
cd /d "%PROJECT_DIR%"
call npm run build
if errorlevel 1 exit /b 1
call :phase_end "%~1 FRONTEND - vite build"
exit /b 0

:step_desktop
call :phase_start "%~1 CARGO - syncdash-desktop, release"
cd /d "%PROJECT_DIR%"
cargo build --release -p syncdash-desktop
if errorlevel 1 exit /b 1
call :phase_end "%~1 CARGO - syncdash-desktop, release"
exit /b 0

:step_cli
call :phase_start "%~1 CARGO - syncdash, release"
cd /d "%PROJECT_DIR%"
cargo build --release -p syncdash
if errorlevel 1 exit /b 1
call :phase_end "%~1 CARGO - syncdash, release"
exit /b 0

:report
REM %1 = artifact, %2 = headline
if not exist "%~1" ( echo   %CR%ERROR: cargo reported success but %~nx1 is missing%C0% & exit /b 1 )
call :size_mb "%~1"
echo.
echo   %CG%===== %~2 =====%C0%  %CDM%%SIZE_MB% MB%C0%
call :print_link "%~1"
exit /b 0

:launch_app
if not exist "%APP%" exit /b 0
echo.
echo   %CG%[Run]%C0% Launching %CW%syncdash-desktop.exe%C0% ...
start "" "%APP%"
exit /b 0

REM Only the frontend needs Node. Install deps on a fresh clone too, so a double
REM click does not dead-end on "vite is not recognized".
:need_node
where npm >nul 2>&1
if errorlevel 1 (
    echo.
    echo   %CR%npm not found.%C0% Install Node: https://nodejs.org
    exit /b 1
)
if exist "%PROJECT_DIR%node_modules" exit /b 0
echo.
echo   %CY%[Setup]%C0% First build - running npm install ...
pushd "%PROJECT_DIR%"
call npm install
popd
REM Test the directory rather than errorlevel: popd clobbers it.
if not exist "%PROJECT_DIR%node_modules" exit /b 1
exit /b 0

:need_cargo
where cargo >nul 2>&1
if errorlevel 1 (
    echo.
    echo   %CR%cargo not found.%C0% Install Rust: https://rustup.rs
    exit /b 1
)
exit /b 0

:free_desktop
REM The GUI shell, killed without asking: it is a viewer over the library, holds
REM no lock of its own, and relaunches in a second. taskkill /T takes the
REM WebView2 children with it - they hold the exe just as firmly as the parent.
REM Name matching is exact, so this never touches a running syncdash.exe.
echo.
echo   %CY%[Kill]%C0% Freeing syncdash-desktop ...
powershell -NoProfile -Command ^
  "$procs = Get-Process -Name 'syncdash-desktop' -ErrorAction SilentlyContinue;" ^
  "if (-not $procs) { Write-Host '    Nothing running'; exit 0 };" ^
  "foreach ($p in $procs) { & taskkill /F /T /PID $p.Id 2>&1 | Out-Null };" ^
  "Start-Sleep -Milliseconds 800;" ^
  "$still = Get-Process -Name 'syncdash-desktop' -ErrorAction SilentlyContinue;" ^
  "if ($still) { $still | Stop-Process -Force -ErrorAction SilentlyContinue; Start-Sleep -Milliseconds 500 };" ^
  "Write-Host '    Freed syncdash-desktop + children'"
exit /b 0

:free_cli
REM The CLI is not a shell: a live syncdash.exe can be halfway through an apply,
REM holding the root heartbeat lock and writing files. Killing that behind the
REM user's back is not a build step's business, so this one asks, and a timeout
REM leaves it running - the build failing is the cheaper of the two outcomes.
powershell -NoProfile -Command ^
  "$p = Get-Process -Name 'syncdash' -ErrorAction SilentlyContinue;" ^
  "if (-not $p) { exit 0 };" ^
  "$p | ForEach-Object { $t = '?'; try { $t = $_.StartTime.ToString('HH:mm:ss') } catch {}; Write-Host ('    syncdash.exe  PID ' + $_.Id + '  since ' + $t) };" ^
  "exit 1"
if not errorlevel 1 exit /b 0
echo.
echo   %CY%[Warn]%C0% A syncdash.exe is running - it may be mid-apply.
choice /c YN /n /d N /t 10 /m "   Kill it? [Y/N]  (default = N in 10s): "
if errorlevel 2 (
    echo     Left running - cargo will stop at the link step if it holds the exe.
    exit /b 0
)
powershell -NoProfile -Command "Get-Process -Name 'syncdash' -ErrorAction SilentlyContinue | Stop-Process -Force; Start-Sleep -Milliseconds 500; Write-Host '    Killed syncdash.exe'"
exit /b 0

:free_port
REM %1 = TCP port. Vite is on strictPort, so a dev server left over from a
REM closed terminal does not shift to 5174 - it refuses to start at all.
powershell -NoProfile -Command ^
  "$c = Get-NetTCPConnection -LocalPort %~1 -State Listen -ErrorAction SilentlyContinue;" ^
  "if ($c) { $c | ForEach-Object { Stop-Process -Id $_.OwningProcess -Force -ErrorAction SilentlyContinue }; Write-Host '    Freed port %~1' } else { Write-Host '    Port %~1 free' }"
exit /b 0

:wait_unlocked
REM %1 = file. Killing a process returns before Windows has dropped its handles,
REM and antivirus can hold an exe open for seconds after that. Waiting here
REM turns a compile that dies at the link step into an immediate, readable line.
if not exist "%~1" exit /b 0
powershell -NoProfile -Command ^
  "$f = '%~1'; $r = 0;" ^
  "$n = (Split-Path (Split-Path $f -Parent) -Leaf) + '\' + (Split-Path $f -Leaf);" ^
  "while ($r -lt 5) { try { [IO.File]::Open($f,'Open','ReadWrite','None').Close(); Write-Host ('    Unlocked ' + $n); break } catch { $r++; Write-Host ('    ' + $n + ' locked, waiting ' + $r + '/5 ...'); Start-Sleep 1 } };" ^
  "if ($r -ge 5) { Write-Host ('    WARNING: still locked (antivirus?) ' + $f) }"
exit /b 0

:size_mb
REM %1 = file -> SIZE_MB as "12.3". The decimal comes off the remainder rather
REM than FSIZE*10 so a large artifact cannot overflow set /a's 32-bit signed math.
set "SIZE_MB=?"
if not exist "%~1" exit /b 0
for %%F in ("%~1") do set "FSIZE=%%~zF"
set /a "SZ_INT=FSIZE / 1048576"
set /a "SZ_DEC=(FSIZE %% 1048576) * 10 / 1048576"
set "SIZE_MB=%SZ_INT%.%SZ_DEC%"
exit /b 0

:print_link
REM %1 = full path. OSC-8 hyperlink (ctrl+click in Windows Terminal) when ESC is
REM available, else a plain selectable path.
set "LP=%~1"
set "LURI=file:///%LP:\=/%"
if defined ESC (
    echo   %ESC%]8;;%LURI%%ESC%\^>^> %LP%%ESC%]8;;%ESC%\
) else (
    echo   ^>^> %LP%
)
exit /b 0

REM Timing helpers (PowerShell clock; locale-independent)
:tic
REM %1 = name of var to receive start ticks; also sets _CLK = HH:mm:ss now
for /f "usebackq tokens=1,2 delims=|" %%a in (`powershell -NoProfile -Command "$n=[DateTime]::Now; '{0:HH:mm:ss}|{1}' -f $n,$n.Ticks"`) do ( set "_CLK=%%a" & set "%~1=%%b" )
exit /b 0

:toc
REM %1 = start-ticks value; sets _CLK = HH:mm:ss now, _EL = "Xm Ys" elapsed
for /f "usebackq tokens=1,2 delims=|" %%a in (`powershell -NoProfile -Command "$n=[DateTime]::Now; $e=[TimeSpan]::FromTicks($n.Ticks - %~1); '{0:HH:mm:ss}|{1:0}m {2:00}s' -f $n,[int][math]::Floor($e.TotalMinutes),$e.Seconds"`) do ( set "_CLK=%%a" & set "_EL=%%b" )
exit /b 0

:phase_start
call :tic _PT0
echo.
echo   %CDM%[%_CLK%]%C0%  %CG%START%C0%  %CW%%~1%C0%
exit /b 0

:phase_end
call :toc %_PT0%
echo   %CDM%[%_CLK%]%C0%  %CB%END%C0%    %~1  %CDM%^| elapsed%C0% %CW%%_EL%%C0%
exit /b 0

:total_start
call :tic _TT0
exit /b 0

:total_end
call :toc %_TT0%
echo.
echo   %CDM%[%_CLK%]%C0%  %CT%TOTAL%C0%  %CDM%elapsed%C0% %CW%%_EL%%C0%
exit /b 0

:prereq_fail
echo.
echo   %CR%===== CANNOT BUILD =====%C0%
goto :end

:build_fail
echo.
echo   %CR%===== BUILD FAILED =====%C0%
REM The one failure that is not about the code: cargo cannot relink an exe that
REM is still running, and reports it as "Access is denied. (os error 5)".
echo   %CDM%"Access is denied" on syncdash-desktop.exe or syncdash.exe = a live%C0%
echo   %CDM%instance still holds it. Run [6] Kill + unlock exe, then build again.%C0%
call :total_end
goto :end

:end
title SyncDash Builder
echo.
echo   %CB%Done.%C0%
REM Double-clicked windows vanish on exit, taking the message with them.
pause
endlocal
