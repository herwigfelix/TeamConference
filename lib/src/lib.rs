//! TeamConference Core
//!
//! UI-agnostischer Kern, der direkt die bestehenden, geteilten Module des
//! Desktop-Clients wiederverwendet (`protocol`, `state`, `net`, `audio`).
//!
//! Spike-Ansatz: die Quelldateien werden per `#[path]` aus `client/src/`
//! eingebunden, NICHT kopiert. Der Desktop-Client bleibt dadurch unverändert
//! und die Wire-Kompatibilität ist per Konstruktion garantiert (eine Quelle).
//! Langfristig kann man diese Module physisch hierher verschieben und den
//! Client das Crate als Abhängigkeit einbinden lassen.

#[path = "../../client/src/protocol.rs"]
pub mod protocol;

#[path = "../../client/src/state.rs"]
pub mod state;

#[path = "../../client/src/net/mod.rs"]
pub mod net;

#[path = "../../client/src/audio/mod.rs"]
pub mod audio;

/// FFI-Smoke-Test: beweist, dass das Crate ein exportiertes C-Symbol erzeugt
/// und als `staticlib`/`cdylib` für mobile Anbindung taugt.
/// Gibt einen statischen, nullterminierten Versions-String zurück.
#[no_mangle]
pub extern "C" fn tc_core_version() -> *const std::os::raw::c_char {
    concat!("teamconference-core ", env!("CARGO_PKG_VERSION"), "\0").as_ptr()
        as *const std::os::raw::c_char
}
