#!/usr/bin/env bash
set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
SERVER_DIR="$SCRIPT_DIR/server"
CLIENT_DIR="$SCRIPT_DIR/client"
LOG_FILE="$SCRIPT_DIR/log.txt"

VERBOSE=false
if [[ "$1" == "--v" || "$1" == "-v" || "$1" == "--verbose" ]]; then
    VERBOSE=true
fi

SERVER_PID=""
CLIENT_PID=""

cleanup() {
    echo ""
    echo "Shutting down..."
    [ -n "$CLIENT_PID" ] && kill "$CLIENT_PID" 2>/dev/null && echo "Client stopped."
    [ -n "$SERVER_PID" ] && kill "$SERVER_PID" 2>/dev/null && echo "Server stopped."
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

# Build client (Slint)
echo "=== Building client ==="
cd "$CLIENT_DIR"
cargo build --release 2>&1
echo ""

# Start server
echo "=== Starting server ==="
cd "$SERVER_DIR"
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

# Start Slint client
echo "=== Starting client ==="
cd "$CLIENT_DIR"
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
echo "Press Ctrl+C to stop both."
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
    sleep 1
done
