@echo off
rem Erzeugt ein eigenstaendiges Client-Compilat fuer Windows im Ordner dist\.
rem Betrifft nur den Client.
setlocal

set "SCRIPT_DIR=%~dp0"
set "CLIENT_DIR=%SCRIPT_DIR%client"
set "DIST_DIR=%SCRIPT_DIR%dist"
set "BIN_NAME=teamconference-client.exe"
set "PKG=TeamConference-windows-x64"
set "OUT=%DIST_DIR%\%PKG%"

echo === Baue TeamConference-Client (windows-x64) ===
pushd "%CLIENT_DIR%"
cargo build --release
if errorlevel 1 (
    popd
    echo.
    echo FEHLER: Build fehlgeschlagen.
    exit /b 1
)
popd

set "BIN=%CLIENT_DIR%\target\release\%BIN_NAME%"
if not exist "%BIN%" (
    echo FEHLER: Binary nicht gefunden: %BIN%
    exit /b 1
)

if exist "%OUT%" rmdir /s /q "%OUT%"
mkdir "%OUT%"

copy /y "%BIN%" "%OUT%\%BIN_NAME%" >nul
if exist "%CLIENT_DIR%\README.md" copy /y "%CLIENT_DIR%\README.md" "%OUT%\README.md" >nul

rem Screenreader-Client-DLLs neben die EXE legen. Tolk selbst ist statisch in die
rem EXE einkompiliert (tolk-sys), aber die Client-DLLs der Screenreader werden zur
rem Laufzeit per Name geladen und muessen daher als Datei daneben liegen. tolk-sys
rem kopiert sie beim Build nach target\release. Fehlen sie, laeuft die App trotzdem
rem (tts faellt auf die Windows-Sprachausgabe WinRT zurueck) — nur ohne NVDA-Routing.
set "REL=%CLIENT_DIR%\target\release"
if exist "%REL%\nvdaControllerClient64.dll" copy /y "%REL%\nvdaControllerClient64.dll" "%OUT%\" >nul
if exist "%REL%\SAAPI64.dll" copy /y "%REL%\SAAPI64.dll" "%OUT%\" >nul

rem ZIP-Archiv erstellen (PowerShell Compress-Archive ist ab Win10 vorhanden)
powershell -NoProfile -Command "Compress-Archive -Path '%OUT%' -DestinationPath '%DIST_DIR%\%PKG%.zip' -Force"

echo.
echo === Fertig ===
echo Ordner:  %OUT%
echo Archiv:  %DIST_DIR%\%PKG%.zip
exit /b 0
