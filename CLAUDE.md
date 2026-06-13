# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

TeamConference is a self-hosted TeamSpeak/Mumble-style voice app with an **accessibility-first** client. Two Rust crates: `server/` and `client/`. Replies and UI strings are German.

## Build & run

```sh
# Whole stack (build server+client, start both; creates admin/admin):
./start.sh            # macOS/Linux
start.bat             # Windows

# Individually:
cd server && cargo run -- --config config.default.toml --create-admin   # first run
cd server && cargo run -- --config config.default.toml
cd client && cargo run --release

# Release packaging → dist/ (.dmg on macOS, .zip on Windows, .tar.gz on Linux):
./compile.sh          # macOS/Linux
compile.bat           # Windows

# Server in Docker (config entirely via TC_* env vars in docker-compose.yml):
docker compose up -d --build
```

**CMake + a C++ compiler + LLVM/libclang are mandatory** for the client: `wxdragon` (wxWidgets) and `opus`/`audiopus_sys` build their C/C++ from source on first build (slow, then cached), and `wxdragon-sys` runs `bindgen` which needs libclang. If `cmake` is missing, `pip install --user cmake` provides a usable binary (`~/.local/bin/cmake`); put it on `PATH`. libclang ships with Xcode CLT (macOS); on Windows install LLVM (`winget install LLVM.LLVM`, set `LIBCLANG_PATH` if bindgen can't find it); on Linux `libclang-dev` + `libgtk-3-dev` + `libasound2-dev`. `client/.cargo/config.toml` sets `CMAKE_POLICY_VERSION_MINIMUM=3.5` so CMake 4.x accepts the vendored Opus build — don't remove it. Windows also embeds an app manifest via `client/build.rs` (`embed-manifest`) — required so wxWidgets finds Common Controls v6 and doesn't warn about a missing manifest.

`cargo test` works in both crates but coverage is minimal. The server's account/registration protocol is best verified end-to-end with a small Python `websockets` script that connects to `wss://localhost:9500` (accept self-signed cert) and exercises `auth_login` + `account_*` messages — that's how that feature was validated.

## Architecture

**Two wire channels** (see `docs/protocol.md`): a **control channel** = JSON over WebSocket (optional TLS) `{"type","id","data"}`, and an **audio channel** = binary UDP packets with a 22-byte header (magic `TCON`) carrying Opus (`bit_depth=0`) or raw PCM. Audio port = control port + 1.

**Server (`server/src/`)** — Tokio. `control/handler.rs` is the per-connection message loop and the place to add new message types (admin actions check `is_admin(&state, uid)`; replies go via `tx.send`). `control/auth.rs` handles login + self-registration. `db/` is SQLite (`rusqlite`, Argon2 hashes); the `settings` key-value table holds the runtime `registration_open` flag. `room/`, `user/`, `chat/`, `files/`, `admin/`, `audio/` are the subsystems. All config has built-in defaults and is overridable by `config.toml` **and** `TC_*` env vars (env wins) — so the server runs with no config file.

**Client (`client/src/`)** — native wxWidgets UI via `wxdragon` (chosen over Slint because Slint's macOS accessibility was broken; native widgets give native UIA/MSAA/NSAccessibility/ATK). Threading model is the key constraint:

- wxWidgets event loop runs on the **main thread**; widgets are `!Send` and must only be touched there.
- A **Tokio runtime** runs network/audio on background threads.
- Server→UI messages cross the boundary as plain `protocol::Message` data through an `mpsc` channel that a **wxWidgets `Timer` (~30 ms) drains on the UI thread** (`handlers::handle`). Never move widgets into Tokio tasks.

Module roles: `ui.rs` builds the frame/menus/widgets and holds the `Ui` struct (all widgets are `Copy`); `app.rs` defines `Ctx` (clones into every event closure: `Ui` + `Arc<AppState>` + Tokio handle + `ev_tx` + `Rc<RefCell<UiState>>`); `handlers.rs` turns incoming messages into widget updates and rebuilds the room/file views; `actions.rs` turns menu/button/dialog input into protocol messages; `net/` (WebSocket+UDP) and `audio/` (cpal capture/playback, Symphonia file streaming, Opus) are **UI-agnostic and shared** — `net/ws_client.rs::pre_handle_message` updates `AppState` on the network thread; the UI layer only renders.

`state::AppState` (`Arc<Mutex<InnerState>>` + atomics) is the single shared state between UI and network/audio. The rooms+users tree uses **`DataViewTreeCtrl`** (native NSOutlineView/GTK — VoiceOver-accessible); since it can't store item data, `UiState.tree_map` maps the `DataViewItem` pointer (as `usize`) → `NodeRef`.

## Conventions & gotchas

- **Menu accelerators**: define once as `\tCtrl+X` in `ui.rs`; wxWidgets maps Ctrl→Cmd on macOS automatically. Current bindings live in `build_menu_bar`; keep them collision-free.
- **macOS mic permission** only prompts from the bundled `.app` (built by `compile.sh`, which embeds `NSMicrophoneUsageDescription` and ad-hoc-signs it). Bare `cargo run` won't prompt.
- **Client config** persists to `<config_dir>/accessyApplications/teamconference/client.json` (Windows `%APPDATA%`, macOS `~/Library/Application Support`, Linux `~/.config`).
- **TLS**: server auto-generates a self-signed cert via `rcgen` (pure Rust, no `openssl` binary); the client accepts any cert.
- **Accessibility is a core requirement**, not optional: keep everything keyboard-reachable, label controls, and mirror status changes into the chat log so screen-reader users can read them.
- The active client is `client/`. `client-tauri/` and `legazy client(abandoned)/` are dead and git-ignored — read them only as protocol references, never edit.
