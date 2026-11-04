//! wxDragon-Oberfläche: Widget-Struktur, Aufbau, Menüs und Hilfsfunktionen.
//! wxWidgets liefert native Bedienelemente und damit native Barrierefreiheit
//! (UI Automation/MSAA unter Windows, NSAccessibility auf macOS, ATK auf Linux).

use wxdragon::accessible::{AccStatus, AccessibleImpl};
use wxdragon::prelude::*;
use wxdragon::widgets::simplebook::SimpleBook;

use rust_i18n::t;

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

/// Mausrad über einer Auswahl (wxChoice) ignorieren. Windows wandelt jede
/// Raddrehung über einer Auswahlbox in „Pfeil runter/hoch" um — auch ohne
/// Fokus, nur weil der Mauszeiger darüber steht. Die Auswahl wechselt dann
/// unbemerkt (bei der Hub-Ansicht samt Neuladen der Liste), und der
/// Screenreader meldet einen scheinbar zufälligen Sprung. Bedient wird per
/// Tastatur oder Aufklappen; das Ereignis wird hier verschluckt (wxdragon
/// reicht Ereignisse standardmäßig weiter, daher ausdrücklich `skip(false)`).
pub fn ignore_mouse_wheel(choice: &Choice) {
    choice.on_mouse_wheel(|ev| ev.skip(false));
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
// Sprache der Oberfläche wählen
pub const ID_LANGUAGE: i32 = ID_HIGHEST + 34;

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
            .add_initial_text(0, &t!("status.not_connected"))
            .build();

        // Reiter (Notebook) für die Vor-Verbindungs-Ansicht.
        let notebook = Notebook::builder(&frame).build();

        // ── Reiter „Serverliste" ──
        let connect_panel = Panel::builder(&notebook).build();
        let cv = BoxSizer::builder(Orientation::Vertical).build();

        cv.add(
            &StaticText::builder(&connect_panel)
                .with_label(&t!("ui.saved_servers"))
                .build(),
            0,
            SizerFlag::All,
            6,
        );
        let server_list = ListBox::builder(&connect_panel).build();
        cv.add(&server_list, 1, SizerFlag::Expand | SizerFlag::All, 6);

        let server_btns = BoxSizer::builder(Orientation::Horizontal).build();
        let connect_btn = Button::builder(&connect_panel).with_label(&t!("ui.connect")).build();
        let bookmark_btn = Button::builder(&connect_panel)
            .with_label(&t!("ui.save_bookmark"))
            .build();
        let remove_btn = Button::builder(&connect_panel)
            .with_label(&t!("ui.remove_server"))
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
            .with_label(&t!("ui.use_ssl"))
            .build();
        ssl_chk.set_value(true);
        let user_in = TextCtrl::builder(&connect_panel).build();
        let pass_in = TextCtrl::builder(&connect_panel)
            .with_style(TextCtrlStyle::Password)
            .build();
        let nick_in = TextCtrl::builder(&connect_panel).build();

        let use_central_chk = CheckBox::builder(&connect_panel)
            .with_label(&t!("ui.use_central"))
            .build();

        add_form_row(&connect_panel, &cv, &t!("ui.host"), &host_in);
        add_form_row(&connect_panel, &cv, &t!("ui.port"), &port_in);
        add_form_row(&connect_panel, &cv, &t!("ui.audio_port"), &audio_port_in);
        cv.add(&ssl_chk, 0, SizerFlag::All, 6);
        add_form_row(&connect_panel, &cv, &t!("ui.username"), &user_in);
        add_form_row(&connect_panel, &cv, &t!("ui.password"), &pass_in);
        add_form_row(&connect_panel, &cv, &t!("ui.nickname"), &nick_in);
        cv.add(&use_central_chk, 0, SizerFlag::All, 6);

        connect_panel.set_sizer(cv, true);

        // ── Reiter „Server-Hub" (zentrales Login) ──
        // Ein SimpleBook (tabloser Umschalter) zeigt je nach Zustand genau EINE
        // Seite: Anmelden / Registrieren / Passwort-Reset / Eingeloggt.
        let hub_panel = Panel::builder(&notebook).build();
        let hv = BoxSizer::builder(Orientation::Vertical).build();
        hv.add(
            &StaticText::builder(&hub_panel)
                .with_label(&t!("ui.hub_heading"))
                .build(),
            0,
            SizerFlag::All,
            6,
        );
        let hub_status = StaticText::builder(&hub_panel)
            .with_label(&t!("hub_status.logged_out"))
            .build();
        hv.add(&hub_status, 0, SizerFlag::All, 6);

        let hub_book = SimpleBook::builder(&hub_panel).build();

        // Seite 0 — Anmelden
        let hub_login_panel = Panel::builder(&hub_book).build();
        let lv = BoxSizer::builder(Orientation::Vertical).build();
        lv.add(&StaticText::builder(&hub_login_panel).with_label(&t!("ui.hub_login")).build(), 0, SizerFlag::All, 4);
        let hub_ident_in = TextCtrl::builder(&hub_login_panel).build();
        let hub_login_pass_in = TextCtrl::builder(&hub_login_panel).with_style(TextCtrlStyle::Password).build();
        add_form_row(&hub_login_panel, &lv, &t!("ui.hub_ident"), &hub_ident_in);
        add_form_row(&hub_login_panel, &lv, &t!("ui.password"), &hub_login_pass_in);
        let lrow = BoxSizer::builder(Orientation::Horizontal).build();
        let hub_login_btn = Button::builder(&hub_login_panel).with_label(&t!("ui.hub_login")).build();
        let hub_show_register_btn = Button::builder(&hub_login_panel).with_label(&t!("ui.hub_show_register")).build();
        let hub_show_reset_btn = Button::builder(&hub_login_panel).with_label(&t!("ui.hub_forgot_pw")).build();
        lrow.add(&hub_login_btn, 0, SizerFlag::All, 4);
        lrow.add(&hub_show_register_btn, 0, SizerFlag::All, 4);
        lrow.add(&hub_show_reset_btn, 0, SizerFlag::All, 4);
        lv.add_sizer(&lrow, 0, SizerFlag::All, 2);
        hub_login_panel.set_sizer(lv, true);

        // Seite 1 — Registrieren
        let hub_register_panel = Panel::builder(&hub_book).build();
        let rv = BoxSizer::builder(Orientation::Vertical).build();
        rv.add(&StaticText::builder(&hub_register_panel).with_label(&t!("ui.hub_register_heading")).build(), 0, SizerFlag::All, 4);
        let hub_phone_in = TextCtrl::builder(&hub_register_panel).build();
        let hub_reg_user_in = TextCtrl::builder(&hub_register_panel).build();
        let hub_reg_display_in = TextCtrl::builder(&hub_register_panel).build();
        let hub_reg_pass_in = TextCtrl::builder(&hub_register_panel).with_style(TextCtrlStyle::Password).build();
        add_form_row(&hub_register_panel, &rv, &t!("ui.hub_phone_example"), &hub_phone_in);
        add_form_row(&hub_register_panel, &rv, &t!("ui.username"), &hub_reg_user_in);
        add_form_row(&hub_register_panel, &rv, &t!("profile.display_name"), &hub_reg_display_in);
        add_form_row(&hub_register_panel, &rv, &t!("ui.password"), &hub_reg_pass_in);
        let hub_register_btn = Button::builder(&hub_register_panel).with_label(&t!("ui.hub_request_code_sms")).build();
        rv.add(&hub_register_btn, 0, SizerFlag::All, 4);
        let hub_code_in = TextCtrl::builder(&hub_register_panel).build();
        add_form_row(&hub_register_panel, &rv, &t!("ui.hub_verification_code"), &hub_code_in);
        let rrow = BoxSizer::builder(Orientation::Horizontal).build();
        let hub_verify_btn = Button::builder(&hub_register_panel).with_label(&t!("ui.hub_verify")).build();
        let hub_back_register_btn = Button::builder(&hub_register_panel).with_label(&t!("ui.back")).build();
        rrow.add(&hub_verify_btn, 0, SizerFlag::All, 4);
        rrow.add(&hub_back_register_btn, 0, SizerFlag::All, 4);
        rv.add_sizer(&rrow, 0, SizerFlag::All, 2);
        hub_register_panel.set_sizer(rv, true);

        // Seite 2 — Passwort zurücksetzen
        let hub_reset_panel = Panel::builder(&hub_book).build();
        let sv = BoxSizer::builder(Orientation::Vertical).build();
        sv.add(&StaticText::builder(&hub_reset_panel).with_label(&t!("ui.hub_reset_heading")).build(), 0, SizerFlag::All, 4);
        let hub_reset_phone_in = TextCtrl::builder(&hub_reset_panel).build();
        add_form_row(&hub_reset_panel, &sv, &t!("ui.hub_phone"), &hub_reset_phone_in);
        let hub_reset_btn = Button::builder(&hub_reset_panel).with_label(&t!("ui.hub_request_code")).build();
        sv.add(&hub_reset_btn, 0, SizerFlag::All, 4);
        let hub_reset_code_in = TextCtrl::builder(&hub_reset_panel).build();
        let hub_reset_pass_in = TextCtrl::builder(&hub_reset_panel).with_style(TextCtrlStyle::Password).build();
        add_form_row(&hub_reset_panel, &sv, &t!("ui.hub_code"), &hub_reset_code_in);
        add_form_row(&hub_reset_panel, &sv, &t!("accounts.new_pw"), &hub_reset_pass_in);
        let srow = BoxSizer::builder(Orientation::Horizontal).build();
        let hub_reset_confirm_btn = Button::builder(&hub_reset_panel).with_label(&t!("ui.hub_set_new_pw")).build();
        let hub_back_reset_btn = Button::builder(&hub_reset_panel).with_label(&t!("ui.back")).build();
        srow.add(&hub_reset_confirm_btn, 0, SizerFlag::All, 4);
        srow.add(&hub_back_reset_btn, 0, SizerFlag::All, 4);
        sv.add_sizer(&srow, 0, SizerFlag::All, 2);
        hub_reset_panel.set_sizer(sv, true);

        // Seite 3 — Eingeloggt: Verzeichnis, Profil, Abmelden
        let hub_account_panel = Panel::builder(&hub_book).build();
        let av = BoxSizer::builder(Orientation::Vertical).build();
        let hub_logout_btn = Button::builder(&hub_account_panel).with_label(&t!("ui.hub_logout")).build();
        av.add(&hub_logout_btn, 0, SizerFlag::All, 4);
        av.add(&StaticText::builder(&hub_account_panel).with_label(&t!("ui.hub_directory")).build(), 0, SizerFlag::All, 4);
        // Ansicht umschalten: das öffentliche Verzeichnis zeigt nur öffentliche
        // Server — eigene private und Server, bei denen man Mitglied ist,
        // liefert erst „Meine Server" (/servers/mine).
        let scope_row = BoxSizer::builder(Orientation::Horizontal).build();
        let scope_lbl = StaticText::builder(&hub_account_panel).with_label(&t!("ui.hub_view")).build();
        let hub_scope = Choice::builder(&hub_account_panel).build();
        ignore_mouse_wheel(&hub_scope);
        hub_scope.append(&t!("ui.hub_scope_public"));
        hub_scope.append(&t!("ui.hub_scope_mine"));
        hub_scope.set_selection(0);
        scope_row.add(&scope_lbl, 0, SizerFlag::AlignCenterVertical | SizerFlag::All, 6);
        scope_row.add(&hub_scope, 1, SizerFlag::Expand | SizerFlag::All, 6);
        av.add_sizer(&scope_row, 0, SizerFlag::Expand, 0);
        // ProcessEnter: Enter im Suchfeld lädt die Liste neu (siehe main.rs).
        let hub_search_in = TextCtrl::builder(&hub_account_panel)
            .with_style(TextCtrlStyle::ProcessEnter)
            .build();
        add_form_row(&hub_account_panel, &av, &t!("ui.hub_search"), &hub_search_in);
        let hub_servers = ListBox::builder(&hub_account_panel).build();
        av.add(&hub_servers, 1, SizerFlag::Expand | SizerFlag::All, 4);
        // Zeile 1: Server-Aktionen
        let drow = BoxSizer::builder(Orientation::Horizontal).build();
        let hub_refresh_btn = Button::builder(&hub_account_panel).with_label(&t!("common.refresh")).build();
        let hub_join_btn = Button::builder(&hub_account_panel).with_label(&t!("ui.connect")).build();
        let hub_create_btn = Button::builder(&hub_account_panel).with_label(&t!("ui.hub_create_server")).build();
        let hub_edit_btn = Button::builder(&hub_account_panel).with_label(&t!("ui.hub_edit_server")).build();
        let hub_delete_btn = Button::builder(&hub_account_panel).with_label(&t!("ui.hub_delete_server")).build();
        drow.add(&hub_refresh_btn, 0, SizerFlag::All, 4);
        drow.add(&hub_join_btn, 0, SizerFlag::All, 4);
        drow.add(&hub_create_btn, 0, SizerFlag::All, 4);
        drow.add(&hub_edit_btn, 0, SizerFlag::All, 4);
        drow.add(&hub_delete_btn, 0, SizerFlag::All, 4);
        av.add_sizer(&drow, 0, SizerFlag::All, 2);
        // Zeile 2: Konto + Admin
        let drow2 = BoxSizer::builder(Orientation::Horizontal).build();
        let hub_invite_btn = Button::builder(&hub_account_panel).with_label(&t!("ui.hub_invite")).build();
        let hub_invites_btn = Button::builder(&hub_account_panel).with_label(&t!("ui.hub_invites")).build();
        let hub_profile_btn = Button::builder(&hub_account_panel).with_label(&t!("ui.hub_profile")).build();
        let hub_admin_pending_btn = Button::builder(&hub_account_panel).with_label(&t!("ui.hub_admin_pending")).build();
        let hub_admin_user_btn = Button::builder(&hub_account_panel).with_label(&t!("ui.hub_admin_users")).build();
        let hub_admin_servers_btn = Button::builder(&hub_account_panel).with_label(&t!("ui.hub_admin_servers")).build();
        drow2.add(&hub_invite_btn, 0, SizerFlag::All, 4);
        drow2.add(&hub_invites_btn, 0, SizerFlag::All, 4);
        drow2.add(&hub_profile_btn, 0, SizerFlag::All, 4);
        drow2.add(&hub_admin_pending_btn, 0, SizerFlag::All, 4);
        drow2.add(&hub_admin_user_btn, 0, SizerFlag::All, 4);
        drow2.add(&hub_admin_servers_btn, 0, SizerFlag::All, 4);
        av.add_sizer(&drow2, 0, SizerFlag::All, 2);
        hub_account_panel.set_sizer(av, true);

        hub_book.add_page(&hub_login_panel, &t!("ui.hub_login"), true, None);
        hub_book.add_page(&hub_register_panel, &t!("ui.hub_page_register"), false, None);
        hub_book.add_page(&hub_reset_panel, &t!("ui.hub_page_password"), false, None);
        hub_book.add_page(&hub_account_panel, &t!("ui.hub_page_account"), false, None);
        hv.add(&hub_book, 1, SizerFlag::Expand | SizerFlag::All, 4);

        let hub_log = TextCtrl::builder(&hub_panel)
            .with_style(TextCtrlStyle::MultiLine | TextCtrlStyle::ReadOnly | TextCtrlStyle::WordWrap)
            .build();
        hv.add(&hub_log, 1, SizerFlag::Expand | SizerFlag::All, 6);
        hub_panel.set_sizer(hv, true);

        // Server-Hub zuerst, dann Serverliste.
        notebook.add_page(&hub_panel, &t!("hub.caption"), true, None);
        notebook.add_page(&connect_panel, &t!("ui.server_list_tab"), false, None);

        // ── Hauptansicht ──
        let main_panel = Panel::builder(&frame).build();
        let mh = BoxSizer::builder(Orientation::Horizontal).build();

        // Linke Spalte: Räume und Nutzer als nativer Baum.
        let left = BoxSizer::builder(Orientation::Vertical).build();
        left.add(
            &StaticText::builder(&main_panel)
                .with_label(&t!("ui.rooms_and_users"))
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
            let b = Button::builder(&main_panel).with_label(&t!("room.join_caption")).build();
            left.add(&b, 0, SizerFlag::Expand | SizerFlag::All, 4);
            b
        };
        mh.add_sizer(&left, 2, SizerFlag::Expand | SizerFlag::All, 4);

        // Mitte: Chat
        let center = BoxSizer::builder(Orientation::Vertical).build();
        center.add(
            &StaticText::builder(&main_panel).with_label(&t!("ui.chat_log")).build(),
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
        let send_btn = Button::builder(&main_panel).with_label(&t!("ui.send")).build();
        input_row.add(&chat_in, 1, SizerFlag::Expand | SizerFlag::All, 4);
        input_row.add(&send_btn, 0, SizerFlag::All, 4);
        center.add_sizer(&input_row, 0, SizerFlag::Expand | SizerFlag::All, 4);
        mh.add_sizer(&center, 3, SizerFlag::Expand | SizerFlag::All, 4);

        // Rechte Spalte: Lautstärke + Dateien
        let right = BoxSizer::builder(Orientation::Vertical).build();
        right.add(
            &StaticText::builder(&main_panel)
                .with_label(&t!("ui.playback_volume"))
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
                .with_label(&t!("ui.mic_volume"))
                .build(),
            0,
            SizerFlag::All,
            4,
        );
        let mic_volume = Slider::builder(&main_panel)
            .with_value(100)
            .with_min_value(0)
            .with_max_value(200)
            .build();
        right.add(&mic_volume, 0, SizerFlag::Expand | SizerFlag::All, 4);
        right.add(
            &StaticText::builder(&main_panel)
                .with_label(&t!("ui.room_files"))
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
            .with_label(&t!("ui.download"))
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
        set_a11y_name(&server_list, &t!("ui.saved_servers"));
        set_a11y_name(&host_in, &t!("a11y.host"));
        set_a11y_name(&port_in, &t!("a11y.port"));
        set_a11y_name(&audio_port_in, &t!("a11y.audio_port"));
        set_a11y_name(&ssl_chk, &t!("ui.use_ssl"));
        set_a11y_name(&user_in, &t!("a11y.username"));
        set_a11y_name(&pass_in, &t!("a11y.password"));
        set_a11y_name(&nick_in, &t!("a11y.nickname"));
        set_a11y_name(&use_central_chk, &t!("a11y.use_central"));
        set_a11y_name(&hub_status, &t!("a11y.hub_status"));
        set_a11y_name(&hub_ident_in, &t!("a11y.hub_ident"));
        set_a11y_name(&hub_login_pass_in, &t!("a11y.hub_password"));
        set_a11y_name(&hub_phone_in, &t!("a11y.phone"));
        set_a11y_name(&hub_reg_user_in, &t!("a11y.hub_username"));
        set_a11y_name(&hub_reg_display_in, &t!("a11y.display_name"));
        set_a11y_name(&hub_reg_pass_in, &t!("a11y.password_or_new"));
        set_a11y_name(&hub_code_in, &t!("a11y.verification_code"));
        set_a11y_name(&hub_reset_phone_in, &t!("a11y.reset_phone"));
        set_a11y_name(&hub_reset_code_in, &t!("a11y.reset_code"));
        set_a11y_name(&hub_reset_pass_in, &t!("a11y.new_password"));
        set_a11y_name(&hub_scope, &t!("a11y.hub_scope"));
        set_a11y_name(&hub_search_in, &t!("a11y.hub_search"));
        set_a11y_name(&hub_servers, &t!("a11y.hub_servers"));
        set_a11y_name(&hub_log, &t!("a11y.hub_log"));
        set_a11y_name(&rooms_tree, &t!("ui.rooms_and_users"));
        set_a11y_name(&chat_log, &t!("ui.chat_log"));
        set_a11y_name(&chat_in, &t!("a11y.chat_input"));
        set_a11y_name(&volume, &t!("a11y.playback_volume"));
        set_a11y_name(&mic_volume, &t!("a11y.mic_volume"));
        set_a11y_name(&files, &t!("ui.room_files"));

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
        .append_item(ID_DISCONNECT, &t!("menu.disconnect"), &t!("menu.disconnect_help"))
        .append_item(ID_EXIT, &format!("{}\tCtrl+Q", t!("menu.exit")), &t!("menu.exit_help"))
        .build();

    // Check-Einträge: das Häkchen zeigt den Zustand (Screenreader: „aktiviert/deaktiviert").
    let audio_menu = Menu::builder()
        .append_check_item(ID_TOGGLE_MUTE, &format!("{}\tCtrl+M", t!("menu.mute")), &t!("menu.mute_help"))
        .append_check_item(ID_TOGGLE_DEAFEN, &format!("{}\tCtrl+D", t!("menu.deafen")), &t!("menu.deafen_help"))
        .append_check_item(ID_TOGGLE_LOOPBACK, &format!("{}\tCtrl+L", t!("menu.loopback")), &t!("menu.loopback_help"))
        .append_item(ID_AUDIO_SETTINGS, &t!("menu.audio_settings"), &t!("menu.audio_settings_help"))
        .append_item(ID_STREAM_FILE, &format!("{}\tCtrl+S", t!("menu.stream_file")), &t!("menu.stream_file_help"))
        .append_check_item(ID_PAUSE_STREAM, &format!("{}\tCtrl+P", t!("menu.pause_stream")), &t!("menu.pause_stream_help"))
        .append_item(ID_STOP_STREAM, &format!("{}\tCtrl+Shift+S", t!("menu.stop_stream")), &t!("menu.stop_stream_help"))
        .build();

    let room_menu = Menu::builder()
        .append_item(ID_JOIN_ROOM, &format!("{}\tCtrl+J", t!("menu.join_room")), &t!("menu.join_room_help"))
        .append_item(ID_LEAVE_ROOM, &t!("menu.leave_room"), &t!("menu.leave_room_help"))
        .append_item(ID_CREATE_ROOM, &t!("menu.create_room"), &t!("menu.create_room_help"))
        .append_item(ID_EDIT_ROOM, &t!("menu.edit_room"), &t!("menu.edit_room_help"))
        .append_item(ID_DELETE_ROOM, &t!("menu.delete_room"), &t!("menu.delete_room_help"))
        .build();

    let file_menu = Menu::builder()
        .append_item(ID_UPLOAD, &format!("{}\tCtrl+U", t!("menu.upload")), &t!("menu.upload_help"))
        .append_item(ID_DOWNLOAD, &format!("{}\tCtrl+H", t!("menu.download")), &t!("menu.download_help"))
        .build();

    // Nutzerbezogene Aktionen (am im Baum ausgewählten Nutzer).
    let user_menu = Menu::builder()
        .append_item(ID_PM, &format!("{}\tCtrl+Shift+P", t!("menu.pm")), &t!("menu.pm_help"))
        .append_item(ID_KICK, &t!("menu.kick"), &t!("menu.kick_help"))
        .append_item(ID_BAN, &t!("menu.ban"), &t!("menu.ban_help"))
        .append_item(ID_MOVE_USER, &t!("menu.move_user"), &t!("menu.move_user_help"))
        .append_item(ID_ADMIN_MUTE, &t!("menu.admin_mute"), &t!("menu.admin_mute_help"))
        .append_item(ID_ADMIN_UNMUTE, &t!("menu.admin_unmute"), &t!("menu.admin_unmute_help"))
        .build();

    // Verwaltung: serverweite Aktionen und eigenes Konto. „Benutzerkonten
    // verwalten" wird nur für Admins dynamisch eingefügt (siehe
    // actions::update_account_menu).
    let admin_menu = Menu::builder()
        .append_item(ID_SERVER_MSG, &t!("menu.server_msg"), &t!("menu.server_msg_help"))
        .append_item(ID_LANGUAGE, &t!("menu.language"), &t!("menu.language_help"))
        .append_item(ID_CHANGE_PW, &t!("menu.change_pw"), &t!("menu.change_pw_help"))
        .build();

    let help_menu = Menu::builder()
        .append_item(ID_CHECK_UPDATE, &t!("menu.check_update"), &t!("menu.check_update_help"))
        .append_item(ID_HELP_KEYS, &format!("{}\tF1", t!("menu.help_keys")), &t!("menu.help_keys_help"))
        .append_item(ID_ABOUT, &t!("menu.about"), &t!("help.about_title"))
        .build();

    let menubar = MenuBar::builder()
        .append(server_menu, &t!("menu.server"))
        .append(audio_menu, &t!("menu.audio"))
        .append(room_menu, &t!("menu.rooms"))
        .append(file_menu, &t!("menu.files"))
        .append(user_menu, &t!("menu.users"))
        .append(admin_menu, &t!("menu.admin"))
        .append(help_menu, &t!("menu.help"))
        .build();
    frame.set_menu_bar(menubar);
}
