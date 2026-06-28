#!/usr/bin/env bash
set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
SERVER_DIR="$SCRIPT_DIR/server"
CLIENT_DIR="$SCRIPT_DIR/client"
HUB_DIR="$SCRIPT_DIR/../TCServerHub"
LOG_FILE="$SCRIPT_DIR/log.txt"

VERBOSE=false
HUB=false
for arg in "$@"; do
    case "$arg" in
        --v|-v|--verbose) VERBOSE=true ;;
        --hub) HUB=true ;;
    esac
done

# Lokale Hub-/Debug-URL: damit der zentrale Login beim Entwickeln gegen
# localhost statt gegen srvhub.accessy.org läuft. Client UND Server nutzen sie.
HUB_PORT=8099
HUB_URL="http://127.0.0.1:$HUB_PORT"

SERVER_PID=""
CLIENT_PID=""
HUB_PID=""

cleanup() {
    echo ""
    echo "Shutting down..."
    [ -n "$CLIENT_PID" ] && kill "$CLIENT_PID" 2>/dev/null && echo "Client stopped."
    [ -n "$SERVER_PID" ] && kill "$SERVER_PID" 2>/dev/null && echo "Server stopped."
    [ -n "$HUB_PID" ] && kill "$HUB_PID" 2>/dev/null && echo "Hub-Login stopped."
    wait 2>/dev/null
    if $VERBOSE; then
        echo "Logs written to $LOG_FILE"
    fi
    echo "Done."
    exit 0
}

trap cleanup EXIT INT TERM

if $VERBOSE; then
    echo "=== VERBOSE MODE — logging to $LOG_FILE ==="
    > "$LOG_FILE"  # truncate log file
    echo "--- TeamConference verbose log $(date) ---" >> "$LOG_FILE"
    echo ""
fi

# Build server
echo "=== Building server ==="
cd "$SERVER_DIR"
cargo build --release 2>&1
echo ""

# Build client (wxDragon)
# Auf macOS kann der wxWidgets-/wxDragon-CMake-Build mit sehr neuen
# Apple-Clang-Versionen (z. B. aus einer Xcode-Beta) an libc++ scheitern; dann
# auf die stabileren Command Line Tools ausweichen. Siehe compile.sh.
echo "=== Building client ==="
cd "$CLIENT_DIR"
CLT_DIR="/Library/Developer/CommandLineTools"
if cargo build --release 2>&1; then
    :
elif [ "$(uname -s)" = "Darwin" ] && [ -z "${DEVELOPER_DIR:-}" ] \
        && [ -x "$CLT_DIR/usr/bin/clang" ]; then
    echo "Client-Build mit aktiver Toolchain fehlgeschlagen — Wiederholung mit Command Line Tools…"
    export DEVELOPER_DIR="$CLT_DIR"
    cargo build --release 2>&1
else
    exit 1
fi
echo ""

# Optional: zentralen Login-Server (TCServerHub) lokal starten und Server+Client
# darauf zeigen lassen (Debug). Aktivieren mit:  ./start.sh --hub
if $HUB; then
    if [ ! -d "$HUB_DIR" ]; then
        echo "WARNUNG: $HUB_DIR nicht gefunden — --hub wird ignoriert."
        HUB=false
    else
        echo "=== Building hub login (TCServerHub) ==="
        cd "$HUB_DIR"
        cargo build --release 2>&1
        echo ""
        echo "=== Starting hub login on $HUB_URL ==="
        SRVHUB_HTTP_BIND="127.0.0.1:$HUB_PORT" \
        SRVHUB_DB_PATH="$HUB_DIR/data/dev-srvhub.db" \
        SRVHUB_KEY_PATH="$HUB_DIR/data/dev-srvhub_ed25519" \
        SRVHUB_PUBLIC_URL="$HUB_URL" \
        SRVHUB_REQUIRE_APPROVAL=false \
        SRVHUB_ADMIN_USERNAME=felix SRVHUB_ADMIN_PHONE=+491731684221 SRVHUB_ADMIN_PASSWORD=Lena-2006 \
        "$HUB_DIR/target/release/srvhub" &
        HUB_PID=$!
        sleep 2
        if ! kill -0 "$HUB_PID" 2>/dev/null; then
            echo "ERROR: Hub-Login konnte nicht starten."; exit 1
        fi
        echo "Hub-Login läuft (PID $HUB_PID). SMS ist im Dev-Modus ohne Prelude-Key inaktiv;"
        echo "Admin-Login (felix) und Token-Prüfung funktionieren."
        echo ""
    fi
fi

# Start server
echo "=== Starting server ==="
cd "$SERVER_DIR"
# Im --hub-Modus zentrales Login gegen die lokale URL aktivieren.
if $HUB; then
    export TC_CENTRAL_LOGIN=true
    export TC_CENTRAL_LOGIN_URL="$HUB_URL"
fi
if $VERBOSE; then
    RUST_LOG=debug "$SERVER_DIR/target/release/teamconference-server" --config config.default.toml --create-admin >> "$LOG_FILE" 2>&1 &
else
    "$SERVER_DIR/target/release/teamconference-server" --config config.default.toml --create-admin &
fi
SERVER_PID=$!
sleep 3

if ! kill -0 "$SERVER_PID" 2>/dev/null; then
    echo "ERROR: Server failed to start."
    if $VERBOSE; then
        echo "Check $LOG_FILE for details."
    fi
    exit 1
fi
echo "Server running (PID $SERVER_PID)"
echo ""

# Start client. Im --hub-Modus zeigt der Client auf die lokale Hub-URL.
echo "=== Starting client ==="
cd "$CLIENT_DIR"
if $HUB; then
    export SRVHUB_BASE_URL="$HUB_URL"
fi
if $VERBOSE; then
    RUST_LOG=debug "$CLIENT_DIR/target/release/teamconference-client" >> "$LOG_FILE" 2>&1 &
else
    "$CLIENT_DIR/target/release/teamconference-client" &
fi
CLIENT_PID=$!
echo "Client running (PID $CLIENT_PID)"
echo ""

echo "=== TeamConference running ==="
echo "Login: admin / admin (über --create-admin angelegt)"
if $HUB; then
    echo "Zentrales Login aktiv gegen $HUB_URL (Hub-Admin: felix / Lena-2006)"
fi
echo "Press Ctrl+C to stop."
if $VERBOSE; then
    echo "Verbose logging to: $LOG_FILE"
    echo "Use 'tail -f $LOG_FILE' in another terminal to watch live."
fi
echo ""

# Wait for either process to exit
while true; do
    if ! kill -0 "$SERVER_PID" 2>/dev/null; then
        echo "Server exited."
        break
    fi
    if ! kill -0 "$CLIENT_PID" 2>/dev/null; then
        echo "Client exited."
        break
    fi
    if $HUB && [ -n "$HUB_PID" ] && ! kill -0 "$HUB_PID" 2>/dev/null; then
        echo "Hub-Login exited."
        break
    fi
    sleep 1
done
