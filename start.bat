@echo off
rem TeamConference - Server und Client bauen und starten (Windows)
setlocal

set "SCRIPT_DIR=%~dp0"
set "SERVER_DIR=%SCRIPT_DIR%server"
set "CLIENT_DIR=%SCRIPT_DIR%client"

echo === Building server ===
pushd "%SERVER_DIR%"
cargo build --release
if errorlevel 1 goto :builderror
popd

echo.
echo === Building client ===
pushd "%CLIENT_DIR%"
cargo build --release
if errorlevel 1 goto :builderror
popd

echo.
echo === Starting server ===
start "TeamConference Server" /D "%SERVER_DIR%" "%SERVER_DIR%\target\release\teamconference-server.exe" --config config.default.toml --create-admin

rem Dem Server kurz Zeit zum Starten geben
timeout /t 3 /nobreak >nul

echo.
echo === Starting client ===
echo Login: admin / admin (ueber --create-admin angelegt)
echo Das Schliessen des Clients beendet auch den Server.
echo.
"%CLIENT_DIR%\target\release\teamconference-client.exe"

echo.
echo Client beendet - stoppe Server...
taskkill /IM teamconference-server.exe /F >nul 2>&1
echo Done.
exit /b 0

:builderror
popd
echo.
echo FEHLER: Build fehlgeschlagen.
exit /b 1
