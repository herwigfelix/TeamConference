#!/usr/bin/env bash
# Erzeugt ein eigenständiges Client-Compilat für die aktuelle Plattform
# (macOS oder Linux) im Ordner dist/. Betrifft nur den Client.
set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
CLIENT_DIR="$SCRIPT_DIR/client"
DIST_DIR="$SCRIPT_DIR/dist"
BIN_NAME="teamconference-client"

OS="$(uname -s)"
ARCH="$(uname -m)"
case "$OS" in
    Darwin) OSNAME="macos" ;;
    Linux)  OSNAME="linux" ;;
    *) echo "Nicht unterstütztes System: $OS (nutze compile.bat auf Windows)"; exit 1 ;;
esac

echo "=== Baue TeamConference-Client ($OSNAME-$ARCH) ==="
cd "$CLIENT_DIR"
cargo build --release

BIN="$CLIENT_DIR/target/release/$BIN_NAME"
if [ ! -f "$BIN" ]; then
    echo "FEHLER: Binary nicht gefunden: $BIN"
    exit 1
fi

PKG="TeamConference-$OSNAME-$ARCH"
OUT="$DIST_DIR/$PKG"
rm -rf "$OUT"
mkdir -p "$OUT"

if [ "$OSNAME" = "macos" ]; then
    # Doppelklickbares .app-Bundle (mit sprechendem Namen für VoiceOver).
    # NSMicrophoneUsageDescription ist nötig, damit macOS beim ersten
    # Mikrofonzugriff die Berechtigung abfragt.
    APP="$OUT/TeamConference.app"
    mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
    cp "$BIN" "$APP/Contents/MacOS/$BIN_NAME"
    cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>            <string>TeamConference</string>
    <key>CFBundleDisplayName</key>     <string>TeamConference</string>
    <key>CFBundleIdentifier</key>      <string>org.accessy.TCClient</string>
    <key>CFBundleExecutable</key>      <string>$BIN_NAME</string>
    <key>CFBundlePackageType</key>     <string>APPL</string>
    <key>CFBundleShortVersionString</key> <string>0.3.2</string>
    <key>LSMinimumSystemVersion</key>  <string>11.0</string>
    <key>NSHighResolutionCapable</key> <true/>
    <key>NSMicrophoneUsageDescription</key> <string>TeamConference benötigt das Mikrofon für die Sprachübertragung.</string>
</dict>
</plist>
PLIST

    # Ad-hoc-Signatur: ohne Signatur merkt sich macOS die einmal erteilte
    # Mikrofon-Berechtigung nicht zuverlässig (TCC). "-" = ad-hoc, kein Zertifikat nötig.
    codesign --force --deep --sign - "$APP" 2>/dev/null \
        && echo "Ad-hoc signiert." \
        || echo "WARNUNG: codesign nicht verfügbar — Mikrofon-Prompt evtl. unzuverlässig."

    # In die DMG gehört nur die .app plus ein Alias auf /Applications, damit man
    # die App per Drag-and-drop installieren kann (keine README in der DMG).
    ln -sf /Applications "$OUT/Applications"

    # DMG aus dem Ordner erzeugen (enthält die .app und den Applications-Alias)
    DMG="$DIST_DIR/$PKG.dmg"
    rm -f "$DMG"
    hdiutil create -volname "TeamConference" -srcfolder "$OUT" -ov -format UDZO "$DMG" >/dev/null
    echo ""
    echo "=== Fertig ==="
    echo "App:  $APP"
    echo "DMG:  $DMG"
else
    # Linux: nacktes Binary (statisch genug für moderne Distributionen)
    cp "$BIN" "$OUT/$BIN_NAME"
    chmod +x "$OUT/$BIN_NAME"
    cp "$CLIENT_DIR/README.md" "$OUT/README.md" 2>/dev/null || true

    # Archiv erstellen
    cd "$DIST_DIR"
    tar -czf "$PKG.tar.gz" "$PKG"
    echo ""
    echo "=== Fertig ==="
    echo "Ordner:  $OUT"
    echo "Archiv:  $DIST_DIR/$PKG.tar.gz"
fi
