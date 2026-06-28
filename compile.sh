#!/usr/bin/env bash
# Erzeugt ein eigenständiges Client-Compilat für die aktuelle Plattform
# (macOS oder Linux) im Ordner dist/. Betrifft nur den Client.
set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
CLIENT_DIR="$SCRIPT_DIR/client"
DIST_DIR="$SCRIPT_DIR/dist"
BIN_NAME="teamconference-client"

# Version aus client/Cargo.toml (eine Quelle der Wahrheit – keine hartcodierte
# Version mehr in der Info.plist).
VERSION="$(grep -m1 '^version' "$CLIENT_DIR/Cargo.toml" | sed -E 's/.*"([^"]+)".*/\1/')"

OS="$(uname -s)"
ARCH="$(uname -m)"
case "$OS" in
    Darwin) OSNAME="macos" ;;
    Linux)  OSNAME="linux" ;;
    *) echo "Nicht unterstütztes System: $OS (nutze compile.bat auf Windows)"; exit 1 ;;
esac

echo "=== Baue TeamConference-Client ($OSNAME-$ARCH) ==="
cd "$CLIENT_DIR"

# Auf macOS kann der wxWidgets-/wxDragon-CMake-Build mit sehr neuen
# Apple-Clang-Versionen (z. B. aus einer Xcode-Beta) fehlschlagen — libc++
# findet ohne passendes SDK seine C-Header nicht („<cstddef> tried including
# <stddef.h> but didn't find libc++'s <stddef.h>"). Die Command Line Tools
# bringen i. d. R. eine stabilere Toolchain mit. Daher: zuerst mit der aktiven
# Toolchain bauen und nur bei Fehlschlag auf die Command Line Tools ausweichen.
# Auf der CI (funktionierendes Xcode) greift der Fallback nie.
CLT_DIR="/Library/Developer/CommandLineTools"
if cargo build --release; then
    :
elif [ "$OSNAME" = "macos" ] && [ -z "${DEVELOPER_DIR:-}" ] \
        && [ -x "$CLT_DIR/usr/bin/clang" ]; then
    echo ""
    echo "Build mit der aktiven Toolchain fehlgeschlagen — erneuter Versuch mit"
    echo "den Command Line Tools ($CLT_DIR)…"
    export DEVELOPER_DIR="$CLT_DIR"
    cargo build --release
else
    exit 1
fi

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
    <key>CFBundleShortVersionString</key> <string>$VERSION</string>
    <key>LSMinimumSystemVersion</key>  <string>11.0</string>
    <key>NSHighResolutionCapable</key> <true/>
    <key>NSMicrophoneUsageDescription</key> <string>TeamConference benötigt das Mikrofon für die Sprachübertragung.</string>
</dict>
</plist>
PLIST

    # Signieren. Ist MAC_SIGN_IDENTITY gesetzt (z. B. "Developer ID Application:
    # Name (TEAMID)"), wird mit echtem Zertifikat + Hardened Runtime signiert –
    # Voraussetzung für die Notarisierung. Ohne diese Variable fällt der Build
    # auf eine Ad-hoc-Signatur zurück (lokale Builds: kein Zertifikat nötig, aber
    # macOS warnt beim Öffnen und merkt sich die Mikrofon-Berechtigung weniger
    # zuverlässig).
    ENTITLEMENTS="$SCRIPT_DIR/packaging/macos/entitlements.plist"
    if [ -n "${MAC_SIGN_IDENTITY:-}" ]; then
        echo "Signiere mit Developer ID: $MAC_SIGN_IDENTITY"
        codesign --force --options runtime --timestamp \
            --entitlements "$ENTITLEMENTS" \
            --sign "$MAC_SIGN_IDENTITY" "$APP"
        codesign --verify --strict --verbose=2 "$APP"
        echo "Developer-ID-signiert (Hardened Runtime)."
    else
        # "-" = ad-hoc, kein Zertifikat nötig.
        codesign --force --deep --sign - "$APP" 2>/dev/null \
            && echo "Ad-hoc signiert (keine MAC_SIGN_IDENTITY gesetzt)." \
            || echo "WARNUNG: codesign nicht verfügbar — Mikrofon-Prompt evtl. unzuverlässig."
    fi

    # Notarisierung ist möglich, wenn echt signiert wurde und alle App-Store-
    # Connect-API-Key-Variablen vorliegen.
    DO_NOTARIZE=0
    if [ -n "${MAC_SIGN_IDENTITY:-}" ] && [ -n "${AC_API_KEY_PATH:-}" ] \
        && [ -n "${AC_API_KEY_ID:-}" ] && [ -n "${AC_API_ISSUER_ID:-}" ]; then
        DO_NOTARIZE=1
    fi

    # Schritt 1: die App selbst notarisieren und das Ticket ANHEFTEN. Nur so ist
    # die App auch beim allerersten Start OHNE Internet vertrauenswürdig (das
    # Ticket hängt dann an der App, nicht nur am DMG). notarytool akzeptiert keine
    # .app direkt → vorher als ZIP verpacken.
    if [ "$DO_NOTARIZE" = "1" ]; then
        echo "Notarisiere App bei Apple (kann einige Minuten dauern)…"
        APP_ZIP="$DIST_DIR/$PKG-app.zip"
        rm -f "$APP_ZIP"
        ditto -c -k --keepParent "$APP" "$APP_ZIP"
        xcrun notarytool submit "$APP_ZIP" \
            --key "$AC_API_KEY_PATH" \
            --key-id "$AC_API_KEY_ID" \
            --issuer "$AC_API_ISSUER_ID" \
            --wait
        xcrun stapler staple "$APP"
        xcrun stapler validate "$APP"
        rm -f "$APP_ZIP"
        echo "App notarisiert und gestapelt."
    fi

    # In die DMG gehört nur die (jetzt gestapelte) .app plus ein Alias auf
    # /Applications, damit man sie per Drag-and-drop installieren kann.
    ln -sf /Applications "$OUT/Applications"

    # DMG aus dem Ordner erzeugen (enthält die .app und den Applications-Alias)
    DMG="$DIST_DIR/$PKG.dmg"
    rm -f "$DMG"
    hdiutil create -volname "TeamConference" -srcfolder "$OUT" -ov -format UDZO "$DMG" >/dev/null

    # Schritt 2: das DMG-Image selbst codesignieren, dann notarisieren und das
    # Ticket anheften. Das Signieren gibt dem Image eine "usable signature", damit
    # es beim Download per Doppelklick ohne Gatekeeper-Warnung mountet (sonst
    # meldet spctl "no usable signature", obwohl ein Ticket angeheftet ist).
    if [ "$DO_NOTARIZE" = "1" ]; then
        echo "Signiere und notarisiere DMG…"
        codesign --force --timestamp --sign "$MAC_SIGN_IDENTITY" "$DMG"
        xcrun notarytool submit "$DMG" \
            --key "$AC_API_KEY_PATH" \
            --key-id "$AC_API_KEY_ID" \
            --issuer "$AC_API_ISSUER_ID" \
            --wait
        xcrun stapler staple "$DMG"
        xcrun stapler validate "$DMG"
        echo "DMG signiert, notarisiert und gestapelt."
    else
        echo "Notarisierung übersprungen (keine Apple-Credentials/Signatur)."
    fi

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
