//! wxDragon-Oberfläche: Widget-Struktur, Aufbau, Menüs und Hilfsfunktionen.
//! wxWidgets liefert native Bedienelemente und damit native Barrierefreiheit
//! (UI Automation/MSAA unter Windows, NSAccessibility auf macOS, ATK auf Linux).

use wxdragon::accessible::{AccStatus, AccessibleImpl};
use wxdragon::prelude::*;
use wxdragon::widgets::simplebook::SimpleBook;

/// Liefert einem ansonsten namenlosen Bedienelement (Eingabefeld, Checkbox)
/// einen stabilen Accessible-Namen. wxWidgets' Standard-`GetName` nutzt das
/// dynamische Label (bei TextCtrl den Inhalt), weshalb leere Felder sonst nur
/// generisch als „Eingabefeld"/„Kontrollkästchen" angesagt werden.
struct NamedAccessible {
    name: String,
}

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
pub fn set_a11y_name(widget: &dyn WxWidget, name: &str) {
    widget.set_name(name);
    widget.set_accessible(Accessible::new(widget, NamedAccessible { name: name.to_string() }));
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

/// Alle Widget-Handles der Oberfläche. Widgets sind Copy → frei in Closures kopierbar.
#[derive(Clone, Copy)]
pub struct Ui {
    pub frame: Frame,
    /// Reiter vor dem Verbinden: „Serverliste" + „Server-Hub".
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
    /// Tabloser Umschalter: Anmelden(0)/Registrieren(1)/Reset(2)/Konto(3).
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
    pub hub_search_in: TextCtrl,
    pub hub_servers: ListBox,
    pub hub_refresh_btn: Button,
    pub hub_join_btn: Button,
    pub hub_create_btn: Button,
    pub hub_edit_btn: Button,
    pub hub_delete_btn: Button,
    pub hub_invites_btn: Button,
    pub hub_profile_btn: Button,
    pub hub_admin_pending_btn: Button,
    pub hub_admin_user_btn: Button,
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
    pub volume: Slider,
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
        let cv = BoxSizer::builder(Orientation::Vertical).build();

        cv.add(
            &StaticText::builder(&connect_panel)
                .with_label("Gespeicherte Server")
                .build(),
            0,
            SizerFlag::All,
            6,
        );
        let server_list = ListBox::builder(&connect_panel).build();
        cv.add(&server_list, 1, SizerFlag::Expand | SizerFlag::All, 6);

        let server_btns = BoxSizer::builder(Orientation::Horizontal).build();
        let connect_btn = Button::builder(&connect_panel).with_label("Verbinden").build();
        let bookmark_btn = Button::builder(&connect_panel)
            .with_label("Als Lesezeichen speichern")
            .build();
        let remove_btn = Button::builder(&connect_panel)
            .with_label("Server entfernen")
            .build();
        server_btns.add(&connect_btn, 0, SizerFlag::All, 4);
        server_btns.add(&bookmark_btn, 0, SizerFlag::All, 4);
        server_btns.add(&remove_btn, 0, SizerFlag::All, 4);
        cv.add_sizer(&server_btns, 0, SizerFlag::All, 4);

        let host_in = TextCtrl::builder(&connect_panel).build();
        let port_in = TextCtrl::builder(&connect_panel).build();
        port_in.set_value("9500");
        let audio_port_in = TextCtrl::builder(&connect_panel).build();
        let ssl_chk = CheckBox::builder(&connect_panel)
            .with_label("SSL/TLS verwenden")
            .build();
        ssl_chk.set_value(true);
        let user_in = TextCtrl::builder(&connect_panel).build();
        let pass_in = TextCtrl::builder(&connect_panel)
            .with_style(TextCtrlStyle::Password)
            .build();
        let nick_in = TextCtrl::builder(&connect_panel).build();

        let use_central_chk = CheckBox::builder(&connect_panel)
            .with_label("Zentrales Login verwenden (statt Passwort)")
            .build();

        add_form_row(&connect_panel, &cv, "Host:", &host_in);
        add_form_row(&connect_panel, &cv, "Port:", &port_in);
        add_form_row(&connect_panel, &cv, "Audioport (optional):", &audio_port_in);
        cv.add(&ssl_chk, 0, SizerFlag::All, 6);
        add_form_row(&connect_panel, &cv, "Benutzername:", &user_in);
        add_form_row(&connect_panel, &cv, "Passwort:", &pass_in);
        add_form_row(&connect_panel, &cv, "Spitzname:", &nick_in);
        cv.add(&use_central_chk, 0, SizerFlag::All, 6);

        connect_panel.set_sizer(cv, true);

        // ── Reiter „Server-Hub" (zentrales Login) ──
        // Ein SimpleBook (tabloser Umschalter) zeigt je nach Zustand genau EINE
        // Seite: Anmelden / Registrieren / Passwort-Reset / Eingeloggt.
        let hub_panel = Panel::builder(&notebook).build();
        let hv = BoxSizer::builder(Orientation::Vertical).build();
        hv.add(
            &StaticText::builder(&hub_panel)
                .with_label("Server-Hub — zentrales Konto (Anmeldung per Telefonnummer)")
                .build(),
            0,
            SizerFlag::All,
            6,
        );
        let hub_status = StaticText::builder(&hub_panel)
            .with_label("Status: nicht angemeldet.")
            .build();
        hv.add(&hub_status, 0, SizerFlag::All, 6);

        let hub_book = SimpleBook::builder(&hub_panel).build();

        // Seite 0 — Anmelden
        let hub_login_panel = Panel::builder(&hub_book).build();
        let lv = BoxSizer::builder(Orientation::Vertical).build();
        lv.add(&StaticText::builder(&hub_login_panel).with_label("Anmelden").build(), 0, SizerFlag::All, 4);
        let hub_ident_in = TextCtrl::builder(&hub_login_panel).build();
        let hub_login_pass_in = TextCtrl::builder(&hub_login_panel).with_style(TextCtrlStyle::Password).build();
        add_form_row(&hub_login_panel, &lv, "Benutzername/Telefon:", &hub_ident_in);
        add_form_row(&hub_login_panel, &lv, "Passwort:", &hub_login_pass_in);
        let lrow = BoxSizer::builder(Orientation::Horizontal).build();
        let hub_login_btn = Button::builder(&hub_login_panel).with_label("Anmelden").build();
        let hub_show_register_btn = Button::builder(&hub_login_panel).with_label("Neu registrieren").build();
        let hub_show_reset_btn = Button::builder(&hub_login_panel).with_label("Passwort vergessen").build();
        lrow.add(&hub_login_btn, 0, SizerFlag::All, 4);
        lrow.add(&hub_show_register_btn, 0, SizerFlag::All, 4);
        lrow.add(&hub_show_reset_btn, 0, SizerFlag::All, 4);
        lv.add_sizer(&lrow, 0, SizerFlag::All, 2);
        hub_login_panel.set_sizer(lv, true);

        // Seite 1 — Registrieren
        let hub_register_panel = Panel::builder(&hub_book).build();
        let rv = BoxSizer::builder(Orientation::Vertical).build();
        rv.add(&StaticText::builder(&hub_register_panel).with_label("Neu registrieren (per Telefonnummer)").build(), 0, SizerFlag::All, 4);
        let hub_phone_in = TextCtrl::builder(&hub_register_panel).build();
        let hub_reg_user_in = TextCtrl::builder(&hub_register_panel).build();
        let hub_reg_display_in = TextCtrl::builder(&hub_register_panel).build();
        let hub_reg_pass_in = TextCtrl::builder(&hub_register_panel).with_style(TextCtrlStyle::Password).build();
        add_form_row(&hub_register_panel, &rv, "Telefon (z. B. +49…):", &hub_phone_in);
        add_form_row(&hub_register_panel, &rv, "Benutzername:", &hub_reg_user_in);
        add_form_row(&hub_register_panel, &rv, "Anzeigename:", &hub_reg_display_in);
        add_form_row(&hub_register_panel, &rv, "Passwort:", &hub_reg_pass_in);
        let hub_register_btn = Button::builder(&hub_register_panel).with_label("Code anfordern (SMS/WhatsApp)").build();
        rv.add(&hub_register_btn, 0, SizerFlag::All, 4);
        let hub_code_in = TextCtrl::builder(&hub_register_panel).build();
        add_form_row(&hub_register_panel, &rv, "Bestätigungscode:", &hub_code_in);
        let rrow = BoxSizer::builder(Orientation::Horizontal).build();
        let hub_verify_btn = Button::builder(&hub_register_panel).with_label("Code bestätigen & anmelden").build();
        let hub_back_register_btn = Button::builder(&hub_register_panel).with_label("Zurück").build();
        rrow.add(&hub_verify_btn, 0, SizerFlag::All, 4);
        rrow.add(&hub_back_register_btn, 0, SizerFlag::All, 4);
        rv.add_sizer(&rrow, 0, SizerFlag::All, 2);
        hub_register_panel.set_sizer(rv, true);

        // Seite 2 — Passwort zurücksetzen
        let hub_reset_panel = Panel::builder(&hub_book).build();
        let sv = BoxSizer::builder(Orientation::Vertical).build();
        sv.add(&StaticText::builder(&hub_reset_panel).with_label("Passwort zurücksetzen (per SMS-Code)").build(), 0, SizerFlag::All, 4);
        let hub_reset_phone_in = TextCtrl::builder(&hub_reset_panel).build();
        add_form_row(&hub_reset_panel, &sv, "Telefon:", &hub_reset_phone_in);
        let hub_reset_btn = Button::builder(&hub_reset_panel).with_label("Code anfordern").build();
        sv.add(&hub_reset_btn, 0, SizerFlag::All, 4);
        let hub_reset_code_in = TextCtrl::builder(&hub_reset_panel).build();
        let hub_reset_pass_in = TextCtrl::builder(&hub_reset_panel).with_style(TextCtrlStyle::Password).build();
        add_form_row(&hub_reset_panel, &sv, "Code:", &hub_reset_code_in);
        add_form_row(&hub_reset_panel, &sv, "Neues Passwort:", &hub_reset_pass_in);
        let srow = BoxSizer::builder(Orientation::Horizontal).build();
        let hub_reset_confirm_btn = Button::builder(&hub_reset_panel).with_label("Neues Passwort setzen").build();
        let hub_back_reset_btn = Button::builder(&hub_reset_panel).with_label("Zurück").build();
        srow.add(&hub_reset_confirm_btn, 0, SizerFlag::All, 4);
        srow.add(&hub_back_reset_btn, 0, SizerFlag::All, 4);
        sv.add_sizer(&srow, 0, SizerFlag::All, 2);
        hub_reset_panel.set_sizer(sv, true);

        // Seite 3 — Eingeloggt: Verzeichnis, Profil, Abmelden
        let hub_account_panel = Panel::builder(&hub_book).build();
        let av = BoxSizer::builder(Orientation::Vertical).build();
        let hub_logout_btn = Button::builder(&hub_account_panel).with_label("Abmelden").build();
        av.add(&hub_logout_btn, 0, SizerFlag::All, 4);
        av.add(&StaticText::builder(&hub_account_panel).with_label("Server-Verzeichnis").build(), 0, SizerFlag::All, 4);
        let hub_search_in = TextCtrl::builder(&hub_account_panel).build();
        add_form_row(&hub_account_panel, &av, "Suche:", &hub_search_in);
        let hub_servers = ListBox::builder(&hub_account_panel).build();
        av.add(&hub_servers, 1, SizerFlag::Expand | SizerFlag::All, 4);
        // Zeile 1: Server-Aktionen
        let drow = BoxSizer::builder(Orientation::Horizontal).build();
        let hub_refresh_btn = Button::builder(&hub_account_panel).with_label("Aktualisieren").build();
        let hub_join_btn = Button::builder(&hub_account_panel).with_label("Verbinden").build();
        let hub_create_btn = Button::builder(&hub_account_panel).with_label("Server anlegen…").build();
        let hub_edit_btn = Button::builder(&hub_account_panel).with_label("Bearbeiten…").build();
        let hub_delete_btn = Button::builder(&hub_account_panel).with_label("Löschen").build();
        drow.add(&hub_refresh_btn, 0, SizerFlag::All, 4);
        drow.add(&hub_join_btn, 0, SizerFlag::All, 4);
        drow.add(&hub_create_btn, 0, SizerFlag::All, 4);
        drow.add(&hub_edit_btn, 0, SizerFlag::All, 4);
        drow.add(&hub_delete_btn, 0, SizerFlag::All, 4);
        av.add_sizer(&drow, 0, SizerFlag::All, 2);
        // Zeile 2: Konto + Admin
        let drow2 = BoxSizer::builder(Orientation::Horizontal).build();
        let hub_invites_btn = Button::builder(&hub_account_panel).with_label("Einladungen…").build();
        let hub_profile_btn = Button::builder(&hub_account_panel).with_label("Profil bearbeiten…").build();
        let hub_admin_pending_btn = Button::builder(&hub_account_panel).with_label("Admin: Freigaben…").build();
        let hub_admin_user_btn = Button::builder(&hub_account_panel).with_label("Admin: Nutzer…").build();
        drow2.add(&hub_invites_btn, 0, SizerFlag::All, 4);
        drow2.add(&hub_profile_btn, 0, SizerFlag::All, 4);
        drow2.add(&hub_admin_pending_btn, 0, SizerFlag::All, 4);
        drow2.add(&hub_admin_user_btn, 0, SizerFlag::All, 4);
        av.add_sizer(&drow2, 0, SizerFlag::All, 2);
        hub_account_panel.set_sizer(av, true);

        hub_book.add_page(&hub_login_panel, "Anmelden", true, None);
        hub_book.add_page(&hub_register_panel, "Registrieren", false, None);
        hub_book.add_page(&hub_reset_panel, "Passwort", false, None);
        hub_book.add_page(&hub_account_panel, "Konto", false, None);
        hv.add(&hub_book, 1, SizerFlag::Expand | SizerFlag::All, 4);

        let hub_log = TextCtrl::builder(&hub_panel)
            .with_style(TextCtrlStyle::MultiLine | TextCtrlStyle::ReadOnly | TextCtrlStyle::WordWrap)
            .build();
        hv.add(&hub_log, 1, SizerFlag::Expand | SizerFlag::All, 6);
        hub_panel.set_sizer(hv, true);

        // Server-Hub zuerst, dann Serverliste.
        notebook.add_page(&hub_panel, "Server-Hub", true, None);
        notebook.add_page(&connect_panel, "Serverliste", false, None);

        // ── Hauptansicht ──
        let main_panel = Panel::builder(&frame).build();
        let mh = BoxSizer::builder(Orientation::Horizontal).build();

        // Linke Spalte: Räume und Nutzer als nativer Baum.
        let left = BoxSizer::builder(Orientation::Vertical).build();
        left.add(
            &StaticText::builder(&main_panel)
                .with_label("Räume und Nutzer")
                .build(),
            0,
            SizerFlag::All,
            4,
        );
        let rooms_tree = crate::roomtree::build(&main_panel);
        left.add(&rooms_tree, 1, SizerFlag::Expand | SizerFlag::All, 4);
        // Auf Windows kein Beitreten-Knopf: Enter/Doppelklick im Baum tritt bei.
        #[cfg(not(target_os = "windows"))]
        let join_btn = {
            let b = Button::builder(&main_panel).with_label("Beitreten").build();
            left.add(&b, 0, SizerFlag::Expand | SizerFlag::All, 4);
            b
        };
        mh.add_sizer(&left, 2, SizerFlag::Expand | SizerFlag::All, 4);

        // Mitte: Chat
        let center = BoxSizer::builder(Orientation::Vertical).build();
        center.add(
            &StaticText::builder(&main_panel).with_label("Chatverlauf").build(),
            0,
            SizerFlag::All,
            4,
        );
        let chat_log = TextCtrl::builder(&main_panel)
            .with_style(TextCtrlStyle::MultiLine | TextCtrlStyle::ReadOnly | TextCtrlStyle::WordWrap)
            .build();
        center.add(&chat_log, 1, SizerFlag::Expand | SizerFlag::All, 4);
        let input_row = BoxSizer::builder(Orientation::Horizontal).build();
        let chat_in = TextCtrl::builder(&main_panel)
            .with_style(TextCtrlStyle::ProcessEnter)
            .build();
        let send_btn = Button::builder(&main_panel).with_label("Senden").build();
        input_row.add(&chat_in, 1, SizerFlag::Expand | SizerFlag::All, 4);
        input_row.add(&send_btn, 0, SizerFlag::All, 4);
        center.add_sizer(&input_row, 0, SizerFlag::Expand | SizerFlag::All, 4);
        mh.add_sizer(&center, 3, SizerFlag::Expand | SizerFlag::All, 4);

        // Rechte Spalte: Lautstärke + Dateien
        let right = BoxSizer::builder(Orientation::Vertical).build();
        right.add(
            &StaticText::builder(&main_panel)
                .with_label("Lautstärke % (100 = normal)")
                .build(),
            0,
            SizerFlag::All,
            4,
        );
        // 0–200 %: 100 = normale Lautstärke (Mitte), darüber lauter, darunter leiser.
        let volume = Slider::builder(&main_panel)
            .with_value(100)
            .with_min_value(0)
            .with_max_value(200)
            .build();
        right.add(&volume, 0, SizerFlag::Expand | SizerFlag::All, 4);
        right.add(
            &StaticText::builder(&main_panel)
                .with_label("Dateien im aktuellen Raum")
                .build(),
            0,
            SizerFlag::All,
            4,
        );
        let files = ListBox::builder(&main_panel).build();
        right.add(&files, 1, SizerFlag::Expand | SizerFlag::All, 4);
        // Datei-Liste aktualisiert sich automatisch (beim Betreten eines Raums
        // und wenn Dateien hoch- oder runtergeladen werden) — kein Knopf nötig.
        let download_btn = Button::builder(&main_panel)
            .with_label("Herunterladen")
            .build();
        right.add(&download_btn, 0, SizerFlag::All, 4);
        mh.add_sizer(&right, 2, SizerFlag::Expand | SizerFlag::All, 4);

        main_panel.set_sizer(mh, true);

        // Frame-Sizer hält das Notebook (Vor-Verbindung) und die Hauptansicht;
        // anfangs nur das Notebook.
        let frame_sizer = BoxSizer::builder(Orientation::Vertical).build();
        frame_sizer.add(&notebook, 1, SizerFlag::Expand, 0);
        frame_sizer.add(&main_panel, 1, SizerFlag::Expand, 0);
        frame.set_sizer(frame_sizer, true);
        main_panel.show(false);
        frame.layout();

        // Accessible-Namen für sonst namenlose Bedienelemente (Screenreader)
        set_a11y_name(&server_list, "Gespeicherte Server");
        set_a11y_name(&host_in, "Host");
        set_a11y_name(&port_in, "Port");
        set_a11y_name(&audio_port_in, "Audioport optional");
        set_a11y_name(&ssl_chk, "SSL/TLS verwenden");
        set_a11y_name(&user_in, "Benutzername");
        set_a11y_name(&pass_in, "Passwort");
        set_a11y_name(&nick_in, "Spitzname");
        set_a11y_name(&use_central_chk, "Zentrales Login verwenden");
        set_a11y_name(&hub_status, "Hub-Status");
        set_a11y_name(&hub_ident_in, "Hub-Benutzername oder Telefon");
        set_a11y_name(&hub_login_pass_in, "Hub-Passwort");
        set_a11y_name(&hub_phone_in, "Telefonnummer");
        set_a11y_name(&hub_reg_user_in, "Hub-Benutzername");
        set_a11y_name(&hub_reg_display_in, "Anzeigename");
        set_a11y_name(&hub_reg_pass_in, "Passwort oder neues Passwort");
        set_a11y_name(&hub_code_in, "Bestätigungscode");
        set_a11y_name(&hub_reset_phone_in, "Telefonnummer für Reset");
        set_a11y_name(&hub_reset_code_in, "Reset-Code");
        set_a11y_name(&hub_reset_pass_in, "Neues Passwort");
        set_a11y_name(&hub_search_in, "Verzeichnis durchsuchen");
        set_a11y_name(&hub_servers, "Server im Verzeichnis");
        set_a11y_name(&hub_log, "Server-Hub Meldungen");
        set_a11y_name(&rooms_tree, "Räume und Nutzer");
        set_a11y_name(&chat_log, "Chatverlauf");
        set_a11y_name(&chat_in, "Chatnachricht eingeben");
        set_a11y_name(&volume, "Lautstärke in Prozent, 100 ist normal");
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
            hub_search_in,
            hub_servers,
            hub_refresh_btn,
            hub_join_btn,
            hub_create_btn,
            hub_edit_btn,
            hub_delete_btn,
            hub_invites_btn,
            hub_profile_btn,
            hub_admin_pending_btn,
            hub_admin_user_btn,
            hub_log,
            main_panel,
            rooms_tree,
            #[cfg(not(target_os = "windows"))]
            join_btn,
            chat_log,
            chat_in,
            send_btn,
            volume,
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

/// Hilfsfunktion: eine beschriftete Eingabezeile in den vertikalen Sizer einfügen.
fn add_form_row(panel: &Panel, vbox: &BoxSizer, label: &str, field: &TextCtrl) {
    let row = BoxSizer::builder(Orientation::Horizontal).build();
    let lbl = StaticText::builder(panel).with_label(label).build();
    row.add(&lbl, 0, SizerFlag::AlignCenterVertical | SizerFlag::All, 6);
    row.add(field, 1, SizerFlag::Expand | SizerFlag::All, 6);
    vbox.add_sizer(&row, 0, SizerFlag::Expand, 0);
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
