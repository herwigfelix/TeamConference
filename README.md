# TeamConference

A TeamSpeak/Mumble-style voice conferencing application with:
- **Server** in Rust (Tokio) - Windows, macOS, Linux + Docker
- **Client** in Rust (Slint) - Windows, macOS, Linux — accessibility-focused (screen reader support, full keyboard control)
- Opus-encoded audio over UDP (raw PCM fallback) with adaptive jitter buffer
- SSL/TLS encryption, hierarchical rooms, chat, file sharing, admin functions

## Quick Start

The easiest way — build and start server + client together:

```bash
./start.sh        # macOS / Linux
start.bat         # Windows
```

Both scripts create a default admin user (`admin` / `admin`) for local development.

### Build requirements

- Rust (stable) — on Windows the MSVC toolchain with Build Tools
- CMake — needed to build the bundled Opus codec
- Linux only: ALSA dev packages (`libasound2-dev`) and the usual GUI libs for Slint/winit

### Server

```bash
cd server

# First run: creates admin user
cargo run -- --config config.default.toml --create-admin

# Subsequent runs
cargo run -- --config config.default.toml
```

Default ports:
- **9500/TCP** - Control channel (WebSocket, optional TLS)
- **9501/UDP** - Audio channel

Self-signed TLS certificates are generated automatically on first start
(pure Rust via rcgen, no openssl binary needed).

A default admin can also be created via environment variables
(`TC_ADMIN_USERNAME` / `TC_ADMIN_PASSWORD`) — used by the Docker setup.

### Client

```bash
cd client
cargo run --release
```

See [client/README.md](client/README.md) for the keyboard shortcuts and
accessibility documentation (German).

### Docker (server)

```bash
# Admin-Zugangsdaten in docker-compose.yml anpassen, dann im Projektstamm:
docker compose up -d --build
```

Database, uploads and TLS certificates are kept in named volumes
(`teamconference-data`, `teamconference-certs`); the server config is
bind-mounted from `docker/config.toml`.

## Architecture

### Server (Rust)

| Module | Purpose |
|--------|---------|
| `control/` | WebSocket server, message routing, auth |
| `audio/` | UDP audio relay, file streaming |
| `room/` | Hierarchical room management |
| `user/` | Online user sessions, permissions |
| `chat/` | Room chat, private messages, offline messages |
| `files/` | File upload/download (chunked, base64) |
| `admin/` | Kick, ban, move, mute |
| `db/` | SQLite with Argon2 password hashing |

### Client (Rust/Slint, `client/`)

| Module | Purpose |
|--------|---------|
| `ui/main.slint` | Accessible UI: chat, rooms, users, volume, files, menus |
| `net/` | WebSocket control + UDP audio (Opus) |
| `audio/` | Capture, playback (cpal), file streaming (Symphonia) |
| `events.rs` | Server events → UI updates |
| `actions.rs` | Menu/shortcut/dialog actions → protocol messages |

Older clients (`client-tauri/`, `legazy client(abandoned)/`) are deprecated.

## Configuration

### Server (`config.default.toml`)

- Network ports, TLS settings (auto-generate self-signed certs)
- Audio defaults (sample rate, bit depth, channels)
- Storage paths, max upload size
- Logging level

### Client

Stored in the platform config dir (Linux: `~/.config/teamconference/client.json`,
macOS: `~/Library/Application Support/teamconference/client.json`,
Windows: `%APPDATA%\teamconference\client.json`):

- Server, username, nickname
- Audio device selection, playback volume

## Protocol

See [docs/protocol.md](docs/protocol.md) for the full protocol specification.

- **Control Channel**: JSON over WebSocket (optional TLS)
- **Audio Channel**: Binary UDP packets with 22-byte header, Opus payload (bit_depth=0) or raw PCM

## Features

- Hierarchical rooms with passwords and user limits
- Room chat, private messages, server broadcasts
- File upload/download per room
- Audio file streaming into rooms (MP3, WAV, OGG, FLAC, M4A, …)
- Admin: kick, ban (timed/permanent), move, mute users
- Accessibility: screen reader labels (AccessKit), full keyboard operation,
  shortcuts (Ctrl/Cmd+M mute, Ctrl/Cmd+S stream file, …), German UI
- Adaptive jitter buffer, Opus encoding with PCM fallback
