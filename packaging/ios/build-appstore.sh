#!/usr/bin/env bash
# Baut den iOS/iPadOS-Client für echte Geräte, archiviert ihn mit Xcode und lädt
# ihn zu App Store Connect hoch (TestFlight). `cargo mui ios` baut nur für den
# Simulator; dieser Weg ersetzt es für Geräte-/App-Store-Builds.
#
#   packaging/ios/build-appstore.sh [Buildnummer] [--no-upload]
#
# Ohne Buildnummer wird die in packaging/ios/build-number gespeicherte letzte
# hochgeladene Nummer um 1 erhöht. Nach erfolgreichem Upload wird sie dort
# gespeichert. --no-upload exportiert nur die .ipa nach client/target/ios-appstore/export.
#
# Voraussetzungen: Xcode mit angemeldetem Apple-Konto (Team XJ3N4HV6L6),
# xcodegen, Rust-Target aarch64-apple-ios, RustMUi neben diesem Projekt.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
CLIENT_DIR="$ROOT/client"
OUT="$CLIENT_DIR/target/ios-appstore"
TEAM_ID="XJ3N4HV6L6"
TARGET="aarch64-apple-ios"

UPLOAD=1
BUILD=""
for a in "$@"; do
    case "$a" in
        --no-upload) UPLOAD=0 ;;
        *) BUILD="$a" ;;
    esac
done
if [ -z "$BUILD" ]; then
    LAST="$(cat "$SCRIPT_DIR/build-number" 2>/dev/null || echo 0)"
    BUILD=$((LAST + 1))
fi
VERSION="$(grep -m1 '^version' "$CLIENT_DIR/Cargo.toml" | sed -E 's/.*"([^"]+)".*/\1/')"
echo "=== TeamConference iOS $VERSION (Build $BUILD) ==="

# 1. Rust-Binary fürs Gerät (das mui-Build-Skript kompiliert die SwiftUI-Quellen
#    aus RustMUi dabei für iphoneos mit).
(cd "$CLIENT_DIR" && IPHONEOS_DEPLOYMENT_TARGET=17.0 CMAKE_POLICY_VERSION_MINIMUM=3.5 \
    cargo build --release --target "$TARGET" --bin teamconference-client)
BIN="$CLIENT_DIR/target/$TARGET/release/teamconference-client"

# 2. Xcode-Hülle erzeugen.
rm -rf "$OUT"
mkdir -p "$OUT"
cp -R "$SCRIPT_DIR/project.yml" "$SCRIPT_DIR/Info.plist" "$SCRIPT_DIR/PrivacyInfo.xcprivacy" \
    "$SCRIPT_DIR/Assets.xcassets" "$OUT/"
xcodegen generate --spec "$OUT/project.yml" --project "$OUT" --quiet

# 3. Archivieren (automatische Signierung; registriert Bundle-ID und Profile bei Bedarf).
ARCHIVE="$OUT/TeamConference.xcarchive"
xcodebuild -project "$OUT/TeamConference.xcodeproj" -scheme TeamConference \
    -configuration Release -destination 'generic/platform=iOS' \
    -archivePath "$ARCHIVE" -derivedDataPath "$OUT/DerivedData" \
    -allowProvisioningUpdates \
    MARKETING_VERSION="$VERSION" CURRENT_PROJECT_VERSION="$BUILD" TC_RUST_BINARY="$BIN" \
    archive

# 4. Exportieren bzw. hochladen.
DEST=upload
[ "$UPLOAD" = 1 ] || DEST=export
cat > "$OUT/ExportOptions.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>method</key>
	<string>app-store-connect</string>
	<key>destination</key>
	<string>$DEST</string>
	<key>teamID</key>
	<string>$TEAM_ID</string>
	<key>signingStyle</key>
	<string>automatic</string>
	<key>uploadSymbols</key>
	<false/>
	<key>manageAppVersionAndBuildNumber</key>
	<false/>
</dict>
</plist>
EOF
xcodebuild -exportArchive -archivePath "$ARCHIVE" \
    -exportOptionsPlist "$OUT/ExportOptions.plist" -exportPath "$OUT/export" \
    -allowProvisioningUpdates

if [ "$UPLOAD" = 1 ]; then
    echo "$BUILD" > "$SCRIPT_DIR/build-number"
    echo "Hochgeladen: $VERSION ($BUILD). Verarbeitung in App Store Connect dauert einige Minuten."
    # Zwischenstände aufräumen; das Archiv bleibt für spätere Symbolisierung liegen.
    rm -rf "$OUT/DerivedData" "$OUT/export"
else
    echo "Exportiert nach $OUT/export"
fi
