#!/usr/bin/env bash
# Baut die Kern-Bibliothek (lib/) für iOS und Android nach dist/mobile/.
#
#   ./build-mobile.sh            # beides (iOS nur auf macOS)
#   ./build-mobile.sh ios
#   ./build-mobile.sh android
#
# Ergebnis:
#   dist/mobile/ios/TeamConferenceCore.xcframework   (Gerät arm64 + Simulator arm64/x86_64,
#                                                     statische Lib + Header + modulemap)
#   dist/mobile/android/jniLibs/<abi>/               (libteamconference_core.so + libc++_shared.so)
#   dist/mobile/android/include/teamconference_core.h
#
# Voraussetzungen: rustup-Targets (werden bei Bedarf nachinstalliert), CMake,
# Ninja; iOS: Xcode; Android: NDK (ANDROID_NDK_HOME oder <SDK>/ndk/<neueste>)
# und cargo-ndk (`cargo install cargo-ndk`).
set -euo pipefail

cd "$(dirname "$0")"
LIB_DIR="$(pwd)"
OUT="$LIB_DIR/../dist/mobile"
NAME=teamconference_core

# Mindestversionen — gelten gleichermaßen für Rust und den C-Code (Opus, ring).
# Ohne einheitliches Deployment-Target baut cc/CMake gegen das SDK-Maximum und
# der Linker scheitert an ___chkstk_darwin.
export IPHONEOS_DEPLOYMENT_TARGET="${IPHONEOS_DEPLOYMENT_TARGET:-13.0}"
ANDROID_API="${ANDROID_API:-24}"
ANDROID_ABIS=(arm64-v8a armeabi-v7a x86_64 x86)

WANT="${1:-all}"

ensure_targets() {
    local missing=()
    for t in "$@"; do
        rustup target list --installed | grep -qx "$t" || missing+=("$t")
    done
    if [ ${#missing[@]} -gt 0 ]; then
        rustup target add "${missing[@]}"
    fi
}

build_ios() {
    if [ "$(uname)" != "Darwin" ]; then
        echo "iOS: nur auf macOS möglich — übersprungen."
        return
    fi
    echo "==> iOS"
    local targets=(aarch64-apple-ios aarch64-apple-ios-sim x86_64-apple-ios)
    ensure_targets "${targets[@]}"
    for t in "${targets[@]}"; do
        cargo build --release --lib --target "$t"
    done

    local stage="$LIB_DIR/target/ios-stage"
    rm -rf "$stage"
    mkdir -p "$stage/sim" "$stage/headers"
    lipo -create \
        "target/aarch64-apple-ios-sim/release/lib$NAME.a" \
        "target/x86_64-apple-ios/release/lib$NAME.a" \
        -output "$stage/sim/lib$NAME.a"
    cp include/$NAME.h "$stage/headers/"
    cat > "$stage/headers/module.modulemap" <<EOF
module TeamConferenceCore {
    header "$NAME.h"
    export *
}
EOF

    mkdir -p "$OUT/ios"
    rm -rf "$OUT/ios/TeamConferenceCore.xcframework"
    xcodebuild -create-xcframework \
        -library "target/aarch64-apple-ios/release/lib$NAME.a" -headers "$stage/headers" \
        -library "$stage/sim/lib$NAME.a" -headers "$stage/headers" \
        -output "$OUT/ios/TeamConferenceCore.xcframework"
    echo "    -> $OUT/ios/TeamConferenceCore.xcframework"
}

find_ndk() {
    if [ -n "${ANDROID_NDK_HOME:-}" ] && [ -d "$ANDROID_NDK_HOME" ]; then
        echo "$ANDROID_NDK_HOME"; return
    fi
    local sdk
    for sdk in "${ANDROID_HOME:-}" "${ANDROID_SDK_ROOT:-}" "$HOME/Library/Android/sdk" "$HOME/Android/Sdk"; do
        if [ -n "$sdk" ] && [ -d "$sdk/ndk" ]; then
            local ndk
            ndk="$(ls -1 "$sdk/ndk" | sort -V | tail -1)"
            [ -n "$ndk" ] && { echo "$sdk/ndk/$ndk"; return; }
        fi
    done
}

build_android() {
    echo "==> Android"
    command -v cargo-ndk >/dev/null || { echo "cargo-ndk fehlt: cargo install cargo-ndk"; exit 1; }
    local ndk
    ndk="$(find_ndk)"
    [ -n "$ndk" ] || { echo "Android-NDK nicht gefunden (ANDROID_NDK_HOME setzen)"; exit 1; }
    echo "    NDK: $ndk"
    # CMake (Opus-Build von audiopus_sys) findet das NDK nur über ANDROID_NDK_ROOT
    # und braucht einen Generator, den es ohne Toolchain-Datei auch findet.
    export ANDROID_NDK_HOME="$ndk" ANDROID_NDK_ROOT="$ndk" CMAKE_GENERATOR=Ninja

    ensure_targets aarch64-linux-android armv7-linux-androideabi x86_64-linux-android i686-linux-android
    local args=()
    for abi in "${ANDROID_ABIS[@]}"; do args+=(-t "$abi"); done

    rm -rf "$OUT/android"
    mkdir -p "$OUT/android/include"
    # --link-libcxx-shared: cpal/oboe ist C++ — libc++_shared.so wird verlinkt
    # und neben die .so in jniLibs/<abi>/ kopiert (muss mit in die App).
    cargo ndk "${args[@]}" --platform "$ANDROID_API" --link-libcxx-shared \
        -o "$OUT/android/jniLibs" build --release --lib
    cp include/$NAME.h "$OUT/android/include/"
    echo "    -> $OUT/android/jniLibs"
}

case "$WANT" in
    ios) build_ios ;;
    android) build_android ;;
    all) build_ios; build_android ;;
    *) echo "Aufruf: $0 [ios|android|all]"; exit 2 ;;
esac
