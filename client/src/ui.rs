//! wxDragon-Oberfläche: Widget-Struktur, Aufbau, Menüs und Hilfsfunktionen.
//! wxWidgets liefert native Bedienelemente und damit native Barrierefreiheit
//! (UI Automation/MSAA unter Windows, NSAccessibility auf macOS, ATK auf Linux).
//! Auf iOS/Android rendert MUi dieselben Aufrufe mit SwiftUI bzw. Compose.
//!
//! Layout-Grundsätze: Formulare sind auf breiten Bildschirmen zweispaltig
//! (Beschriftung rechtsbündig | Feld), auf Telefonen steht die Beschriftung
//! über dem Feld und Knöpfe stehen untereinander in voller Breite. Zusammen-
//! gehöriges steht in benannten Gruppen (StaticBox) — Screenreader sagen den
//! Gruppennamen an, auf iOS ist er eine Überschrift.

#[cfg(not(any(target_os = "ios", target_os = "android")))]
use wxdragon::accessible::{AccStatus, AccessibleImpl};
use wxdragon::prelude::*;
use wxdragon::widgets::simplebook::SimpleBook;

/// Liefert einem ansonsten namenlosen Bedienelement (Eingabefeld, Checkbox)
/// einen stabilen Accessible-Namen. wxWidgets' Standard-`GetName` nutzt das
/// dynamische Label (bei TextCtrl den Inhalt), weshalb leere Felder sonst nur
/// generisch als „Eingabefeld"/„Kontrollkästchen" angesagt werden.
#[cfg(not(any(target_os = "ios", target_os = "android")))]
struct NamedAccessible {
    name: String,
}

#[cfg(not(any(target_os = "ios", target_os = "android")))]
impl AccessibleImpl for NamedAccessible {
    fn get_name(&self, child_id: i32) -> (AccStatus, Option<String>) {
        // child_id 0 = das Control selbst → unser Name. Für Kind-Elemente
        // (Listen-/Baumeinträge) NOT_IMPLEMENTED, damit wxWidgets den echten
        // Eintragstext liefert statt überall den Control-Namen.
        if child_id == 0 {
            (wxdragon::ffi::wxd_AccStatus_WXD_ACC_OK, Some(self.name.clone()))
        } else {
            (wxdragon::ffi::wxd_AccStatus_WXD_ACC_NOT_IMPLEMENTED, None)
        }
    }
}

/// Setzt Fenster- und Accessible-Namen eines Bedienelements.
#[cfg(not(any(target_os = "ios", target_os = "android")))]
pub fn set_a11y_name(widget: &dyn WxWidget, name: &str) {
    widget.set_name(name);
    widget.set_accessible(Accessible::new(widget, NamedAccessible { name: name.to_string() }));
    // wxAccessible wirkt nur unter Windows (MSAA/UIA). Auf macOS den Namen
    // direkt an der nativen NSView setzen, sonst sagt VoiceOver bei leeren
    // Feldern nur „Textfeld".
    #[cfg(target_os = "macos")]
    mac_set_accessibility_label(widget, name);
}

/// iOS/Android (MUi): Label direkt für VoiceOver/TalkBack setzen.
#[cfg(any(target_os = "ios", target_os = "android"))]
pub fn set_a11y_name(widget: &dyn WxWidget, name: &str) {
    widget.set_name(name);
    widget.set_accessibility_label(name);
}

/// `setAccessibilityLabel:` an der NSView des Widgets (und bei Scroll-Ansichten
/// — mehrzeilige Textfelder, Listen, Bäume — zusätzlich an deren Inhalt, der
/// den VoiceOver-Fokus bekommt).
#[cfg(target_os = "macos")]
fn mac_set_accessibility_label(widget: &dyn WxWidget, name: &str) {
    use objc::runtime::{Object, BOOL, NO};
    use objc::{class, msg_send, sel, sel_impl};
    let view = widget.get_handle() as *mut Object;
    if view.is_null() {
        return;
    }
    unsafe {
        let ns: *mut Object = msg_send![class!(NSString), alloc];
        let ns: *mut Object = msg_send![ns,
            initWithBytes: name.as_ptr() as *const std::ffi::c_void
            length: name.len()
            encoding: 4usize]; // NSUTF8StringEncoding
        if ns.is_null() {
            return;
        }
        let _: () = msg_send![view, setAccessibilityLabel: ns];
        let is_scroll: BOOL = msg_send![view, isKindOfClass: class!(NSScrollView)];
        if is_scroll != NO {
            let doc: *mut Object = msg_send![view, documentView];
            if !doc.is_null() {
                let _: () = msg_send![doc, setAccessibilityLabel: ns];
            }
        }
        let _: () = msg_send![ns, release];
    }
}

// ── Menü-/Befehls-IDs ──
pub const ID_DISCONNECT: i32 = ID_HIGHEST + 1;
pub const ID_TOGGLE_MUTE: i32 = ID_HIGHEST + 2;
pub const ID_TOGGLE_DEAFEN: i32 = ID_HIGHEST + 3;
pub const ID_TOGGLE_LOOPBACK: i32 = ID_HIGHEST + 4;
pub const ID_STREAM_FILE: i32 = ID_HIGHEST + 5;
pub const ID_STOP_STREAM: i32 = ID_HIGHEST + 6;
pub const ID_PAUSE_STREAM: i32 = ID_HIGHEST + 30;
pub const ID_AUDIO_SETTINGS: i32 = ID_HIGHEST + 31;
pub const ID_EDIT_ROOM: i32 = ID_HIGHEST + 32;
pub const ID_JOIN_ROOM: i32 = ID_HIGHEST + 7;
pub const ID_LEAVE_ROOM: i32 = ID_HIGHEST + 8;
pub const ID_CREATE_ROOM: i32 = ID_HIGHEST + 9;
pub const ID_DELETE_ROOM: i32 = ID_HIGHEST + 11;
pub const ID_UPLOAD: i32 = ID_HIGHEST + 12;
pub const ID_DOWNLOAD: i32 = ID_HIGHEST + 13;
pub const ID_PM: i32 = ID_HIGHEST + 15;
pub const ID_KICK: i32 = ID_HIGHEST + 16;
pub const ID_BAN: i32 = ID_HIGHEST + 17;
pub const ID_MOVE_USER: i32 = ID_HIGHEST + 18;
pub const ID_ADMIN_MUTE: i32 = ID_HIGHEST + 19;
pub const ID_ADMIN_UNMUTE: i32 = ID_HIGHEST + 20;
pub const ID_SERVER_MSG: i32 = ID_HIGHEST + 21;
pub const ID_HELP_KEYS: i32 = ID_HIGHEST + 22;
// Benutzerkonten-Dialog (nur Admins; wird dynamisch ein-/ausgeblendet)
pub const ID_ACCOUNTS: i32 = ID_HIGHEST + 23;
// Eigenes Passwort ändern (alle Nutzer)
pub const ID_CHANGE_PW: i32 = ID_HIGHEST + 29;
// Auto-Updater: nach Aktualisierung suchen
pub const ID_CHECK_UPDATE: i32 = ID_HIGHEST + 33;

/// Seiten des Server-Hub-Umschalters (`hub_book`).
pub const HUB_PAGE_LOGIN: usize = 0;
pub const HUB_PAGE_REGISTER: usize = 1;
pub const HUB_PAGE_RESET: usize = 2;
pub const HUB_PAGE_ACCOUNT: usize = 3;

/// Alle Widget-Handles der Oberfläche. Widgets sind Copy → frei in Closures kopierbar.
#[derive(Clone, Copy)]
pub struct Ui {
    pub frame: Frame,
    /// Reiter vor dem Verbinden: „Server-Hub" + „Serverliste".
    pub notebook: Notebook,

    // Verbindungsansicht (Reiter „Serverliste")
    #[allow(dead_code)]
    pub connect_panel: Panel,
    pub server_list: ListBox,
    pub host_in: TextCtrl,
    pub port_in: TextCtrl,
    /// Optionaler Audio-Port; leer = Steuerport + 1.
    pub audio_port_in: TextCtrl,
    pub ssl_chk: CheckBox,
    pub user_in: TextCtrl,
    pub pass_in: TextCtrl,
    pub nick_in: TextCtrl,
    /// „Zentrales Login verwenden" für diesen Server (statt Passwort).
    pub use_central_chk: CheckBox,
    pub connect_btn: Button,
    pub bookmark_btn: Button,
    pub remove_btn: Button,

    // Reiter „Server-Hub" (zentrales Login)
    #[allow(dead_code)]
    pub hub_panel: Panel,
    /// Tabloser Umschalter: Anmelden / Registrieren / Reset / Konto (HUB_PAGE_*).
    pub hub_book: SimpleBook,
    pub hub_status: StaticText,
    // Seite Anmelden
    pub hub_ident_in: TextCtrl,
    pub hub_login_pass_in: TextCtrl,
    pub hub_login_btn: Button,
    pub hub_show_register_btn: Button,
    pub hub_show_reset_btn: Button,
    // Seite Registrieren
    pub hub_phone_in: TextCtrl,
    pub hub_reg_user_in: TextCtrl,
    pub hub_reg_display_in: TextCtrl,
    pub hub_reg_pass_in: TextCtrl,
    pub hub_register_btn: Button,
    pub hub_code_in: TextCtrl,
    pub hub_verify_btn: Button,
    pub hub_back_register_btn: Button,
    // Seite Passwort-Reset
    pub hub_reset_phone_in: TextCtrl,
    pub hub_reset_code_in: TextCtrl,
    pub hub_reset_pass_in: TextCtrl,
    pub hub_reset_btn: Button,
    pub hub_reset_confirm_btn: Button,
    pub hub_back_reset_btn: Button,
    // Seite Konto: Verzeichnis + Profil/Einladungen + Abmelden
    pub hub_logout_btn: Button,
    /// Umschalter: öffentliches Verzeichnis ⇄ eigene/private Server
    pub hub_scope: Choice,
    pub hub_search_in: TextCtrl,
    pub hub_servers: ListBox,
    pub hub_refresh_btn: Button,
    pub hub_join_btn: Button,
    pub hub_create_btn: Button,
    pub hub_edit_btn: Button,
    pub hub_delete_btn: Button,
    pub hub_invite_btn: Button,
    pub hub_invites_btn: Button,
    pub hub_profile_btn: Button,
    /// Gruppe „Hub-Verwaltung" (nur für Hub-Admins sichtbar).
    pub hub_admin_box: StaticBox,
    pub hub_admin_pending_btn: Button,
    pub hub_admin_user_btn: Button,
    pub hub_admin_servers_btn: Button,
    pub hub_log: TextCtrl,

    // Hauptansicht — Räume/Nutzer als nativer Baum (plattformspezifisch:
    // wxTreeCtrl auf Windows, DataViewTreeCtrl auf macOS/Linux).
    pub main_panel: Panel,
    pub rooms_tree: crate::roomtree::Widget,
    // Beitreten-Knopf nur auf macOS/Linux; auf Windows per Enter/Doppelklick.
    #[cfg(not(target_os = "windows"))]
    pub join_btn: Button,
    pub chat_log: TextCtrl,
    pub chat_in: TextCtrl,
    pub send_btn: Button,
    /// Schnellschalter (spiegeln die Häkchen im Audio-Menü).
    pub mute_btn: ToggleButton,
    pub deafen_btn: ToggleButton,
    pub volume: Slider,
    pub mic_volume: Slider,
    pub files: ListBox,
    pub download_btn: Button,
}

impl Ui {
    /// Baut Menüleiste, beide Panels und die Statuszeile auf und gibt die Handles zurück.
    pub fn build(frame: Frame) -> Ui {
        build_menu_bar(&frame);

        StatusBar::builder(&frame)
            .with_fields_count(1)
            .add_initial_text(0, "Nicht verbunden")
            .build();

        // Reiter (Notebook) für die Vor-Verbindungs-Ansicht.
        let notebook = Notebook::builder(&frame).build();

        // ── Reiter „Serverliste" ──
        let connect_panel = Panel::builder(&notebook).build();
        let compact = compact();
        // Breit: Liste links, Verbindungsdaten rechts. Telefon: untereinander.
        let cv = BoxSizer::builder(if compact { Orientation::Vertical } else { Orientation::Horizontal }).build();

        let list_box = group(&connect_panel, "Gespeicherte Server");
        let server_list = ListBox::builder(&connect_panel).build();
        if compact {
            server_list.set_min_size(Size::new(-1, 160));
        }
        list_box.add(&server_list, 1, SizerFlag::Expand | SizerFlag::All, 8);
        let remove_btn = Button::builder(&connect_panel).with_label("Server entfernen").build();
        list_box.add(&remove_btn, 0, SizerFlag::Left | SizerFlag::Right | SizerFlag::Bottom, 8);
        cv.add_sizer(&list_box, if compact { 0 } else { 2 }, SizerFlag::Expand | SizerFlag::All, 10);

        let conn_box = group(&connect_panel, "Verbindung");
        let form = form_grid();
        let host_in = TextCtrl::builder(&connect_panel).build();
        let port_in = TextCtrl::builder(&connect_panel).build();
        port_in.set_value("9500");
        let audio_port_in = TextCtrl::builder(&connect_panel).build();
        let user_in = TextCtrl::builder(&connect_panel).build();
        // ProcessEnter: Enter im Passwortfeld verbindet (siehe main.rs).
        let pass_in = TextCtrl::builder(&connect_panel)
            .with_style(TextCtrlStyle::Password | TextCtrlStyle::ProcessEnter)
            .build();
        let nick_in = TextCtrl::builder(&connect_panel).build();
        form_row(&connect_panel, &form, "Host:", &host_in, "Host");
        form_row(&connect_panel, &form, "Port:", &port_in, "Port");
        form_row(&connect_panel, &form, "Audioport (optional):", &audio_port_in, "Audioport, optional");
        form_row(&connect_panel, &form, "Benutzername:", &user_in, "Benutzername");
        form_row(&connect_panel, &form, "Passwort:", &pass_in, "Passwort");
        form_row(&connect_panel, &form, "Spitzname:", &nick_in, "Spitzname");
        hint(&audio_port_in, "Steuerport + 1");
        hint(&nick_in, "leer = Benutzername");
        conn_box.add_sizer(&form, 0, SizerFlag::Expand | SizerFlag::All, 8);
        let ssl_chk = CheckBox::builder(&connect_panel)
            .with_label("SSL/TLS verwenden")
            .build();
        ssl_chk.set_value(true);
        set_a11y_name(&ssl_chk, "SSL/TLS verwenden");
        let use_central_chk = CheckBox::builder(&connect_panel)
            .with_label("Zentrales Login verwenden (statt Passwort)")
            .build();
        set_a11y_name(&use_central_chk, "Zentrales Login verwenden");
        conn_box.add(&ssl_chk, 0, SizerFlag::Left | SizerFlag::Right | SizerFlag::Bottom, 8);
        conn_box.add(&use_central_chk, 0, SizerFlag::Left | SizerFlag::Right | SizerFlag::Bottom, 8);
        let connect_btn = Button::builder(&connect_panel).with_label("Verbinden").build();
        let bookmark_btn = Button::builder(&connect_panel)
            .with_label("Als Lesezeichen speichern")
            .build();
        conn_box.add_sizer(&button_row(&[&connect_btn, &bookmark_btn]), 0, SizerFlag::Expand | SizerFlag::All, 4);
        cv.add_sizer(&conn_box, if compact { 0 } else { 3 }, SizerFlag::Expand | SizerFlag::All, 10);
        connect_panel.set_sizer(cv, true);

        // ── Reiter „Server-Hub" (zentrales Login) ──
        // Ein SimpleBook (tabloser Umschalter) zeigt je nach Zustand genau EINE
        // Seite: Anmelden / Registrieren / Passwort-Reset / Eingeloggt.
        let hub_panel = Panel::builder(&notebook).build();
        let hv = BoxSizer::builder(Orientation::Vertical).build();
        let hub_status = StaticText::builder(&hub_panel)
            .with_label("Nicht angemeldet. Mit dem zentralen Konto (Telefonnummer) siehst du das Server-Verzeichnis.")
            .build();
        set_a11y_name(&hub_status, "Hub-Status");
        hv.add(&hub_status, 0, SizerFlag::Expand | SizerFlag::All, 10);

        let hub_book = SimpleBook::builder(&hub_panel).build();

        // Seite 0 — Anmelden
        let hub_login_panel = Panel::builder(&hub_book).build();
        let lv = BoxSizer::builder(Orientation::Vertical).build();
        let login_box = group(&hub_login_panel, "Anmelden");
        let lform = form_grid();
        let hub_ident_in = TextCtrl::builder(&hub_login_panel).build();
        let hub_login_pass_in = TextCtrl::builder(&hub_login_panel)
            .with_style(TextCtrlStyle::Password | TextCtrlStyle::ProcessEnter)
            .build();
        form_row(&hub_login_panel, &lform, "Benutzername oder Telefon:", &hub_ident_in, "Hub-Benutzername oder Telefonnummer");
        form_row(&hub_login_panel, &lform, "Passwort:", &hub_login_pass_in, "Hub-Passwort");
        hint(&hub_ident_in, "Benutzername oder +491701234567");
        login_box.add_sizer(&lform, 0, SizerFlag::Expand | SizerFlag::All, 8);
        let hub_login_btn = Button::builder(&hub_login_panel).with_label("Anmelden").build();
        login_box.add_sizer(&button_row(&[&hub_login_btn]), 0, SizerFlag::Expand | SizerFlag::All, 4);
        lv.add_sizer(&login_box, 0, SizerFlag::Expand | SizerFlag::All, 10);
        let other_box = group(&hub_login_panel, "Noch kein Konto oder Passwort vergessen?");
        let hub_show_register_btn = Button::builder(&hub_login_panel).with_label("Neu registrieren…").build();
        let hub_show_reset_btn = Button::builder(&hub_login_panel).with_label("Passwort vergessen…").build();
        other_box.add_sizer(
            &button_row(&[&hub_show_register_btn, &hub_show_reset_btn]),
            0,
            SizerFlag::Expand | SizerFlag::All,
            4,
        );
        lv.add_sizer(&other_box, 0, SizerFlag::Expand | SizerFlag::Left | SizerFlag::Right | SizerFlag::Bottom, 10);
        hub_login_panel.set_sizer(lv, true);

        // Seite 1 — Registrieren. „Zurück" steht oben (wie eine Navigations-
        // zeile): als erstes Element sofort erreichbar und nie abgeschnitten.
        let hub_register_panel = Panel::builder(&hub_book).build();
        let rv = BoxSizer::builder(Orientation::Vertical).build();
        let hub_back_register_btn = Button::builder(&hub_register_panel).with_label("‹ Zurück zur Anmeldung").build();
        set_a11y_name(&hub_back_register_btn, "Zurück zur Anmeldung");
        rv.add(&hub_back_register_btn, 0, back_flags(), 10);
        let reg1 = group(&hub_register_panel, "Schritt 1: Konto anlegen");
        let rform = form_grid();
        let hub_phone_in = TextCtrl::builder(&hub_register_panel).build();
        let hub_reg_user_in = TextCtrl::builder(&hub_register_panel).build();
        let hub_reg_display_in = TextCtrl::builder(&hub_register_panel).build();
        let hub_reg_pass_in = TextCtrl::builder(&hub_register_panel).with_style(TextCtrlStyle::Password).build();
        form_row(&hub_register_panel, &rform, "Telefonnummer:", &hub_phone_in, "Telefonnummer mit Ländervorwahl");
        form_row(&hub_register_panel, &rform, "Benutzername:", &hub_reg_user_in, "Hub-Benutzername");
        form_row(&hub_register_panel, &rform, "Anzeigename:", &hub_reg_display_in, "Anzeigename");
        form_row(&hub_register_panel, &rform, "Passwort:", &hub_reg_pass_in, "Passwort");
        hint(&hub_phone_in, "+491701234567");
        hint(&hub_reg_display_in, "optional");
        reg1.add_sizer(&rform, 0, SizerFlag::Expand | SizerFlag::All, 8);
        let hub_register_btn = Button::builder(&hub_register_panel).with_label("Code per SMS/WhatsApp anfordern").build();
        reg1.add_sizer(&button_row(&[&hub_register_btn]), 0, SizerFlag::Expand | SizerFlag::All, 4);
        rv.add_sizer(&reg1, 0, SizerFlag::Expand | SizerFlag::Left | SizerFlag::Right | SizerFlag::Bottom, 10);
        let reg2 = group(&hub_register_panel, "Schritt 2: Code bestätigen");
        let cform = form_grid();
        let hub_code_in = TextCtrl::builder(&hub_register_panel).build();
        form_row(&hub_register_panel, &cform, "Bestätigungscode:", &hub_code_in, "Bestätigungscode");
        reg2.add_sizer(&cform, 0, SizerFlag::Expand | SizerFlag::All, 8);
        let hub_verify_btn = Button::builder(&hub_register_panel).with_label("Bestätigen und anmelden").build();
        reg2.add_sizer(&button_row(&[&hub_verify_btn]), 0, SizerFlag::Expand | SizerFlag::All, 4);
        rv.add_sizer(&reg2, 0, SizerFlag::Expand | SizerFlag::Left | SizerFlag::Right | SizerFlag::Bottom, 10);
        hub_register_panel.set_sizer(rv, true);

        // Seite 2 — Passwort zurücksetzen
        let hub_reset_panel = Panel::builder(&hub_book).build();
        let sv = BoxSizer::builder(Orientation::Vertical).build();
        let hub_back_reset_btn = Button::builder(&hub_reset_panel).with_label("‹ Zurück zur Anmeldung").build();
        set_a11y_name(&hub_back_reset_btn, "Zurück zur Anmeldung");
        sv.add(&hub_back_reset_btn, 0, back_flags(), 10);
        let res1 = group(&hub_reset_panel, "Schritt 1: Code anfordern");
        let sform = form_grid();
        let hub_reset_phone_in = TextCtrl::builder(&hub_reset_panel).build();
        form_row(&hub_reset_panel, &sform, "Telefonnummer:", &hub_reset_phone_in, "Telefonnummer für das Zurücksetzen");
        hint(&hub_reset_phone_in, "+491701234567");
        res1.add_sizer(&sform, 0, SizerFlag::Expand | SizerFlag::All, 8);
        let hub_reset_btn = Button::builder(&hub_reset_panel).with_label("Code anfordern").build();
        res1.add_sizer(&button_row(&[&hub_reset_btn]), 0, SizerFlag::Expand | SizerFlag::All, 4);
        sv.add_sizer(&res1, 0, SizerFlag::Expand | SizerFlag::Left | SizerFlag::Right | SizerFlag::Bottom, 10);
        let res2 = group(&hub_reset_panel, "Schritt 2: Neues Passwort setzen");
        let s2form = form_grid();
        let hub_reset_code_in = TextCtrl::builder(&hub_reset_panel).build();
        let hub_reset_pass_in = TextCtrl::builder(&hub_reset_panel).with_style(TextCtrlStyle::Password).build();
        form_row(&hub_reset_panel, &s2form, "Code:", &hub_reset_code_in, "Code aus der SMS");
        form_row(&hub_reset_panel, &s2form, "Neues Passwort:", &hub_reset_pass_in, "Neues Passwort");
        res2.add_sizer(&s2form, 0, SizerFlag::Expand | SizerFlag::All, 8);
        let hub_reset_confirm_btn = Button::builder(&hub_reset_panel).with_label("Neues Passwort setzen").build();
        res2.add_sizer(&button_row(&[&hub_reset_confirm_btn]), 0, SizerFlag::Expand | SizerFlag::All, 4);
        sv.add_sizer(&res2, 0, SizerFlag::Expand | SizerFlag::Left | SizerFlag::Right | SizerFlag::Bottom, 10);
        hub_reset_panel.set_sizer(sv, true);

        // Seite 3 — Eingeloggt: Verzeichnis, Konto, Verwaltung
        let hub_account_panel = Panel::builder(&hub_book).build();
        let av = BoxSizer::builder(Orientation::Vertical).build();
        let dir_box = group(&hub_account_panel, "Server");
        // Ansicht umschalten: das öffentliche Verzeichnis zeigt nur öffentliche
        // Server — eigene private und Server, bei denen man Mitglied ist,
        // liefert erst „Meine Server" (/servers/mine).
        let dform = form_grid();
        let hub_scope = Choice::builder(&hub_account_panel).build();
        hub_scope.append("Öffentliches Verzeichnis");
        hub_scope.append("Meine Server (auch private)");
        hub_scope.set_selection(0);
        // ProcessEnter: Enter im Suchfeld lädt die Liste neu (siehe main.rs).
        let hub_search_in = TextCtrl::builder(&hub_account_panel)
            .with_style(TextCtrlStyle::ProcessEnter)
            .build();
        form_row(&hub_account_panel, &dform, "Ansicht:", &hub_scope, "Ansicht: öffentliches Verzeichnis oder meine Server");
        form_row(&hub_account_panel, &dform, "Suche:", &hub_search_in, "Server suchen");
        hint(&hub_search_in, "Name oder Beschreibung, Enter sucht");
        dir_box.add_sizer(&dform, 0, SizerFlag::Expand | SizerFlag::All, 8);
        let hub_servers = ListBox::builder(&hub_account_panel).build();
        hub_servers.set_min_size(Size::new(-1, if compact { 180 } else { 140 }));
        set_a11y_name(&hub_servers, "Server");
        dir_box.add(&hub_servers, 1, SizerFlag::Expand | SizerFlag::Left | SizerFlag::Right, 8);
        let hub_join_btn = Button::builder(&hub_account_panel).with_label("Verbinden").build();
        let hub_refresh_btn = Button::builder(&hub_account_panel).with_label("Aktualisieren").build();
        let hub_create_btn = Button::builder(&hub_account_panel).with_label("Server anlegen…").build();
        let hub_edit_btn = Button::builder(&hub_account_panel).with_label("Bearbeiten…").build();
        let hub_delete_btn = Button::builder(&hub_account_panel).with_label("Löschen").build();
        dir_box.add_sizer(
            &button_row(&[&hub_join_btn, &hub_refresh_btn, &hub_create_btn, &hub_edit_btn, &hub_delete_btn]),
            0,
            SizerFlag::Expand | SizerFlag::All,
            4,
        );
        av.add_sizer(&dir_box, 1, SizerFlag::Expand | SizerFlag::All, 10);

        let acc_box = group(&hub_account_panel, "Mein Konto");
        let hub_invite_btn = Button::builder(&hub_account_panel).with_label("Nutzer einladen…").build();
        let hub_invites_btn = Button::builder(&hub_account_panel).with_label("Einladungen…").build();
        let hub_profile_btn = Button::builder(&hub_account_panel).with_label("Profil bearbeiten…").build();
        let hub_logout_btn = Button::builder(&hub_account_panel).with_label("Abmelden").build();
        acc_box.add_sizer(
            &button_row(&[&hub_invite_btn, &hub_invites_btn, &hub_profile_btn, &hub_logout_btn]),
            0,
            SizerFlag::Expand | SizerFlag::All,
            4,
        );
        av.add_sizer(&acc_box, 0, SizerFlag::Expand | SizerFlag::Left | SizerFlag::Right | SizerFlag::Bottom, 10);

        let admin_box = group(&hub_account_panel, "Hub-Verwaltung");
        let hub_admin_box = admin_box.get_static_box().expect("StaticBox der Gruppe");
        let hub_admin_pending_btn = Button::builder(&hub_account_panel).with_label("Freigaben…").build();
        let hub_admin_user_btn = Button::builder(&hub_account_panel).with_label("Nutzer…").build();
        let hub_admin_servers_btn = Button::builder(&hub_account_panel).with_label("Alle Server…").build();
        set_a11y_name(&hub_admin_pending_btn, "Admin: Freigaben");
        set_a11y_name(&hub_admin_user_btn, "Admin: Nutzer");
        set_a11y_name(&hub_admin_servers_btn, "Admin: alle Server");
        admin_box.add_sizer(
            &button_row(&[&hub_admin_pending_btn, &hub_admin_user_btn, &hub_admin_servers_btn]),
            0,
            SizerFlag::Expand | SizerFlag::All,
            4,
        );
        av.add_sizer(&admin_box, 0, SizerFlag::Expand | SizerFlag::Left | SizerFlag::Right | SizerFlag::Bottom, 10);
        hub_account_panel.set_sizer(av, true);

        hub_book.add_page(&hub_login_panel, "Anmelden", true, None);
        hub_book.add_page(&hub_register_panel, "Registrieren", false, None);
        hub_book.add_page(&hub_reset_panel, "Passwort", false, None);
        hub_book.add_page(&hub_account_panel, "Konto", false, None);
        hv.add(&hub_book, 1, SizerFlag::Expand, 0);

        // Meldungsverlauf: klein unten (die Meldungen werden zusätzlich
        // angesagt bzw. als Dialog/Hinweis gezeigt).
        let log_box = group(&hub_panel, "Meldungen");
        let hub_log = TextCtrl::builder(&hub_panel)
            .with_style(TextCtrlStyle::MultiLine | TextCtrlStyle::ReadOnly | TextCtrlStyle::WordWrap)
            .build();
        hub_log.set_min_size(Size::new(-1, if compact { 90 } else { 80 }));
        set_a11y_name(&hub_log, "Server-Hub Meldungen");
        log_box.add(&hub_log, 1, SizerFlag::Expand | SizerFlag::All, 6);
        hv.add_sizer(&log_box, 0, SizerFlag::Expand | SizerFlag::Left | SizerFlag::Right | SizerFlag::Bottom, 10);
        hub_panel.set_sizer(hv, true);

        // Server-Hub zuerst, dann Serverliste.
        notebook.add_page(&hub_panel, "Server-Hub", true, None);
        notebook.add_page(&connect_panel, "Serverliste", false, None);

        // ── Hauptansicht ──
        // Breit (Desktop, iPad): drei Spalten Räume | Chat | Audio & Dateien.
        // Telefon: dieselben drei Bereiche als Seiten mit Umschalter oben —
        // untereinander wären sie zu lang, und der Chat bekäme nur einen
        // schmalen Streifen.
        let main_panel = Panel::builder(&frame).build();
        let (rooms_page, chat_page, side_page, main_book) = if compact {
            let book = Notebook::builder(&main_panel).build();
            set_a11y_name(&book, "Bereich");
            let a = Panel::builder(&book).build();
            let b = Panel::builder(&book).build();
            let c = Panel::builder(&book).build();
            (a, b, c, Some(book))
        } else {
            (main_panel, main_panel, main_panel, None)
        };

        // Räume und Nutzer als nativer Baum.
        // Auf dem Telefon nennt der Umschalter den Bereich schon — dort ohne
        // doppelte Überschrift.
        let left = group(&rooms_page, if compact { "" } else { "Räume und Nutzer" });
        let rooms_tree = crate::roomtree::build(&rooms_page);
        left.add(&rooms_tree, 1, SizerFlag::Expand | SizerFlag::All, 6);
        // Auf Windows kein Beitreten-Knopf: Enter/Doppelklick im Baum tritt bei.
        #[cfg(not(target_os = "windows"))]
        let join_btn = {
            let b = Button::builder(&rooms_page).with_label("Raum beitreten").build();
            left.add(&b, 0, SizerFlag::Expand | SizerFlag::Left | SizerFlag::Right | SizerFlag::Bottom, 6);
            b
        };

        // Chat
        let center = group(&chat_page, if compact { "" } else { "Chat" });
        let chat_log = TextCtrl::builder(&chat_page)
            .with_style(TextCtrlStyle::MultiLine | TextCtrlStyle::ReadOnly | TextCtrlStyle::WordWrap)
            .build();
        center.add(&chat_log, 1, SizerFlag::Expand | SizerFlag::All, 6);
        let input_row = BoxSizer::builder(Orientation::Horizontal).build();
        let chat_in = TextCtrl::builder(&chat_page)
            .with_style(TextCtrlStyle::ProcessEnter)
            .build();
        hint(&chat_in, "Nachricht an den Raum");
        let send_btn = Button::builder(&chat_page).with_label("Senden").build();
        input_row.add(&chat_in, 1, SizerFlag::Expand | SizerFlag::Right, 6);
        input_row.add(&send_btn, 0, SizerFlag::AlignCenterVertical, 0);
        center.add_sizer(&input_row, 0, SizerFlag::Expand | SizerFlag::Left | SizerFlag::Right | SizerFlag::Bottom, 6);

        // Audio: Schnellschalter + Lautstärken
        let right = BoxSizer::builder(Orientation::Vertical).build();
        let audio_box = group(&side_page, "Audio");
        let mute_btn = ToggleButton::builder(&side_page).with_label("Mikrofon stumm").build();
        let deafen_btn = ToggleButton::builder(&side_page).with_label("Ton aus").build();
        set_a11y_name(&mute_btn, "Mikrofon stumm");
        set_a11y_name(&deafen_btn, "Ton aus");
        // Untereinander: die Spalte ist auf iPad und Desktop schmal.
        let toggles = BoxSizer::builder(Orientation::Vertical).build();
        toggles.add(&mute_btn, 0, SizerFlag::Expand | SizerFlag::Bottom, 6);
        toggles.add(&deafen_btn, 0, SizerFlag::Expand, 0);
        audio_box.add_sizer(&toggles, 0, SizerFlag::Expand | SizerFlag::All, 8);
        // 0–200 %: 100 = normale Lautstärke (Mitte), darüber lauter, darunter leiser.
        let volume = Slider::builder(&side_page)
            .with_value(100)
            .with_min_value(0)
            .with_max_value(200)
            .build();
        let mic_volume = Slider::builder(&side_page)
            .with_value(100)
            .with_min_value(0)
            .with_max_value(200)
            .build();
        for (label, slider, a11y) in [
            ("Wiedergabe (100 % = normal)", &volume, "Wiedergabe-Lautstärke in Prozent, 100 ist normal"),
            ("Mikrofon (100 % = normal)", &mic_volume, "Mikrofon-Lautstärke in Prozent, 100 ist normal"),
        ] {
            audio_box.add(
                &StaticText::builder(&side_page).with_label(label).build(),
                0,
                SizerFlag::Left | SizerFlag::Right,
                8,
            );
            audio_box.add(slider, 0, SizerFlag::Expand | SizerFlag::Left | SizerFlag::Right | SizerFlag::Bottom, 8);
            set_a11y_name(slider, a11y);
        }
        right.add_sizer(&audio_box, 0, SizerFlag::Expand | SizerFlag::Bottom, 10);

        // Dateien. Die Liste aktualisiert sich automatisch (beim Betreten eines
        // Raums und nach Hoch-/Herunterladen) — kein Knopf nötig.
        let files_box = group(&side_page, "Dateien im Raum");
        let files = ListBox::builder(&side_page).build();
        files.set_min_size(Size::new(-1, 120));
        files_box.add(&files, 1, SizerFlag::Expand | SizerFlag::All, 6);
        let download_btn = Button::builder(&side_page)
            .with_label("Herunterladen")
            .build();
        files_box.add(&download_btn, 0, SizerFlag::Expand | SizerFlag::Left | SizerFlag::Right | SizerFlag::Bottom, 6);
        right.add_sizer(&files_box, 1, SizerFlag::Expand, 0);

        if let Some(book) = main_book {
            for (page, sizer) in [(rooms_page, left), (chat_page, center)] {
                let s = BoxSizer::builder(Orientation::Vertical).build();
                s.add_sizer(&sizer, 1, SizerFlag::Expand | SizerFlag::All, 8);
                page.set_sizer(s, true);
            }
            let s = BoxSizer::builder(Orientation::Vertical).build();
            s.add_sizer(&right, 1, SizerFlag::Expand | SizerFlag::All, 8);
            side_page.set_sizer(s, true);
            book.add_page(&rooms_page, "Räume", true, None);
            book.add_page(&chat_page, "Chat", false, None);
            book.add_page(&side_page, "Audio & Dateien", false, None);
            let mv = BoxSizer::builder(Orientation::Vertical).build();
            mv.add(&book, 1, SizerFlag::Expand | SizerFlag::Left | SizerFlag::Right | SizerFlag::Top, 8);
            main_panel.set_sizer(mv, true);
        } else {
            let mh = BoxSizer::builder(Orientation::Horizontal).build();
            mh.add_sizer(&left, 2, SizerFlag::Expand | SizerFlag::Top | SizerFlag::Bottom | SizerFlag::Left, 10);
            mh.add_sizer(&center, 3, SizerFlag::Expand | SizerFlag::All, 10);
            mh.add_sizer(&right, 2, SizerFlag::Expand | SizerFlag::Top | SizerFlag::Bottom | SizerFlag::Right, 10);
            main_panel.set_sizer(mh, true);
        }

        // Frame-Sizer hält das Notebook (Vor-Verbindung) und die Hauptansicht;
        // anfangs nur das Notebook.
        let frame_sizer = BoxSizer::builder(Orientation::Vertical).build();
        frame_sizer.add(&notebook, 1, SizerFlag::Expand, 0);
        frame_sizer.add(&main_panel, 1, SizerFlag::Expand, 0);
        frame.set_sizer(frame_sizer, true);
        main_panel.show(false);
        frame.layout();

        // iOS/Android (MUi-Erweiterungen): Hauptaktionen hervorheben, Löschen
        // als zerstörerisch kennzeichnen, und die „Zurück"-Knöpfe der Hub-
        // Unterseiten als echte Navigation — auf iOS stehen sie dann links in
        // der Navigationsleiste, wo VoiceOver sie erwartet, und die
        // Zurück-Geste (Zwei-Finger-Z) löst sie aus.
        #[cfg(any(target_os = "ios", target_os = "android"))]
        {
            for b in [
                &connect_btn,
                &hub_login_btn,
                &hub_register_btn,
                &hub_verify_btn,
                &hub_reset_btn,
                &hub_reset_confirm_btn,
                &hub_join_btn,
            ] {
                b.set_prominent(true);
            }
            remove_btn.set_destructive(true);
            hub_delete_btn.set_destructive(true);
            for b in [&hub_back_register_btn, &hub_back_reset_btn] {
                b.set_label("Anmeldung");
                b.set_navigation_back(true);
            }
        }

        // Accessible-Namen für sonst namenlose Bedienelemente (Screenreader)
        set_a11y_name(&server_list, "Gespeicherte Server");
        set_a11y_name(&rooms_tree, "Räume und Nutzer");
        set_a11y_name(&chat_log, "Chatverlauf");
        set_a11y_name(&chat_in, "Chatnachricht eingeben");
        set_a11y_name(&files, "Dateien im aktuellen Raum");

        Ui {
            frame,
            notebook,
            connect_panel,
            server_list,
            host_in,
            port_in,
            audio_port_in,
            ssl_chk,
            user_in,
            pass_in,
            nick_in,
            use_central_chk,
            connect_btn,
            bookmark_btn,
            remove_btn,
            hub_panel,
            hub_book,
            hub_status,
            hub_ident_in,
            hub_login_pass_in,
            hub_login_btn,
            hub_show_register_btn,
            hub_show_reset_btn,
            hub_phone_in,
            hub_reg_user_in,
            hub_reg_display_in,
            hub_reg_pass_in,
            hub_register_btn,
            hub_code_in,
            hub_verify_btn,
            hub_back_register_btn,
            hub_reset_phone_in,
            hub_reset_code_in,
            hub_reset_pass_in,
            hub_reset_btn,
            hub_reset_confirm_btn,
            hub_back_reset_btn,
            hub_logout_btn,
            hub_scope,
            hub_search_in,
            hub_servers,
            hub_refresh_btn,
            hub_join_btn,
            hub_create_btn,
            hub_edit_btn,
            hub_delete_btn,
            hub_invite_btn,
            hub_invites_btn,
            hub_profile_btn,
            hub_admin_box,
            hub_admin_pending_btn,
            hub_admin_user_btn,
            hub_admin_servers_btn,
            hub_log,
            main_panel,
            rooms_tree,
            #[cfg(not(target_os = "windows"))]
            join_btn,
            chat_log,
            chat_in,
            send_btn,
            mute_btn,
            deafen_btn,
            volume,
            mic_volume,
            files,
            download_btn,
        }
    }

    /// Zwischen Verbindungs- (Notebook) und Hauptansicht umschalten.
    pub fn show_main(&self, main: bool) {
        self.main_panel.show(main);
        self.notebook.show(!main);
        self.frame.layout();
    }

    /// Eine Zeile an das Server-Hub-Meldungsfeld anhängen.
    pub fn append_hub_log(&self, line: &str) {
        let ts = chrono::Local::now().format("%H:%M").to_string();
        self.hub_log.append_text(&format!("[{}] {}\n", ts, line));
    }

    pub fn set_status(&self, text: &str) {
        self.frame.set_status_text(text, 0);
    }

    /// Eine Zeile (mit Zeitstempel) an den Chatverlauf anhängen; scrollt ans Ende.
    pub fn append_chat(&self, line: &str) {
        let ts = chrono::Local::now().format("%H:%M").to_string();
        self.chat_log.append_text(&format!("[{}] {}\n", ts, line));
    }
}

/// Tablet-Erkennung beim Start: kürzere Seite der App-Fläche ≥ 600 pt
/// (iPad in beiden Ausrichtungen; Telefone auch quer darunter).
#[cfg(any(target_os = "ios", target_os = "android"))]
fn is_tablet() -> bool {
    let w = SystemSettings::get_metric(SystemMetric::ScreenX, None);
    let h = SystemSettings::get_metric(SystemMetric::ScreenY, None);
    w > 0 && h > 0 && w.min(h) >= 600
}

/// Schmale Darstellung (Telefon): Beschriftungen über den Feldern, Knöpfe
/// untereinander, Hauptansicht in Seiten. Desktop und Tablets: breit.
pub fn compact() -> bool {
    #[cfg(any(target_os = "ios", target_os = "android"))]
    return !is_tablet();
    #[cfg(not(any(target_os = "ios", target_os = "android")))]
    false
}

/// Benannte Gruppe (StaticBox). Der Name wird von Screenreadern als Gruppe
/// angesagt (auf iOS als Überschrift).
pub fn group<W: WxWidget>(parent: &W, label: &str) -> StaticBoxSizer {
    StaticBoxSizerBuilder::new_with_label(Orientation::Vertical, parent, label).build()
}

/// Raster für Formulare: breit zwei Spalten (Beschriftung | Feld, das Feld
/// wächst), schmal eine Spalte (Beschriftung über dem Feld).
pub fn form_grid() -> FlexGridSizer {
    let cols = if compact() { 1 } else { 2 };
    let g = FlexGridSizer::builder(0, cols)
        .with_vgap(if compact() { 4 } else { 8 })
        .with_hgap(12)
        .build();
    g.add_growable_col((cols - 1) as usize, 1);
    g
}

/// Eine Formularzeile (Beschriftung + Feld) ins Raster setzen und dem Feld
/// seinen Accessible-Namen geben.
pub fn form_row<P: WxWidget, W: WxWidget>(parent: &P, grid: &FlexGridSizer, label: &str, field: &W, a11y: &str) {
    let lbl = StaticText::builder(parent).with_label(label).build();
    if compact() {
        grid.add(&lbl, 0, SizerFlag::Top, 6);
    } else {
        grid.add(&lbl, 0, SizerFlag::AlignCenterVertical | SizerFlag::AlignRight, 0);
    }
    grid.add(field, 1, SizerFlag::Expand, 0);
    set_a11y_name(field, a11y);
}

/// Knopfreihe: breit nebeneinander (linksbündig), schmal untereinander in
/// voller Breite (große Tippflächen, keine abgeschnittenen Beschriftungen).
pub fn button_row(buttons: &[&Button]) -> BoxSizer {
    if compact() {
        let s = BoxSizer::builder(Orientation::Vertical).build();
        for b in buttons {
            s.add(*b, 0, SizerFlag::Expand | SizerFlag::All, 4);
        }
        s
    } else {
        let s = BoxSizer::builder(Orientation::Horizontal).build();
        for b in buttons {
            s.add(*b, 0, SizerFlag::All, 4);
        }
        s
    }
}

/// Abbrechen/OK-Leiste für modale Dialoge (unten rechts). Die OK-Schaltfläche
/// ist Standard (Enter), Abbrechen beendet mit `ID_CANCEL` (auch Escape).
pub fn dialog_buttons(dialog: &Dialog, ok_label: &str) -> BoxSizer {
    let row = BoxSizer::builder(Orientation::Horizontal).build();
    let cancel = Button::builder(dialog).with_label("Abbrechen").build();
    let ok = Button::builder(dialog).with_label(ok_label).build();
    ok.set_default();
    {
        let d = *dialog;
        cancel.on_click(move |_| d.end_modal(ID_CANCEL));
    }
    {
        let d = *dialog;
        ok.on_click(move |_| d.end_modal(ID_OK));
    }
    row.add(&cancel, 0, SizerFlag::All, 6);
    row.add(&ok, 0, SizerFlag::All, 6);
    row
}

/// Hinweistext zu einem Eingabefeld: auf iOS/Android als Platzhalter im Feld,
/// am Desktop als Tooltip (macOS liest ihn VoiceOver als Hilfe vor).
pub fn hint(field: &TextCtrl, text: &str) {
    #[cfg(any(target_os = "ios", target_os = "android"))]
    field.set_hint(text);
    #[cfg(not(any(target_os = "ios", target_os = "android")))]
    field.set_tooltip(text);
}

/// Platzierung der „Zurück"-Knöpfe oben auf Unterseiten.
fn back_flags() -> SizerFlag {
    if compact() {
        SizerFlag::Expand | SizerFlag::All
    } else {
        SizerFlag::AlignLeft | SizerFlag::All
    }
}

/// Menüleiste mit Beschleunigertasten. wxWidgets bildet "Ctrl" auf macOS
/// automatisch auf Cmd ab — eine Definition genügt für alle Plattformen.
fn build_menu_bar(frame: &Frame) {
    let server_menu = Menu::builder()
        .append_item(ID_DISCONNECT, "Verbindung &trennen", "Vom Server trennen")
        .append_item(ID_EXIT, "&Beenden\tCtrl+Q", "Programm beenden")
        .build();

    // Check-Einträge: das Häkchen zeigt den Zustand (Screenreader: „aktiviert/deaktiviert").
    let audio_menu = Menu::builder()
        .append_check_item(ID_TOGGLE_MUTE, "Mikrofon &stummschalten\tCtrl+M", "Häkchen = Mikrofon ist stumm")
        .append_check_item(ID_TOGGLE_DEAFEN, "Ton &ausschalten (taub)\tCtrl+D", "Häkchen = Ton ist aus")
        .append_check_item(ID_TOGGLE_LOOPBACK, "&Loopback\tCtrl+L", "Häkchen = Loopback ist an")
        .append_item(ID_AUDIO_SETTINGS, "Audio-&Einstellungen…", "Samplerate, Bittiefe, Mono/Stereo")
        .append_item(ID_STREAM_FILE, "Audiodatei &streamen…\tCtrl+S", "Datei in den Raum streamen")
        .append_check_item(ID_PAUSE_STREAM, "Streaming &pausieren\tCtrl+P", "Häkchen = Streaming ist pausiert")
        .append_item(ID_STOP_STREAM, "Streaming &stoppen\tCtrl+Shift+S", "Streaming beenden")
        .build();

    let room_menu = Menu::builder()
        .append_item(ID_JOIN_ROOM, "Raum &beitreten\tCtrl+J", "Ausgewähltem Raum beitreten")
        .append_item(ID_LEAVE_ROOM, "Raum &verlassen", "Aktuellen Raum verlassen")
        .append_item(ID_CREATE_ROOM, "Raum &erstellen…", "Raum an der Cursor-Position erstellen (Lobby = oberste Ebene, auf einem Raum = Unterraum)")
        .append_item(ID_EDIT_ROOM, "Raum &bearbeiten…", "Ausgewählten Raum bearbeiten (Name, Passwort, Audio)")
        .append_item(ID_DELETE_ROOM, "Raum &löschen…", "Ausgewählten Raum löschen")
        .build();

    let file_menu = Menu::builder()
        .append_item(ID_UPLOAD, "Datei &hochladen…\tCtrl+U", "Datei in den aktuellen Raum hochladen")
        .append_item(ID_DOWNLOAD, "Datei &herunterladen…\tCtrl+H", "Ausgewählte Datei herunterladen")
        .build();

    // Nutzerbezogene Aktionen (am im Baum ausgewählten Nutzer).
    let user_menu = Menu::builder()
        .append_item(ID_PM, "&Privatnachricht senden\tCtrl+Shift+P", "An ausgewählten Nutzer (Text im Eingabefeld)")
        .append_item(ID_KICK, "Nutzer &kicken…", "Ausgewählten Nutzer kicken")
        .append_item(ID_BAN, "Nutzer &bannen…", "Ausgewählten Nutzer bannen")
        .append_item(ID_MOVE_USER, "Nutzer &verschieben", "In ausgewählten Raum verschieben")
        .append_item(ID_ADMIN_MUTE, "Nutzer stummschalten (Admin)", "Ausgewählten Nutzer stummschalten")
        .append_item(ID_ADMIN_UNMUTE, "Stummschaltung aufheben (Admin)", "Stummschaltung aufheben")
        .build();

    // Verwaltung: serverweite Aktionen und eigenes Konto. „Benutzerkonten
    // verwalten" wird nur für Admins dynamisch eingefügt (siehe
    // actions::update_account_menu).
    let admin_menu = Menu::builder()
        .append_item(ID_SERVER_MSG, "&Servernachricht senden…", "Nachricht an alle senden")
        .append_item(ID_CHANGE_PW, "Eigenes &Passwort ändern…", "Eigenes Passwort ändern")
        .build();

    let help_menu = Menu::builder()
        .append_item(ID_CHECK_UPDATE, "Nach &Aktualisierung suchen…", "Auf neue Version prüfen")
        .append_item(ID_HELP_KEYS, "&Kurztasten\tF1", "Kurztasten anzeigen")
        .append_item(ID_ABOUT, "&Über…", "Über TeamConference")
        .build();

    let menubar = MenuBar::builder()
        .append(server_menu, "&Server")
        .append(audio_menu, "&Audio")
        .append(room_menu, "&Räume")
        .append(file_menu, "&Dateien")
        .append(user_menu, "&Nutzer")
        .append(admin_menu, "&Verwaltung")
        .append(help_menu, "&Hilfe")
        .build();
    frame.set_menu_bar(menubar);
}
