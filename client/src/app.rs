//! Gemeinsamer UI-seitiger Kontext, der an alle Event-Closures übergeben wird.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use tokio::sync::mpsc;

use crate::config::ServerEntry;
use crate::protocol::{FileInfo, Message};
use crate::roomtree::NodeRef;
use crate::state::AppState;
use crate::ui::Ui;

/// Live-Verweise auf den (modalen) Benutzerkonten-Dialog, damit eingehende
/// Server-Antworten (account_list_result) die Liste aktualisieren können,
/// während der Dialog offen ist. Widgets sind Copy-Handles.
pub struct AccountDialogRef {
    pub list: wxdragon::widgets::ListBox,
    pub reg_chk: wxdragon::widgets::CheckBox,
    /// (Benutzername, Rolle) je Listeneintrag — für die Auswahl-Aktionen.
    pub accounts: Vec<(String, String)>,
}

/// Welche Seite im Server-Hub-Tab gezeigt wird, solange man NICHT eingeloggt
/// ist (eingeloggt → Konto-Seite, unabhängig davon).
#[derive(Default, Clone, Copy, PartialEq, Eq)]
pub enum HubView {
    #[default]
    Login,
    Register,
    Reset,
}

/// UI-Thread-eigene Daten (nicht thread-shared): Index→Daten-Zuordnungen.
#[derive(Default)]
pub struct UiState {
    /// Serverliste (entspricht den Einträgen der server_list-ListBox)
    pub servers: Vec<ServerEntry>,
    /// Hub-Verzeichnis (entspricht den Einträgen der hub_servers-ListBox)
    pub hub_servers: Vec<crate::hub::ServerInfo>,
    /// Im Hub-Modus zu verwendende Unterserver-ID beim nächsten Verbinden.
    pub pending_server_id: Option<String>,
    /// Aktuell sichtbare Hub-Seite (nur relevant, solange nicht eingeloggt).
    pub hub_view: HubView,
    /// Dateien der Dateiliste (entspricht den Einträgen der files-ListBox)
    pub files: Vec<FileInfo>,
    /// DataViewItem-Pointer → Baumknoten (nur macOS/Linux genutzt)
    pub tree_map: HashMap<usize, NodeRef>,
    /// Zuletzt vom Server gemeldeter Registrierungsstatus (für das Umschalten)
    pub registration_open: bool,
    /// Ob der angemeldete Nutzer Administrator ist (steuert Menü-Sichtbarkeit)
    pub is_admin: bool,
    /// Offener Benutzerkonten-Dialog (für Live-Aktualisierung der Liste)
    pub account_dialog: Option<AccountDialogRef>,
    /// Sprachausgabe (Screenreader/TTS). Nur auf dem UI-Thread benutzen — die
    /// Instanz ist plattformbedingt nicht Send (z. B. AVSpeechSynthesizer auf
    /// macOS). `None`, falls keine Sprachausgabe initialisiert werden konnte.
    pub tts: Option<tts::Tts>,
    /// Ob Server-Ereignisse (Joins/Verlassen/Abmelden/Raumnachrichten) per
    /// Sprachausgabe angesagt werden. Standardmäßig an (siehe Audio-Einstellungen).
    pub announce_events: bool,
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
