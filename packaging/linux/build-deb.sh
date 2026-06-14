#!/usr/bin/env bash
# Paketiert den bereits release-gebauten Client als .deb (Debian/Ubuntu) und
# zusätzlich als .tar.gz. Erwartet, dass `cargo build --release` im client/
# bereits gelaufen ist. Läuft auf Debian/Ubuntu (nutzt dpkg-deb).
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
DIST="$ROOT/dist"
BIN_NAME="teamconference-client"
BIN="$ROOT/client/target/release/$BIN_NAME"

ARCH="$(dpkg --print-architecture 2>/dev/null || echo amd64)"

# Version aus client/Cargo.toml lesen (erste version = "…"-Zeile im [package]).
VERSION="$(grep -m1 '^version' "$ROOT/client/Cargo.toml" | sed -E 's/.*"([^"]+)".*/\1/')"

if [ ! -f "$BIN" ]; then
    echo "FEHLER: Release-Binary nicht gefunden: $BIN (zuerst 'cargo build --release' im client/)"
    exit 1
fi

mkdir -p "$DIST"

# ── .deb bauen ──
PKGROOT="$(mktemp -d)"
trap 'rm -rf "$PKGROOT"' EXIT

install -Dm755 "$BIN" "$PKGROOT/usr/bin/$BIN_NAME"
install -Dm644 "$SCRIPT_DIR/teamconference.desktop" \
    "$PKGROOT/usr/share/applications/teamconference.desktop"

mkdir -p "$PKGROOT/DEBIAN"
cat > "$PKGROOT/DEBIAN/control" <<CONTROL
Package: teamconference-client
Version: $VERSION
Section: net
Priority: optional
Architecture: $ARCH
Maintainer: herwigfelix <noreply@users.noreply.github.com>
Depends: libgtk-3-0, libasound2, libspeechd2
Description: TeamConference - barrierefreier Sprach-Client
 Native, screenreader-freundliche Oberfläche (GTK) fuer den TeamConference-
 Sprachserver. Sprachausgabe von Server-Ereignissen ueber Speech Dispatcher.
CONTROL

DEB="$DIST/TeamConference-linux-${ARCH}.deb"
dpkg-deb --root-owner-group --build "$PKGROOT" "$DEB"
echo "DEB:  $DEB"

# ── .tar.gz bauen (nacktes Binary für distributionsunabhängige Nutzung) ──
PKG="TeamConference-linux-${ARCH}"
OUT="$DIST/$PKG"
rm -rf "$OUT"
mkdir -p "$OUT"
cp "$BIN" "$OUT/$BIN_NAME"
chmod +x "$OUT/$BIN_NAME"
cp "$SCRIPT_DIR/teamconference.desktop" "$OUT/teamconference.desktop"
( cd "$DIST" && tar -czf "$PKG.tar.gz" "$PKG" )
echo "TGZ:  $DIST/$PKG.tar.gz"
