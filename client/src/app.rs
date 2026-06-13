//! Gemeinsamer UI-seitiger Kontext, der an alle Event-Closures übergeben wird.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use tokio::sync::mpsc;

use crate::config::ServerEntry;
use crate::protocol::{FileInfo, Message};
use crate::state::AppState;
use crate::ui::Ui;

/// UI-Thread-eigene Daten (nicht thread-shared): Index→Daten-Zuordnungen.
#[derive(Default)]
pub struct UiState {
    /// Serverliste (entspricht den Einträgen der server_list-ListBox)
    pub servers: Vec<ServerEntry>,
    /// Dateien der Dateiliste (entspricht den Einträgen der files-ListBox)
    pub files: Vec<FileInfo>,
    /// Raum-IDs in Anzeigereihenfolge (Index der rooms-ListBox → Raum-ID)
    pub room_ids: Vec<i64>,
    /// Nutzer-IDs in Anzeigereihenfolge (Index der users-ListBox → Nutzer-ID)
    pub user_ids: Vec<i64>,
    /// Zuletzt vom Server gemeldeter Registrierungsstatus (für das Umschalten)
    pub registration_open: bool,
}

/// Bündelt alles, was Event-Handler brauchen. Clone ist billig
/// (Widgets sind Copy, Rest sind Handles/Arc/Rc).
#[derive(Clone)]
pub struct Ctx {
    pub ui: Ui,
    pub app: Arc<AppState>,
    pub rt: tokio::runtime::Handle,
    pub ev_tx: mpsc::UnboundedSender<Message>,
    pub st: Rc<RefCell<UiState>>,
}
