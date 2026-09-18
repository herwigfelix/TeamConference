//! Rauchtest der C-API gegen einen laufenden TeamConference-Server — geht
//! den Weg eines fremden Hosts: Bibliothek per dlopen
//! laden (KEINE Rust-Abhängigkeit zum Kern, damit wirklich die exportierten
//! Symbole geprüft werden), anmelden, Standardraum betreten, chatten, eine
//! Datei streamen und das gemischte PCM aus `tc_read_audio` lesen.
//!
//!   cargo build --release
//!   cargo run --release --example smoke -- <dylib> <host> <port> <audiodatei>
//!
//! Voraussetzung: Server mit admin/admin (`--create-admin`) auf host:port (TLS).

use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int};
use std::time::{Duration, Instant};

type FnI = unsafe extern "C" fn() -> c_int;
type FnV = unsafe extern "C" fn();
type FnCStr = unsafe extern "C" fn() -> *const c_char;
type FnStr = unsafe extern "C" fn(*const c_char) -> c_int;
type FnBuf = unsafe extern "C" fn(*mut c_char, c_int) -> c_int;

struct Api {
    create: FnI,
    destroy: FnV,
    version: FnCStr,
    last_error: FnCStr,
    connect: unsafe extern "C" fn(*const c_char, c_int, c_int, c_int, *const c_char) -> c_int,
    disconnect: FnV,
    is_authenticated: FnI,
    send: FnStr,
    poll_event: FnBuf,
    join_room: unsafe extern "C" fn(i64, *const c_char) -> c_int,
    leave_room: FnV,
    current_room: unsafe extern "C" fn() -> i64,
    stream_file: FnStr,
    stream_stop: FnV,
    stream_is_active: FnI,
    audio_format: unsafe extern "C" fn(*mut c_int, *mut c_int),
    read_audio: unsafe extern "C" fn(*mut u8, c_int) -> c_int,
    set_mute: unsafe extern "C" fn(c_int),
    get_mute: FnI,
    list_input_devices: FnBuf,
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let dylib = args
        .get(1)
        .cloned()
        .unwrap_or_else(|| "target/release/libteamconference_core.dylib".into());
    let host = args.get(2).cloned().unwrap_or_else(|| "127.0.0.1".into());
    let port: c_int = args.get(3).and_then(|p| p.parse().ok()).unwrap_or(9500);
    let audio = args
        .get(4)
        .cloned()
        .expect("Audiodatei fehlt (4. Argument, z. B. eine .ogg oder .mp3)");

    let lib = unsafe { libloading::Library::new(&dylib) }.expect("dylib laden");
    macro_rules! sym {
        ($n:literal, $t:ty) => {
            *unsafe { lib.get::<$t>($n) }.expect("Symbol fehlt")
        };
    }
    let api = Api {
        create: sym!(b"tc_create", FnI),
        destroy: sym!(b"tc_destroy", FnV),
        version: sym!(b"tc_version", FnCStr),
        last_error: sym!(b"tc_last_error", FnCStr),
        connect: sym!(b"tc_connect", unsafe extern "C" fn(*const c_char, c_int, c_int, c_int, *const c_char) -> c_int),
        disconnect: sym!(b"tc_disconnect", FnV),
        is_authenticated: sym!(b"tc_is_authenticated", FnI),
        send: sym!(b"tc_send", FnStr),
        poll_event: sym!(b"tc_poll_event", FnBuf),
        join_room: sym!(b"tc_join_room", unsafe extern "C" fn(i64, *const c_char) -> c_int),
        leave_room: sym!(b"tc_leave_room", FnV),
        current_room: sym!(b"tc_current_room", unsafe extern "C" fn() -> i64),
        stream_file: sym!(b"tc_stream_file", FnStr),
        stream_stop: sym!(b"tc_stream_stop", FnV),
        stream_is_active: sym!(b"tc_stream_is_active", FnI),
        audio_format: sym!(b"tc_audio_format", unsafe extern "C" fn(*mut c_int, *mut c_int)),
        read_audio: sym!(b"tc_read_audio", unsafe extern "C" fn(*mut u8, c_int) -> c_int),
        set_mute: sym!(b"tc_set_mute", unsafe extern "C" fn(c_int)),
        get_mute: sym!(b"tc_get_mute", FnI),
        list_input_devices: sym!(b"tc_list_input_devices", FnBuf),
    };
    let err = || unsafe { CStr::from_ptr((api.last_error)()) }.to_string_lossy().to_string();

    assert_eq!(unsafe { (api.create)() }, 1, "tc_create: {}", err());
    println!("Version: {}", unsafe { CStr::from_ptr((api.version)()) }.to_string_lossy());

    let mut buf = vec![0u8; 64 * 1024];
    let mut poll = |api: &Api, buf: &mut Vec<u8>| -> Option<serde_json::Value> {
        loop {
            let n = unsafe { (api.poll_event)(buf.as_mut_ptr() as *mut c_char, buf.len() as c_int) };
            if n == 0 {
                return None;
            }
            if n < 0 {
                buf.resize((-n) as usize + 16, 0);
                continue;
            }
            return serde_json::from_slice(&buf[..n as usize]).ok();
        }
    };
    // Auf ein Ereignis eines Typs warten; alle anderen werden ausgegeben.
    let wait_for = |api: &Api, buf: &mut Vec<u8>, poll: &mut dyn FnMut(&Api, &mut Vec<u8>) -> Option<serde_json::Value>, types: &[&str], secs: u64| -> serde_json::Value {
        let t0 = Instant::now();
        while t0.elapsed() < Duration::from_secs(secs) {
            if let Some(ev) = poll(api, buf) {
                let ty = ev["type"].as_str().unwrap_or("").to_string();
                println!("  Ereignis: {}", ty);
                if types.contains(&ty.as_str()) {
                    return ev;
                }
            } else {
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        panic!("Zeitablauf beim Warten auf {:?}", types);
    };

    let devs = {
        let n = unsafe { (api.list_input_devices)(buf.as_mut_ptr() as *mut c_char, buf.len() as c_int) };
        String::from_utf8_lossy(&buf[..n.max(0) as usize]).to_string()
    };
    println!("Eingabegeräte: {}", devs);

    let h = CString::new(host.clone()).unwrap();
    let login = CString::new(r#"{"username":"admin","password":"admin","nickname":"Rauchtest"}"#).unwrap();
    assert_eq!(unsafe { (api.connect)(h.as_ptr(), port, 0, 1, login.as_ptr()) }, 1, "tc_connect: {}", err());

    let auth = wait_for(&api, &mut buf, &mut poll, &["auth_response", "connect_failed"], 15);
    assert_eq!(auth["type"], "auth_response", "Verbindung: {}", auth);
    assert_eq!(auth["data"]["success"], true, "Anmeldung abgelehnt: {}", auth);
    assert_eq!(unsafe { (api.is_authenticated)() }, 1);
    let rooms = auth["data"]["rooms"].as_array().cloned().unwrap_or_default();
    let room_id = rooms.first().and_then(|r| r["id"].as_i64()).expect("Standardraum");
    println!("Angemeldet als user_id={}, Räume={}, erster Raum={}", auth["data"]["user_id"], rooms.len(), room_id);

    assert_eq!(unsafe { (api.join_room)(room_id, std::ptr::null()) }, 1, "tc_join_room: {}", err());
    let _ = wait_for(&api, &mut buf, &mut poll, &["room_list"], 10);
    let ack = wait_for(&api, &mut buf, &mut poll, &["audio_config_ack"], 10);
    assert_eq!(ack["data"]["success"], true, "audio_config_ack: {}", ack);
    assert_eq!(unsafe { (api.current_room)() }, room_id);
    println!("Raum {} betreten, UDP-Token erhalten", room_id);

    let chat = CString::new(format!(
        r#"{{"type":"chat_room","data":{{"room_id":{},"message":"Hallo vom Rauchtest"}}}}"#,
        room_id
    ))
    .unwrap();
    assert_eq!(unsafe { (api.send)(chat.as_ptr()) }, 1, "tc_send: {}", err());
    let echo = wait_for(&api, &mut buf, &mut poll, &["chat_room"], 10);
    assert_eq!(echo["data"]["message"], "Hallo vom Rauchtest");
    println!("Chat zurückerhalten von {}", echo["data"]["from_user"]["nickname"]);

    unsafe { (api.set_mute)(1) };
    assert_eq!(unsafe { (api.get_mute)() }, 1);
    let st = wait_for(&api, &mut buf, &mut poll, &["audio_user_state"], 10);
    assert_eq!(st["data"]["muted"], true);
    println!("Mikrofon stumm bestätigt");

    let (mut rate, mut ch) = (0, 0);
    unsafe { (api.audio_format)(&mut rate, &mut ch) };
    println!("Audioformat: {} Hz, {} Kanäle", rate, ch);

    let a = CString::new(audio.clone()).unwrap();
    assert_eq!(unsafe { (api.stream_file)(a.as_ptr()) }, 1, "tc_stream_file: {}", err());
    assert_eq!(unsafe { (api.stream_is_active)() }, 1);
    let mut pcm = vec![0u8; 32 * 1024];
    let (mut bytes, mut nonzero) = (0usize, 0usize);
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs(2) {
        let n = unsafe { (api.read_audio)(pcm.as_mut_ptr(), pcm.len() as c_int) };
        if n > 0 {
            bytes += n as usize;
            nonzero += pcm[..n as usize]
                .chunks_exact(2)
                .filter(|c| i16::from_le_bytes([c[0], c[1]]).abs() > 100)
                .count();
        } else {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    println!("PCM in 2 s: {} Bytes, davon {} hörbare Samples", bytes, nonzero);
    assert!(bytes > 0 && nonzero > 1000, "kein hörbares Audio aus tc_read_audio");
    unsafe { (api.stream_stop)() };
    let _ = wait_for(&api, &mut buf, &mut poll, &["stream_finished"], 5);
    println!("Stream beendet");

    unsafe { (api.leave_room)() };
    assert_eq!(unsafe { (api.current_room)() }, 0);
    unsafe { (api.disconnect)() };
    unsafe { (api.destroy)() };
    println!("Rauchtest OK");
}
