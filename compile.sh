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
    # Doppelklickbares .app-Bundle (mit sprechendem Namen für VoiceOver)
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
    <key>CFBundleShortVersionString</key> <string>0.2.0</string>
    <key>LSMinimumSystemVersion</key>  <string>11.0</string>
    <key>NSHighResolutionCapable</key> <true/>
    <key>NSMicrophoneUsageDescription</key> <string>TeamConference benötigt das Mikrofon für Sprachübertragung.</string>
</dict>
</plist>
PLIST
else
    # Linux: nacktes Binary (statisch genug für moderne Distributionen)
    cp "$BIN" "$OUT/$BIN_NAME"
    chmod +x "$OUT/$BIN_NAME"
fi

# README mitliefern
cp "$CLIENT_DIR/README.md" "$OUT/README.md" 2>/dev/null || true

# Archiv erstellen
cd "$DIST_DIR"
tar -czf "$PKG.tar.gz" "$PKG"

echo ""
echo "=== Fertig ==="
echo "Ordner:  $OUT"
echo "Archiv:  $DIST_DIR/$PKG.tar.gz"
