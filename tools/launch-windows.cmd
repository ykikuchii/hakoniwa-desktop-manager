@echo off
setlocal
rem Launch Hakoniwa Desktop Manager by double-click (native Windows build).
rem Builds the app when the executable is missing or the sources changed, then
rem starts it detached so this console does not stay attached to the app.
rem
rem ASCII only on purpose: a UTF-8 batch file breaks parsing on a Japanese
rem (cp932) console.
rem
rem Requirements: Node.js, Rust (rustup, MSVC toolchain), the Visual Studio C++
rem build tools and the WebView2 runtime.

pushd "%~dp0.." || goto :fail
set "EXE=%CD%\src-tauri\target\release\hakoniwa-desktop-manager.exe"
if not "%USERPROFILE%"=="" set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"

where cargo >nul 2>&1
if errorlevel 1 (
  echo [ERROR] cargo was not found. Install Rust from https://rustup.rs and reopen the shell.
  goto :fail
)

set "NEEDS_BUILD="
if not exist "%EXE%" set "NEEDS_BUILD=1"
if "%HDM_REBUILD%"=="1" set "NEEDS_BUILD=1"
if not defined NEEDS_BUILD (
  for /f "delims=" %%r in ('powershell -NoProfile -Command "$e=Get-Item -LiteralPath '%EXE%'; $s=Get-ChildItem -Recurse -File -Path 'src','src-tauri\src','src-tauri\Cargo.toml','src-tauri\tauri.conf.json' -ErrorAction SilentlyContinue ^| Where-Object { $_.LastWriteTime -gt $e.LastWriteTime } ^| Select-Object -First 1; if ($s) { 'stale' } else { 'fresh' }"') do set "FRESHNESS=%%r"
  if /i "%FRESHNESS%"=="stale" set "NEEDS_BUILD=1"
)

if defined NEEDS_BUILD (
  echo Building the app, this takes a few minutes on the first run...
  where pnpm >nul 2>&1
  if errorlevel 1 (call corepack pnpm@10 tauri build --no-bundle) else (call pnpm tauri build --no-bundle)
  if errorlevel 1 (echo [ERROR] Build failed. & goto :fail)
)

if not exist "%EXE%" (echo [ERROR] Executable not found: %EXE% & goto :fail)

echo Starting Hakoniwa Desktop Manager...
start "" "%EXE%"
popd
endlocal
exit /b 0

:fail
echo.
echo Failed to launch. See the message above.
pause
popd
endlocal
exit /b 1
