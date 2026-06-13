//! wxDragon-Oberfläche: Widget-Struktur, Aufbau, Menüs und Hilfsfunktionen.
//! wxWidgets liefert native Bedienelemente und damit native Barrierefreiheit
//! (UI Automation/MSAA unter Windows, NSAccessibility auf macOS, ATK auf Linux).

use wxdragon::accessible::{AccStatus, AccessibleImpl};
use wxdragon::prelude::*;

/// Liefert einem ansonsten namenlosen Bedienelement (Eingabefeld, Checkbox)
/// einen stabilen Accessible-Namen. wxWidgets' Standard-`GetName` nutzt das
/// dynamische Label (bei TextCtrl den Inhalt), weshalb leere Felder sonst nur
/// generisch als „Eingabefeld"/„Kontrollkästchen" angesagt werden.
struct NamedAccessible {
    name: String,
}

impl AccessibleImpl for NamedAccessible {
    fn get_name(&self, _child_id: i32) -> (AccStatus, Option<String>) {
        (wxdragon::ffi::wxd_AccStatus_WXD_ACC_OK, Some(self.name.clone()))
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
pub const ID_JOIN_ROOM: i32 = ID_HIGHEST + 7;
pub const ID_LEAVE_ROOM: i32 = ID_HIGHEST + 8;
pub const ID_CREATE_ROOM: i32 = ID_HIGHEST + 9;
pub const ID_CREATE_SUBROOM: i32 = ID_HIGHEST + 10;
pub const ID_DELETE_ROOM: i32 = ID_HIGHEST + 11;
pub const ID_UPLOAD: i32 = ID_HIGHEST + 12;
pub const ID_DOWNLOAD: i32 = ID_HIGHEST + 13;
pub const ID_REFRESH_FILES: i32 = ID_HIGHEST + 14;
pub const ID_PM: i32 = ID_HIGHEST + 15;
pub const ID_KICK: i32 = ID_HIGHEST + 16;
pub const ID_BAN: i32 = ID_HIGHEST + 17;
pub const ID_MOVE_USER: i32 = ID_HIGHEST + 18;
pub const ID_ADMIN_MUTE: i32 = ID_HIGHEST + 19;
pub const ID_ADMIN_UNMUTE: i32 = ID_HIGHEST + 20;
pub const ID_SERVER_MSG: i32 = ID_HIGHEST + 21;
pub const ID_HELP_KEYS: i32 = ID_HIGHEST + 22;
// Account-Verwaltung
pub const ID_ACCOUNTS: i32 = ID_HIGHEST + 23;
pub const ID_ACCOUNT_CREATE: i32 = ID_HIGHEST + 24;
pub const ID_ACCOUNT_PASSWORD: i32 = ID_HIGHEST + 25;
pub const ID_ACCOUNT_ROLE: i32 = ID_HIGHEST + 26;
pub const ID_ACCOUNT_DELETE: i32 = ID_HIGHEST + 27;
pub const ID_REGISTRATION: i32 = ID_HIGHEST + 28;
pub const ID_CHANGE_PW: i32 = ID_HIGHEST + 29;

/// Alle Widget-Handles der Oberfläche. Widgets sind Copy → frei in Closures kopierbar.
#[derive(Clone, Copy)]
pub struct Ui {
    pub frame: Frame,

    // Verbindungsansicht
    pub connect_panel: Panel,
    pub server_list: ListBox,
    pub host_in: TextCtrl,
    pub port_in: TextCtrl,
    pub ssl_chk: CheckBox,
    pub user_in: TextCtrl,
    pub pass_in: TextCtrl,
    pub nick_in: TextCtrl,
    pub connect_btn: Button,
    pub bookmark_btn: Button,
    pub remove_btn: Button,

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
    pub refresh_btn: Button,
}

impl Ui {
    /// Baut Menüleiste, beide Panels und die Statuszeile auf und gibt die Handles zurück.
    pub fn build(frame: Frame) -> Ui {
        build_menu_bar(&frame);

        StatusBar::builder(&frame)
            .with_fields_count(1)
            .add_initial_text(0, "Nicht verbunden")
            .build();

        // ── Verbindungsansicht ──
        let connect_panel = Panel::builder(&frame).build();
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
        let ssl_chk = CheckBox::builder(&connect_panel)
            .with_label("SSL/TLS verwenden")
            .build();
        ssl_chk.set_value(true);
        let user_in = TextCtrl::builder(&connect_panel).build();
        let pass_in = TextCtrl::builder(&connect_panel)
            .with_style(TextCtrlStyle::Password)
            .build();
        let nick_in = TextCtrl::builder(&connect_panel).build();

        add_form_row(&connect_panel, &cv, "Host:", &host_in);
        add_form_row(&connect_panel, &cv, "Port:", &port_in);
        cv.add(&ssl_chk, 0, SizerFlag::All, 6);
        add_form_row(&connect_panel, &cv, "Benutzername:", &user_in);
        add_form_row(&connect_panel, &cv, "Passwort:", &pass_in);
        add_form_row(&connect_panel, &cv, "Spitzname:", &nick_in);

        connect_panel.set_sizer(cv, true);

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
                .with_label("Lautstärke (%)")
                .build(),
            0,
            SizerFlag::All,
            4,
        );
        let volume = Slider::builder(&main_panel)
            .with_value(100)
            .with_min_value(0)
            .with_max_value(100)
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
        let file_btns = BoxSizer::builder(Orientation::Horizontal).build();
        let download_btn = Button::builder(&main_panel)
            .with_label("Herunterladen")
            .build();
        let refresh_btn = Button::builder(&main_panel)
            .with_label("Aktualisieren")
            .build();
        file_btns.add(&download_btn, 0, SizerFlag::All, 4);
        file_btns.add(&refresh_btn, 0, SizerFlag::All, 4);
        right.add_sizer(&file_btns, 0, SizerFlag::All, 4);
        mh.add_sizer(&right, 2, SizerFlag::Expand | SizerFlag::All, 4);

        main_panel.set_sizer(mh, true);

        // Frame-Sizer hält beide Panels; anfangs nur die Verbindungsansicht.
        let frame_sizer = BoxSizer::builder(Orientation::Vertical).build();
        frame_sizer.add(&connect_panel, 1, SizerFlag::Expand, 0);
        frame_sizer.add(&main_panel, 1, SizerFlag::Expand, 0);
        frame.set_sizer(frame_sizer, true);
        main_panel.show(false);
        frame.layout();

        // Accessible-Namen für sonst namenlose Bedienelemente (Screenreader)
        set_a11y_name(&server_list, "Gespeicherte Server");
        set_a11y_name(&host_in, "Host");
        set_a11y_name(&port_in, "Port");
        set_a11y_name(&ssl_chk, "SSL/TLS verwenden");
        set_a11y_name(&user_in, "Benutzername");
        set_a11y_name(&pass_in, "Passwort");
        set_a11y_name(&nick_in, "Spitzname");
        set_a11y_name(&rooms_tree, "Räume und Nutzer");
        set_a11y_name(&chat_log, "Chatverlauf");
        set_a11y_name(&chat_in, "Chatnachricht eingeben");
        set_a11y_name(&volume, "Lautstärke in Prozent");
        set_a11y_name(&files, "Dateien im aktuellen Raum");

        Ui {
            frame,
            connect_panel,
            server_list,
            host_in,
            port_in,
            ssl_chk,
            user_in,
            pass_in,
            nick_in,
            connect_btn,
            bookmark_btn,
            remove_btn,
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
            refresh_btn,
        }
    }

    /// Zwischen Verbindungs- und Hauptansicht umschalten.
    pub fn show_main(&self, main: bool) {
        self.main_panel.show(main);
        self.connect_panel.show(!main);
        self.frame.layout();
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

    let audio_menu = Menu::builder()
        .append_item(ID_TOGGLE_MUTE, "Mikrofon &stumm/laut\tCtrl+M", "Mikrofon umschalten")
        .append_item(ID_TOGGLE_DEAFEN, "Ton aus/an (&taub)\tCtrl+D", "Wiedergabe umschalten")
        .append_item(ID_TOGGLE_LOOPBACK, "&Loopback an/aus\tCtrl+L", "Loopback umschalten")
        .append_item(ID_AUDIO_SETTINGS, "Audio-&Einstellungen…", "Samplerate, Bittiefe, Mono/Stereo")
        .append_item(ID_STREAM_FILE, "Audiodatei &streamen…\tCtrl+S", "Datei in den Raum streamen")
        .append_item(ID_PAUSE_STREAM, "Streaming &pausieren/fortsetzen\tCtrl+P", "Gestreamte Datei pausieren bzw. fortsetzen")
        .append_item(ID_STOP_STREAM, "Streaming &stoppen\tCtrl+Shift+S", "Streaming beenden")
        .build();

    let room_menu = Menu::builder()
        .append_item(ID_JOIN_ROOM, "Raum &beitreten\tCtrl+J", "Ausgewähltem Raum beitreten")
        .append_item(ID_LEAVE_ROOM, "Raum &verlassen", "Aktuellen Raum verlassen")
        .append_item(ID_CREATE_ROOM, "Raum &erstellen…", "Neuen Raum erstellen")
        .append_item(ID_CREATE_SUBROOM, "&Unterraum erstellen…", "Unterraum im ausgewählten Raum erstellen")
        .append_item(ID_DELETE_ROOM, "Raum &löschen…", "Ausgewählten Raum löschen")
        .build();

    let file_menu = Menu::builder()
        .append_item(ID_UPLOAD, "Datei &hochladen…\tCtrl+U", "Datei in den aktuellen Raum hochladen")
        .append_item(ID_DOWNLOAD, "Datei &herunterladen…\tCtrl+H", "Ausgewählte Datei herunterladen")
        .append_item(ID_REFRESH_FILES, "Dateiliste &aktualisieren\tCtrl+R", "Dateiliste neu laden")
        .build();

    let admin_menu = Menu::builder()
        .append_item(ID_PM, "&Privatnachricht senden\tCtrl+Shift+P", "An ausgewählten Nutzer (Text im Eingabefeld)")
        .append_item(ID_KICK, "Nutzer &kicken…", "Ausgewählten Nutzer kicken")
        .append_item(ID_BAN, "Nutzer &bannen…", "Ausgewählten Nutzer bannen")
        .append_item(ID_MOVE_USER, "Nutzer &verschieben", "In ausgewählten Raum verschieben")
        .append_item(ID_ADMIN_MUTE, "Nutzer stummschalten (Admin)", "Ausgewählten Nutzer stummschalten")
        .append_item(ID_ADMIN_UNMUTE, "Stummschaltung aufheben (Admin)", "Stummschaltung aufheben")
        .append_item(ID_SERVER_MSG, "&Servernachricht senden…", "Nachricht an alle senden")
        .append_item(ID_ACCOUNTS, "&Konten anzeigen…", "Account-Liste vom Server abrufen")
        .append_item(ID_ACCOUNT_CREATE, "Konto an&legen…", "Neuen Account anlegen")
        .append_item(ID_ACCOUNT_PASSWORD, "Konto-Passwort zurücksetzen…", "Passwort eines Accounts setzen")
        .append_item(ID_ACCOUNT_ROLE, "Konto-Rolle ändern…", "Rolle user/admin setzen")
        .append_item(ID_ACCOUNT_DELETE, "Konto löschen…", "Account löschen")
        .append_item(ID_REGISTRATION, "Registrierung umschalten", "Selbstregistrierung an/aus")
        .append_item(ID_CHANGE_PW, "Eigenes &Passwort ändern…", "Eigenes Passwort ändern")
        .build();

    let help_menu = Menu::builder()
        .append_item(ID_HELP_KEYS, "&Kurztasten\tF1", "Kurztasten anzeigen")
        .append_item(ID_ABOUT, "&Über…", "Über TeamConference")
        .build();

    let menubar = MenuBar::builder()
        .append(server_menu, "&Server")
        .append(audio_menu, "&Audio")
        .append(room_menu, "&Räume")
        .append(file_menu, "&Dateien")
        .append(admin_menu, "&Verwaltung")
        .append(help_menu, "&Hilfe")
        .build();
    frame.set_menu_bar(menubar);
}
