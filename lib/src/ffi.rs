//! C-API der Kern-Bibliothek — der Vertrag steht in `docs/klango.md`, Abschnitt 2.
//!
//! Ein Singleton (`CORE`) mit eigener Tokio-Runtime. Der Aufrufer (Klangos
//! Lua-Thread) ruft nur kurze, nicht blockierende Funktionen; alles Laufende
//! (WebSocket, UDP, Aufnahme, Dateistream) lebt auf der Runtime bzw. in eigenen
//! Threads. Ereignisse (Servernachrichten plus die synthetischen
//! `connect_failed`, `connection_lost`, `client_error`, `stream_finished`,
//! `upload_finished`, `download_finished`) sammeln sich als JSON-Zeichenketten
//! in einer Schlange, die `tc_poll_event` leert.
//!
//! Audio-Ausgabe: KEIN eigener Lautsprecher. Der Empfangsmischer aus
//! `net/udp_client.rs` legt seine 20-ms-Frames in `state.playback_tx`; hier
//! hängt daran statt `audio/playback.rs` ein Pump-Thread, der die Frames in
//! einen Ring schiebt (Deckel ~500 ms, Ältestes fliegt raus). `tc_read_audio`
//! leert den Ring. Das Format ist damit immer 48 kHz, 2 Kanäle, i16 LE.
//!
//! Die Logik der einzelnen Aufrufe ist aus `client/src/actions.rs` übernommen
//! (join_room, leave_room, toggle_mute, stream_file, …), nur ohne Oberfläche.

use std::collections::VecDeque;
use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int};
use std::sync::atomic::Ordering;
use std::sync::Arc;

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use parking_lot::Mutex;
use tokio::sync::mpsc;

use crate::protocol::Message;
use crate::state::{AppState, PendingUpload};

/// Ausgaberate des Mischers (Opus wird immer mit 48 kHz dekodiert).
const OUT_RATE: c_int = 48000;
const OUT_CHANNELS: c_int = 2;
/// Ring-Deckel: 500 ms bei 48 kHz, 2 Kanäle, 2 Byte je Sample.
const RING_MAX_BYTES: usize = 48000 * 2 * 2 / 2;

struct Core {
    /// Bleibt hier, damit die Runtime lebt; `tc_destroy` fährt sie herunter.
    rt: tokio::runtime::Runtime,
    state: Arc<AppState>,
    events: Arc<Mutex<VecDeque<String>>>,
    /// Sender der laufenden Verbindung (Ereignisse der Bibliothek selbst).
    ev_tx: Option<mpsc::UnboundedSender<Message>>,
    /// Weiterleitungs-Task der laufenden Verbindung — wird beim Trennen
    /// abgebrochen, damit ein verspätetes `connection_lost` der ALTEN
    /// Verbindung nicht in die Schlange der neuen fällt.
    forward: Option<tokio::task::JoinHandle<()>>,
    /// Empfangsring (gemischtes PCM), gefüllt vom Pump-Thread.
    ring: Arc<Mutex<VecDeque<u8>>>,
    /// Stopp-Signal für den Pump-Thread (er endet, sobald der Sender fällt).
    pump_live: Arc<std::sync::atomic::AtomicBool>,
    /// Beitritt über `room_join_group`: `audio_config` erst schicken, wenn
    /// `room_joined` (und die Raumparameter) da sind.
    group_join_pending: Arc<std::sync::atomic::AtomicBool>,
}

static CORE: Mutex<Option<Core>> = Mutex::new(None);
static LAST_ERROR: Mutex<Option<CString>> = Mutex::new(None);
static VERSION: &str = concat!("teamconference-core ", env!("CARGO_PKG_VERSION"), "\0");

fn set_error(msg: impl Into<String>) {
    let s: String = msg.into();
    tracing::warn!("[ffi] {}", s);
    *LAST_ERROR.lock() = CString::new(s).ok();
}

fn clear_error() {
    *LAST_ERROR.lock() = None;
}

/// C-String lesen; NULL → None.
unsafe fn cstr_opt(p: *const c_char) -> Option<String> {
    if p.is_null() {
        None
    } else {
        Some(CStr::from_ptr(p).to_string_lossy().into_owned())
    }
}

/// Makro: mit dem Singleton arbeiten oder mit `$fail` zurück.
macro_rules! with_core {
    ($c:ident, $fail:expr, $body:block) => {{
        let mut guard = CORE.lock();
        match guard.as_mut() {
            Some($c) => $body,
            None => {
                set_error("tc_create wurde nicht aufgerufen");
                $fail
            }
        }
    }};
}

// ---------------------------------------------------------------------------
// Lebenszyklus
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn tc_create() -> c_int {
    // Protokoll nur auf Wunsch (TC_LOG=debug o. ä.), sonst bleibt stderr still.
    if std::env::var("TC_LOG").is_ok() {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_env("TC_LOG"))
            .with_writer(std::io::stderr)
            .try_init();
    }
    let mut guard = CORE.lock();
    if guard.is_some() {
        return 1;
    }
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("tc-core")
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            set_error(format!("tokio-Runtime: {}", e));
            return 0;
        }
    };
    let state = Arc::new(AppState::new());
    // Der Mischer schreibt in diesen Kanal; der Pump-Thread trägt ihn in den Ring.
    let (ptx, prx) = crossbeam_channel::bounded::<Vec<u8>>(64);
    *state.playback_tx.lock() = Some(ptx);
    state.inner.lock().playback_device_channels = OUT_CHANNELS as u16;

    let ring = Arc::new(Mutex::new(VecDeque::with_capacity(RING_MAX_BYTES)));
    let pump_live = Arc::new(std::sync::atomic::AtomicBool::new(true));
    {
        let ring = ring.clone();
        let live = pump_live.clone();
        let st = state.clone();
        std::thread::Builder::new()
            .name("tc-pcm-pump".into())
            .spawn(move || {
                while live.load(Ordering::Relaxed) {
                    let frame = match prx.recv_timeout(std::time::Duration::from_millis(200)) {
                        Ok(f) => f,
                        Err(crossbeam_channel::RecvTimeoutError::Timeout) => continue,
                        Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
                    };
                    // Gesamtlautstärke — bei der Desktop-Wiedergabe tut das der
                    // cpal-Rückruf (playback.rs); hier gibt es den nicht.
                    let gain = st.volume();
                    let mut r = ring.lock();
                    if (gain - 1.0).abs() < f32::EPSILON {
                        r.extend(frame.iter().copied());
                    } else {
                        for c in frame.chunks_exact(2) {
                            let s = i16::from_le_bytes([c[0], c[1]]) as f32 * gain;
                            let s = s.clamp(-32768.0, 32767.0) as i16;
                            r.extend(s.to_le_bytes());
                        }
                    }
                    let over = r.len().saturating_sub(RING_MAX_BYTES);
                    if over > 0 {
                        // Ältestes verwerfen — an Sample-Grenzen (4 Byte je Frame).
                        let drop = ((over + 3) / 4 * 4).min(r.len());
                        r.drain(..drop);
                    }
                }
            })
            .ok();
    }

    *guard = Some(Core {
        rt,
        state,
        events: Arc::new(Mutex::new(VecDeque::new())),
        ev_tx: None,
        forward: None,
        ring,
        pump_live,
        group_join_pending: Arc::new(std::sync::atomic::AtomicBool::new(false)),
    });
    clear_error();
    1
}

#[no_mangle]
pub extern "C" fn tc_destroy() {
    tc_disconnect();
    let core = CORE.lock().take();
    if let Some(core) = core {
        core.pump_live.store(false, Ordering::Relaxed);
        *core.state.playback_tx.lock() = None;
        // Nicht auf Tasks warten, die nie enden würden (Aufnahme-Owner o. ä.).
        core.rt.shutdown_timeout(std::time::Duration::from_millis(500));
    }
}

#[no_mangle]
pub extern "C" fn tc_version() -> *const c_char {
    VERSION.as_ptr() as *const c_char
}

#[no_mangle]
pub extern "C" fn tc_last_error() -> *const c_char {
    static EMPTY: &[u8] = b"\0";
    match LAST_ERROR.lock().as_ref() {
        Some(s) => s.as_ptr(),
        None => EMPTY.as_ptr() as *const c_char,
    }
}

// ---------------------------------------------------------------------------
// Verbindung
// ---------------------------------------------------------------------------

/// Verbindung aufbauen — Nachbau von `actions.rs::establish_session`, ohne
/// Wiedergabe-Thread (den Ring füllt der Pump-Thread) und ohne Wiederverbindung.
async fn establish(
    state: Arc<AppState>,
    ev_tx: mpsc::UnboundedSender<Message>,
    host: String,
    port: u16,
    udp_port: u16,
    ssl: bool,
    login: serde_json::Value,
) {
    let fail = |text: String| {
        let _ = ev_tx.send(Message::new("connect_failed", serde_json::json!({ "message": text })));
    };
    if let Err(e) = crate::net::ws_client::connect(&host, port, ssl, state.clone(), ev_tx.clone()).await {
        fail(format!("Verbindung fehlgeschlagen: {}", e));
        return;
    }
    match crate::net::udp_client::start_udp_audio(&host, udp_port, state.clone()).await {
        Ok((send_shutdown, recv_shutdown)) => {
            let mut inner = state.inner.lock();
            inner.capture_shutdown = Some(send_shutdown);
            inner.playback_shutdown = Some(recv_shutdown);
        }
        Err(e) => {
            let _ = ev_tx.send(Message::new(
                "client_error",
                serde_json::json!({ "message": format!("UDP-Audio fehlgeschlagen: {}", e) }),
            ));
        }
    }
    if let Err(e) = state.send_ws(Message::new("auth_login", login)) {
        fail(format!("Anmeldung fehlgeschlagen: {}", e));
    }
}

/// Aufnahme und Dateistream anhalten (lokal, ohne Servernachricht).
fn stop_capture_and_stream(state: &Arc<AppState>) {
    let mut inner = state.inner.lock();
    if let Some(tx) = inner.capture_shutdown.take() {
        let _ = tx.send(true);
    }
    if let Some(tx) = inner.capture_stream_stop.take() {
        let _ = tx.send(());
    }
    if let Some(tx) = inner.stream_shutdown.take() {
        let _ = tx.send(true);
    }
    inner.capturing = false;
    inner.streaming_file = false;
    drop(inner);
    state.file_streaming.store(false, Ordering::Relaxed);
    state.stream_paused.store(false, Ordering::Relaxed);
}

/// `audio_config` für den aktuellen Raum schicken (Parameter des Raums, sonst
/// Vorgaben) — aus `actions.rs::join_room`.
fn send_audio_config(state: &Arc<AppState>, room_id: i64) {
    let (sr, bd, ch) = {
        let mut inner = state.inner.lock();
        let (mut sr, mut bd, mut ch, br) = inner
            .rooms
            .iter()
            .find(|r| r.id == room_id)
            .map(|r| (r.sample_rate, r.bit_depth, r.channels, r.bitrate))
            .unwrap_or((48000, 16, 1, 0));
        if sr <= 0 {
            sr = 48000;
        }
        if bd <= 0 {
            bd = 16;
        }
        if ch <= 0 {
            ch = 1;
        }
        inner.audio_config.sample_rate = sr as u32;
        inner.audio_config.bit_depth = bd as u8;
        inner.audio_config.channels = ch as u8;
        inner.audio_config.bitrate = br.max(0) as u32;
        (sr, bd, ch)
    };
    let _ = state.send_ws(Message::new(
        "audio_config",
        serde_json::json!({ "sample_rate": sr, "bit_depth": bd, "channels": ch, "enabled": true }),
    ));
    let _ = state.send_ws(Message::new("file_list", serde_json::json!({ "room_id": room_id })));
}

/// Ereignisse der Verbindung entgegennehmen: ein paar werden hier bearbeitet
/// (Upload-Stücke, Download-Zusammenbau, Gruppenbeitritt, Rauswurf), alle
/// landen als JSON in der Schlange.
async fn forward_events(
    mut rx: mpsc::UnboundedReceiver<Message>,
    state: Arc<AppState>,
    events: Arc<Mutex<VecDeque<String>>>,
    ev_tx: mpsc::UnboundedSender<Message>,
    group_join_pending: Arc<std::sync::atomic::AtomicBool>,
) {
    while let Some(msg) = rx.recv().await {
        match msg.msg_type.as_str() {
            "room_joined" => {
                let rid = msg.data.get("room_id").and_then(|v| v.as_i64()).unwrap_or(0);
                if rid != 0 && group_join_pending.swap(false, Ordering::SeqCst) {
                    state.inner.lock().current_room_id = Some(rid);
                    send_audio_config(&state, rid);
                }
            }
            "room_kicked" | "room_banned" | "room_closed" | "user_kicked" | "user_banned" => {
                // Der Server hat uns aus dem Raum genommen — lokal nachziehen.
                {
                    let mut inner = state.inner.lock();
                    inner.current_room_id = None;
                    inner.current_room_password = None;
                    inner.current_files.clear();
                }
                stop_capture_and_stream(&state);
            }
            "user_moved" => {
                if let Some(rid) = msg.data.get("room_id").and_then(|v| v.as_i64()) {
                    state.inner.lock().current_room_id = Some(rid);
                    send_audio_config(&state, rid);
                }
            }
            "file_upload_ack" => {
                if let Ok(ack) = serde_json::from_value::<crate::protocol::FileUploadAck>(msg.data.clone()) {
                    let pending = state.inner.lock().pending_upload.take();
                    match (ack.success, pending) {
                        (true, Some(upload)) => {
                            let st = state.clone();
                            let etx = ev_tx.clone();
                            tokio::spawn(async move {
                                const CHUNK: usize = 48 * 1024; // durch 3 teilbar → saubere Base64-Grenzen
                                let mut offset: i64 = 0;
                                let mut ok = true;
                                for chunk in upload.data.chunks(CHUNK) {
                                    let m = Message::new(
                                        "file_upload_chunk",
                                        serde_json::json!({
                                            "upload_id": ack.upload_id,
                                            "data": BASE64.encode(chunk),
                                            "offset": offset,
                                        }),
                                    );
                                    if st.send_ws(m).is_err() {
                                        ok = false;
                                        break;
                                    }
                                    offset += chunk.len() as i64;
                                }
                                if ok {
                                    let _ = st.send_ws(Message::new(
                                        "file_upload_complete",
                                        serde_json::json!({ "upload_id": ack.upload_id }),
                                    ));
                                }
                                let _ = etx.send(Message::new(
                                    "upload_finished",
                                    serde_json::json!({ "filename": upload.filename, "ok": ok }),
                                ));
                            });
                        }
                        (_, pending) => {
                            let name = pending.map(|p| p.filename).unwrap_or_default();
                            let _ = ev_tx.send(Message::new(
                                "upload_finished",
                                serde_json::json!({ "filename": name, "ok": false, "message": "Upload vom Server abgelehnt" }),
                            ));
                        }
                    }
                }
            }
            "file_download_data" => {
                if let Ok(data) = serde_json::from_value::<crate::protocol::FileDownloadData>(msg.data.clone()) {
                    let decoded = BASE64.decode(data.data.as_bytes()).unwrap_or_default();
                    let finished = {
                        let mut inner = state.inner.lock();
                        if let Some((_p, buf)) = inner.download_targets.get_mut(&data.file_id) {
                            buf.extend_from_slice(&decoded);
                            if buf.len() as i64 >= data.total {
                                inner.download_targets.remove(&data.file_id)
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    };
                    if let Some((path, buf)) = finished {
                        let (ok, message) = match std::fs::write(&path, &buf) {
                            Ok(()) => (true, String::new()),
                            Err(e) => (false, e.to_string()),
                        };
                        let _ = ev_tx.send(Message::new(
                            "download_finished",
                            serde_json::json!({ "file_id": data.file_id, "path": path.to_string_lossy(), "ok": ok, "message": message }),
                        ));
                    }
                    // Die rohen Stücke interessieren den Aufrufer nicht.
                    continue;
                }
            }
            "connection_lost" => {
                stop_capture_and_stream(&state);
                let mut inner = state.inner.lock();
                inner.current_room_id = None;
                inner.session_token = None;
                inner.audio_id = None;
            }
            _ => {}
        }
        if let Ok(json) = serde_json::to_string(&msg) {
            let mut q = events.lock();
            q.push_back(json);
            // Niemand holt ab? Dann nicht ins Unendliche wachsen.
            while q.len() > 4096 {
                q.pop_front();
            }
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn tc_connect(
    host: *const c_char,
    port: c_int,
    udp_port: c_int,
    ssl: c_int,
    login_json: *const c_char,
) -> c_int {
    let Some(host) = cstr_opt(host) else {
        set_error("host fehlt");
        return 0;
    };
    let login: serde_json::Value = match cstr_opt(login_json)
        .ok_or_else(|| "login_json fehlt".to_string())
        .and_then(|s| serde_json::from_str(&s).map_err(|e| format!("login_json: {}", e)))
    {
        Ok(v) => v,
        Err(e) => {
            set_error(e);
            return 0;
        }
    };
    if !(1..=65535).contains(&port) {
        set_error("ungültiger Port");
        return 0;
    }
    let udp_port = if (1..=65535).contains(&udp_port) { udp_port } else { port + 1 };

    // Eine alte Verbindung erst wegräumen.
    tc_disconnect();

    with_core!(c, 0, {
        let nick = login.get("nickname").and_then(|v| v.as_str()).unwrap_or("").to_string();
        c.state.inner.lock().nickname = nick;
        c.state.connect_gen.fetch_add(1, Ordering::SeqCst);
        c.group_join_pending.store(false, Ordering::SeqCst);

        let (ev_tx, ev_rx) = mpsc::unbounded_channel::<Message>();
        c.ev_tx = Some(ev_tx.clone());
        c.forward = Some(c.rt.spawn(forward_events(
            ev_rx,
            c.state.clone(),
            c.events.clone(),
            ev_tx.clone(),
            c.group_join_pending.clone(),
        )));
        let st = c.state.clone();
        c.rt.spawn(establish(st, ev_tx, host, port as u16, udp_port as u16, ssl != 0, login));
        clear_error();
        1
    })
}

#[no_mangle]
pub extern "C" fn tc_disconnect() {
    let mut guard = CORE.lock();
    let Some(c) = guard.as_mut() else { return };
    c.state.connect_gen.fetch_add(1, Ordering::SeqCst);
    stop_capture_and_stream(&c.state);
    {
        let mut inner = c.state.inner.lock();
        if let Some(tx) = inner.playback_shutdown.take() {
            let _ = tx.send(true);
        }
        // Sender fallen lassen → der Sende-Task endet; der Empfangs-Task endet,
        // sobald der Server die Verbindung schließt.
        inner.ws_tx = None;
        inner.connected = false;
        inner.authenticated = false;
        inner.user_id = None;
        inner.self_role = None;
        inner.session_token = None;
        inner.audio_id = None;
        inner.rooms.clear();
        inner.current_room_id = None;
        inner.current_room_password = None;
        inner.udp_socket = None;
        inner.server_udp_addr = None;
        inner.current_files.clear();
        inner.pending_upload = None;
        inner.download_targets.clear();
        inner.muted = false;
        inner.deafened = false;
        inner.loopback = false;
        inner.token_to_user.clear();
    }
    // Ereignisse der alten Verbindung nicht mehr annehmen.
    if let Some(h) = c.forward.take() {
        h.abort();
    }
    c.ev_tx = None;
    c.group_join_pending.store(false, Ordering::SeqCst);
    c.ring.lock().clear();
}

#[no_mangle]
pub extern "C" fn tc_is_connected() -> c_int {
    with_core!(c, 0, { c.state.inner.lock().connected as c_int })
}

#[no_mangle]
pub extern "C" fn tc_is_authenticated() -> c_int {
    with_core!(c, 0, { c.state.inner.lock().authenticated as c_int })
}

#[no_mangle]
pub extern "C" fn tc_user_id() -> i64 {
    with_core!(c, 0, { c.state.inner.lock().user_id.unwrap_or(0) })
}

// ---------------------------------------------------------------------------
// Steuerkanal
// ---------------------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn tc_send(json: *const c_char) -> c_int {
    let Some(s) = cstr_opt(json) else {
        set_error("json fehlt");
        return 0;
    };
    let msg: Message = match serde_json::from_str(&s) {
        Ok(m) => m,
        Err(e) => {
            set_error(format!("json: {}", e));
            return 0;
        }
    };
    with_core!(c, 0, {
        match c.state.send_ws(msg) {
            Ok(()) => 1,
            Err(e) => {
                set_error(e);
                0
            }
        }
    })
}

#[no_mangle]
pub unsafe extern "C" fn tc_poll_event(buf: *mut c_char, cap: c_int) -> c_int {
    if buf.is_null() || cap <= 0 {
        return 0;
    }
    with_core!(c, 0, {
        let mut q = c.events.lock();
        let Some(front) = q.front() else { return 0 };
        let need = front.len() + 1;
        if need > cap as usize {
            return -(need as c_int);
        }
        let s = q.pop_front().unwrap();
        std::ptr::copy_nonoverlapping(s.as_ptr(), buf as *mut u8, s.len());
        *buf.add(s.len()) = 0;
        s.len() as c_int
    })
}

// ---------------------------------------------------------------------------
// Räume
// ---------------------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn tc_join_room(room_id: i64, password: *const c_char) -> c_int {
    let password = cstr_opt(password).filter(|p| !p.is_empty());
    with_core!(c, 0, {
        let mut data = serde_json::json!({ "room_id": room_id });
        if let Some(pw) = &password {
            data["password"] = serde_json::Value::String(pw.clone());
        }
        if let Err(e) = c.state.send_ws(Message::new("room_join", data)) {
            set_error(e);
            return 0;
        }
        c.group_join_pending.store(false, Ordering::SeqCst);
        {
            let mut inner = c.state.inner.lock();
            inner.current_room_id = Some(room_id);
            inner.current_room_password = password;
        }
        send_audio_config(&c.state, room_id);
        clear_error();
        1
    })
}

#[no_mangle]
pub unsafe extern "C" fn tc_join_group_room(group_id: *const c_char, name: *const c_char) -> c_int {
    let Some(gid) = cstr_opt(group_id) else {
        set_error("group_id fehlt");
        return 0;
    };
    let name = cstr_opt(name).unwrap_or_default();
    with_core!(c, 0, {
        c.group_join_pending.store(true, Ordering::SeqCst);
        match c.state.send_ws(Message::new(
            "room_join_group",
            serde_json::json!({ "group_id": gid, "name": name }),
        )) {
            Ok(()) => {
                clear_error();
                1
            }
            Err(e) => {
                c.group_join_pending.store(false, Ordering::SeqCst);
                set_error(e);
                0
            }
        }
    })
}

#[no_mangle]
pub extern "C" fn tc_leave_room() {
    with_core!(c, (), {
        let room_id = c.state.inner.lock().current_room_id;
        if let Some(rid) = room_id {
            let _ = c.state.send_ws(Message::new("room_leave", serde_json::json!({ "room_id": rid })));
        }
        c.group_join_pending.store(false, Ordering::SeqCst);
        {
            let mut inner = c.state.inner.lock();
            inner.current_room_id = None;
            inner.current_room_password = None;
            inner.current_files.clear();
        }
        stop_capture_and_stream(&c.state);
        c.ring.lock().clear();
    })
}

#[no_mangle]
pub extern "C" fn tc_current_room() -> i64 {
    with_core!(c, 0, { c.state.inner.lock().current_room_id.unwrap_or(0) })
}

// ---------------------------------------------------------------------------
// Mikrofon / Ton
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn tc_set_mute(muted: c_int) {
    with_core!(c, (), {
        let m = muted != 0;
        c.state.inner.lock().muted = m;
        let _ = c.state.send_ws(Message::new("audio_mute", serde_json::json!({ "muted": m })));
    })
}

#[no_mangle]
pub extern "C" fn tc_get_mute() -> c_int {
    with_core!(c, 0, { c.state.inner.lock().muted as c_int })
}

#[no_mangle]
pub extern "C" fn tc_set_deafen(deafened: c_int) {
    with_core!(c, (), {
        let d = deafened != 0;
        c.state.inner.lock().deafened = d;
        let _ = c.state.send_ws(Message::new("audio_deafen", serde_json::json!({ "deafened": d })));
        if d {
            c.ring.lock().clear();
        }
    })
}

#[no_mangle]
pub extern "C" fn tc_get_deafen() -> c_int {
    with_core!(c, 0, { c.state.inner.lock().deafened as c_int })
}

#[no_mangle]
pub extern "C" fn tc_set_volume(gain: f32) {
    with_core!(c, (), { c.state.set_volume(gain) })
}

#[no_mangle]
pub extern "C" fn tc_set_user_volume(user_id: i64, gain: f32) {
    with_core!(c, (), {
        c.state.inner.lock().user_volumes.insert(user_id, gain.clamp(0.0, 2.0));
    })
}

#[no_mangle]
pub unsafe extern "C" fn tc_set_input_device(name: *const c_char) -> c_int {
    let name = cstr_opt(name).filter(|n| !n.is_empty());
    with_core!(c, 0, {
        c.state.inner.lock().input_device = name;
        1
    })
}

#[no_mangle]
pub unsafe extern "C" fn tc_list_input_devices(buf: *mut c_char, cap: c_int) -> c_int {
    if buf.is_null() || cap <= 0 {
        return 0;
    }
    let names: Vec<String> = crate::audio::device::list_devices()
        .into_iter()
        .filter(|d| d.is_input)
        .map(|d| d.name)
        .collect();
    let json = serde_json::to_string(&names).unwrap_or_else(|_| "[]".into());
    let need = json.len() + 1;
    if need > cap as usize {
        return -(need as c_int);
    }
    std::ptr::copy_nonoverlapping(json.as_ptr(), buf as *mut u8, json.len());
    *buf.add(json.len()) = 0;
    json.len() as c_int
}

// ---------------------------------------------------------------------------
// Dateistream
// ---------------------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn tc_stream_file(path: *const c_char) -> c_int {
    let Some(path) = cstr_opt(path) else {
        set_error("path fehlt");
        return 0;
    };
    let path = std::path::PathBuf::from(path);
    if !path.is_file() {
        set_error(format!("Datei nicht gefunden: {}", path.display()));
        return 0;
    }
    with_core!(c, 0, {
        if !c.state.inner.lock().connected {
            set_error("Nicht verbunden");
            return 0;
        }
        let Some(ev_tx) = c.ev_tx.clone() else {
            set_error("Nicht verbunden");
            return 0;
        };
        {
            let inner = c.state.inner.lock();
            if let Some(ref tx) = inner.stream_shutdown {
                let _ = tx.send(true);
            }
        }
        c.state.stream_paused.store(false, Ordering::Relaxed);
        c.state.set_stream_volume(1.0);
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        {
            let mut inner = c.state.inner.lock();
            inner.stream_shutdown = Some(shutdown_tx);
            inner.streaming_file = true;
        }
        let st = c.state.clone();
        c.rt.spawn(async move {
            let result = crate::audio::file_stream::stream_audio_file(&path, st.clone(), shutdown_rx).await;
            if let Err(e) = result {
                let _ = ev_tx.send(Message::new(
                    "client_error",
                    serde_json::json!({ "message": format!("Streaming-Fehler: {}", e) }),
                ));
            }
            {
                let mut inner = st.inner.lock();
                inner.stream_shutdown = None;
                inner.streaming_file = false;
            }
            let _ = ev_tx.send(Message::new("stream_finished", serde_json::json!({})));
        });
        clear_error();
        1
    })
}

#[no_mangle]
pub extern "C" fn tc_stream_stop() {
    with_core!(c, (), {
        let mut inner = c.state.inner.lock();
        if let Some(tx) = inner.stream_shutdown.take() {
            let _ = tx.send(true);
        }
        inner.streaming_file = false;
        drop(inner);
        c.state.stream_paused.store(false, Ordering::Relaxed);
    })
}

#[no_mangle]
pub extern "C" fn tc_stream_pause(paused: c_int) {
    with_core!(c, (), {
        if c.state.inner.lock().stream_shutdown.is_some() {
            c.state.stream_paused.store(paused != 0, Ordering::Relaxed);
        }
    })
}

#[no_mangle]
pub extern "C" fn tc_stream_is_paused() -> c_int {
    with_core!(c, 0, { c.state.stream_paused.load(Ordering::Relaxed) as c_int })
}

#[no_mangle]
pub extern "C" fn tc_stream_seek(delta_seconds: c_int) {
    with_core!(c, (), {
        if c.state.inner.lock().streaming_file {
            c.state.request_stream_seek(delta_seconds);
        }
    })
}

#[no_mangle]
pub extern "C" fn tc_stream_set_volume(gain: f32) {
    with_core!(c, (), { c.state.set_stream_volume(gain) })
}

#[no_mangle]
pub extern "C" fn tc_stream_is_active() -> c_int {
    with_core!(c, 0, { c.state.inner.lock().streaming_file as c_int })
}

// ---------------------------------------------------------------------------
// Audio-Ausgabe
// ---------------------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn tc_audio_format(sample_rate: *mut c_int, channels: *mut c_int) {
    if !sample_rate.is_null() {
        *sample_rate = OUT_RATE;
    }
    if !channels.is_null() {
        *channels = OUT_CHANNELS;
    }
}

#[no_mangle]
pub unsafe extern "C" fn tc_read_audio(buf: *mut u8, cap: c_int) -> c_int {
    if buf.is_null() || cap <= 0 {
        return 0;
    }
    with_core!(c, 0, {
        let mut r = c.ring.lock();
        // Nur ganze Frames (2 Kanäle × i16 = 4 Byte) herausgeben.
        let n = (r.len().min(cap as usize)) / 4 * 4;
        if n == 0 {
            return 0;
        }
        for (i, b) in r.drain(..n).enumerate() {
            *buf.add(i) = b;
        }
        n as c_int
    })
}

#[no_mangle]
pub extern "C" fn tc_clear_audio() {
    with_core!(c, (), { c.ring.lock().clear() })
}

// ---------------------------------------------------------------------------
// Dateien im Raum
// ---------------------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn tc_upload_file(room_id: i64, path: *const c_char) -> c_int {
    let Some(path) = cstr_opt(path) else {
        set_error("path fehlt");
        return 0;
    };
    let path = std::path::PathBuf::from(path);
    let filename = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let data = match std::fs::read(&path) {
        Ok(d) => d,
        Err(e) => {
            set_error(format!("Datei konnte nicht gelesen werden: {}", e));
            return 0;
        }
    };
    with_core!(c, 0, {
        let size = data.len() as i64;
        c.state.inner.lock().pending_upload = Some(PendingUpload { filename: filename.clone(), data });
        match c.state.send_ws(Message::new(
            "file_upload_start",
            serde_json::json!({ "room_id": room_id, "filename": filename, "size": size }),
        )) {
            Ok(()) => 1,
            Err(e) => {
                c.state.inner.lock().pending_upload = None;
                set_error(e);
                0
            }
        }
    })
}

#[no_mangle]
pub unsafe extern "C" fn tc_download_file(file_id: i64, dest_path: *const c_char) -> c_int {
    let Some(dest) = cstr_opt(dest_path) else {
        set_error("dest_path fehlt");
        return 0;
    };
    with_core!(c, 0, {
        c.state
            .inner
            .lock()
            .download_targets
            .insert(file_id, (std::path::PathBuf::from(dest), Vec::new()));
        match c.state.send_ws(Message::new("file_download", serde_json::json!({ "file_id": file_id }))) {
            Ok(()) => 1,
            Err(e) => {
                c.state.inner.lock().download_targets.remove(&file_id);
                set_error(e);
                0
            }
        }
    })
}
