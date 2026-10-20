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

# iOS/iPadOS client (MUi) → TestFlight: device build + Xcode shell (xcodegen) + upload.
# `cargo mui ios` only builds for the simulator. Build number auto-increments (packaging/ios/build-number).
packaging/ios/build-appstore.sh [build] [--no-upload]

# Core library (lib/, C-API in lib/include/teamconference_core.h) for iOS/Android → dist/mobile/:
lib/build-mobile.sh [ios|android]
```

`lib/` is the UI-less core (protocol/net/audio pulled from `client/src` via `#[path]`) as `staticlib`/`cdylib`. iOS builds need a uniform `IPHONEOS_DEPLOYMENT_TARGET` (script sets 13.0; otherwise linking fails on `___chkstk_darwin`) and link only `AudioToolbox.framework`. Android needs the NDK + `cargo-ndk`; cpal/oboe is C++, so `libc++_shared.so` ships next to the `.so`, and the host must call `tc_android_init(JavaVM*, context)` before the first room join or capture aborts.

**CMake + a C++ compiler + LLVM/libclang are mandatory** for the client: `wxdragon` (wxWidgets) and `opus`/`audiopus_sys` build their C/C++ from source on first build (slow, then cached), and `wxdragon-sys` runs `bindgen` which needs libclang. If `cmake` is missing, `pip install --user cmake` provides a usable binary (`~/.local/bin/cmake`); put it on `PATH`. wxdragon-sys also builds wxWidgets via CMake with the **Ninja** generator, so Ninja must be installed. libclang ships with Xcode CLT (macOS); on Windows install LLVM (`winget install LLVM.LLVM`, set `LIBCLANG_PATH` if bindgen can't find it) and Ninja (`winget install Ninja-build.Ninja`); on Linux `libclang-dev` + `ninja-build` + `libgtk-3-dev` + `libasound2-dev`. `client/.cargo/config.toml` sets `CMAKE_POLICY_VERSION_MINIMUM=3.5` so CMake 4.x accepts the vendored Opus build — don't remove it. Windows also embeds an app manifest via `client/build.rs` (`embed-manifest`) — required so wxWidgets finds Common Controls v6 and doesn't warn about a missing manifest.

`cargo test` works in both crates but coverage is minimal. The server's account/registration protocol is best verified end-to-end with a small Python `websockets` script that connects to `wss://localhost:9500` (accept self-signed cert) and exercises `auth_login` + `account_*` messages — that's how that feature was validated.

## Architecture

**Two wire channels** (see `docs/protocol.md`): a **control channel** = JSON over WebSocket (optional TLS) `{"type","id","data"}`, and an **audio channel** = binary UDP packets with a 22-byte header (magic `TCON`) carrying Opus (`bit_depth=0`) or raw PCM. Audio port = control port + 1.

**Server (`server/src/`)** — Tokio. `control/handler.rs` is the per-connection message loop and the place to add new message types (admin actions check `is_admin(&state, uid)`; replies go via `tx.send`). `control/auth.rs` handles login + self-registration. `db/` is SQLite (`rusqlite`, Argon2 hashes); the `settings` key-value table holds the runtime `registration_open` flag. `room/`, `user/`, `chat/`, `files/`, `admin/`, `audio/` are the subsystems. All config has built-in defaults and is overridable by `config.toml` **and** `TC_*` env vars (env wins) — so the server runs with no config file.

**Client (`client/src/`)** — native wxWidgets UI via `wxdragon` (chosen over Slint because Slint's macOS accessibility was broken; native widgets give native UIA/MSAA/NSAccessibility/ATK). Threading model is the key constraint:

- wxWidgets event loop runs on the **main thread**; widgets are `!Send` and must only be touched there.
- A **Tokio runtime** runs network/audio on background threads.
- Server→UI messages cross the boundary as plain `protocol::Message` data through an `mpsc` channel that a **wxWidgets `Timer` (~30 ms) drains on the UI thread** (`handlers::handle`). Never move widgets into Tokio tasks.

Module roles: `ui.rs` builds the frame/menus/widgets and holds the `Ui` struct (all widgets are `Copy`); `app.rs` defines `Ctx` (clones into every event closure: `Ui` + `Arc<AppState>` + Tokio handle + `ev_tx` + `Rc<RefCell<UiState>>`); `handlers.rs` turns incoming messages into widget updates and rebuilds the room/file views; `actions.rs` turns menu/button/dialog input into protocol messages; `net/` (WebSocket+UDP) and `audio/` (cpal capture/playback, Symphonia file streaming, Opus) are **UI-agnostic and shared** — `net/ws_client.rs::pre_handle_message` updates `AppState` on the network thread; the UI layer only renders.

`state::AppState` (`Arc<Mutex<InnerState>>` + atomics) is the single shared state between UI and network/audio. Rooms+users are a tree, implemented in `roomtree.rs` with the **platform-native** widget per OS because neither tree widget is accessible everywhere: **Windows → `wxTreeCtrl`** (native SysTreeView32/MSAA), **macOS/Linux → `wxDataViewTreeCtrl`** (native NSOutlineView/GtkTreeView). Both expose one API (`build`/`rebuild`/`selected`); `roomtree` is cfg-gated, so the inactive branch isn't compiled on the host — verify the other branch by temporarily swapping the `#[cfg]` conditions and building. The DataView path maps `DataViewItem` ptr→`NodeRef` in `UiState.tree_map`; the TreeCtrl path stores `NodeRef` as item data. On Windows there is no join button — `on_item_activated` (Enter/double-click) joins; macOS/Linux also have a join button.

## Conventions & gotchas

- **Menu accelerators**: define once as `\tCtrl+X` in `ui.rs`; wxWidgets maps Ctrl→Cmd on macOS automatically. Current bindings live in `build_menu_bar`; keep them collision-free.
- **macOS mic permission** only prompts from the bundled `.app` (built by `compile.sh`, which embeds `NSMicrophoneUsageDescription` and ad-hoc-signs it). Bare `cargo run` won't prompt.
- **Client config** persists to `<config_dir>/accessyApplications/teamconference/client.json` (Windows `%APPDATA%`, macOS `~/Library/Application Support`, Linux `~/.config`).
- **TLS**: server auto-generates a self-signed cert via `rcgen` (pure Rust, no `openssl` binary); the client accepts any cert.
- **Layout helpers** live in `ui.rs` (`group`, `form_grid`/`form_row`, `button_row`, `dialog_buttons`, `compact()`): wide screens get two-column forms, phones (`compact()`) get labels above fields, stacked full-width buttons and the main view as pages (Räume / Chat / Audio & Dateien). Use them for new forms and dialogs. On macOS `set_a11y_name` sets the NSView's `accessibilityLabel` (wxAccessible only works on Windows).
- **Mobile back buttons**: sub-pages (e.g. hub register/reset in the `SimpleBook`) use MUi's `Button::set_navigation_back(true)` → on iOS the button becomes the navigation-bar back button (VoiceOver back gesture works). Move focus explicitly when switching pages (see `actions::set_hub_view`).
- **Voice processing** (`audio/voice.rs`, crate `sonora`): echo cancellation + noise suppression run on the mic in `capture.rs`; the receive mixer in `udp_client.rs` feeds the speaker signal (minus own loopback) as echo reference into the same processor. Local mixer sources are keyed by source id — loopback (`SOURCE_MIC`) and streamed file (`SOURCE_FILE`) must never share a queue.
- **iOS microphone choice** goes through `mui::audio_session::inputs()` / `set_preferred_input()` (cpal only sees "Default Device" on iOS).
- **Accessibility is a core requirement**, not optional: keep everything keyboard-reachable, label controls, and mirror status changes into the chat log so screen-reader users can read them.
- The active client is `client/`. `client-tauri/` and `legazy client(abandoned)/` are dead and git-ignored — read them only as protocol references, never edit.
