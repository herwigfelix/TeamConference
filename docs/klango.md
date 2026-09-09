# TeamConference als Konferenz-Unterbau von Klango

Klango (das Audioportal, `~/Documents/projekte/klango`) benutzt TeamConference
als Sprachkonferenz-Unterbau: **ein** TeamConference-Server läuft auf
klango.online, jeder Klango-Client hält eine ständige Steuerverbindung zu ihm
und tritt bei Bedarf Räumen bei. Dieses Dokument ist der **Vertrag** zwischen
drei Teilen, die getrennt entwickelt werden:

1. **Server im „Klango-Modus"** (`server/`, dieses Repo)
2. **Kern-Bibliothek mit C-API** (`lib/`, dieses Repo → `libteamconference_core.dylib` / `teamconference_core.dll`)
3. **Klango-Host** (`klango_rs/src/engine/conf.rs`, lädt die Bibliothek) und
   **Klango-Lua** (`klango_code/llib/llib_conf.lua`, die Bedienung)

Alles, was hier nicht steht, bleibt wie in `protocol.md`.

---

## 1. Server: Klango-Modus

Aktiv, sobald `[server] klango_secret` (bzw. `TC_KLANGO_SECRET`) nicht leer ist.
Dann gilt:

- Anmeldung mit **Klango-Token** (Abschnitt 1.1). Passwort-Anmeldung bleibt
  nur für lokale Konten (Admin-Werkzeug) möglich; Selbstregistrierung ist aus.
- **Jeder** angemeldete Nutzer darf Räume anlegen (Abschnitt 1.2).
- Räume kennen **Eigentümer, Raum-Admins, Gruppenzugehörigkeit** und darauf
  fussende Rechte (Abschnitt 1.3).
- **Anrufe** zwischen zwei Nutzern (Abschnitt 1.4).
- Leere temporäre Räume verschwinden von selbst.

### 1.1 Anmeldung mit Klango-Token

`auth_login { klango_token, nickname? }`

Token = `b64url(claims) . b64url(HMAC_SHA256(secret, b64url(claims)))` —
beide Teile Base64-URL **ohne** Padding, Signatur über den **Base64-String**
des Claims-Teils (nicht über das rohe JSON). Vergleich in konstanter Zeit.

```json
{ "sub":  "felix",            // Klango-ID, kleingeschrieben — die Identität
  "nick": "Felix",            // Anzeigename (Klango-ID wie eingegeben)
  "iat":  1756800000,
  "exp":  1756800600,         // Ablauf (Unix-Sekunden); danach ablehnen
  "adm":  ["12", "37"],       // Gruppen (Klango-gid als String), in denen der
                              // Nutzer Admin ODER Moderator ist
  "sadm": false }             // Klango-Serveradmin → Sitzungsrolle "admin"
```

- Konto: `find_user_by_central_uid("klango:" + sub)`, sonst anlegen mit
  `username = sub` (`unique_username`) und Rolle `user`. Die Sitzungsrolle ist
  `"admin"`, wenn `sadm`, sonst die DB-Rolle.
- `OnlineUser` bekommt ein neues Feld `klango_groups: Vec<String>` (= `adm`).
- Doppelte Anmeldung desselben Kontos: die alte Sitzung wird ersetzt (wie heute).
- `UserInfo` bekommt zusätzlich `username: String` (die Klango-ID). Damit
  können Klango-Clients Nutzer in Räumen ihren Kontakten zuordnen.

### 1.2 Räume

Neue Spalten in `rooms`: `group_id TEXT NOT NULL DEFAULT ''`,
`owner_id INTEGER NOT NULL DEFAULT 0`, `temporary INTEGER NOT NULL DEFAULT 0`,
`private INTEGER NOT NULL DEFAULT 0`. Beim Serverstart werden alle Räume mit
`temporary=1` gelöscht (Reste eines Absturzes).

Nur im Speicher (leben, bis der Raum gelöscht wird): `admins: HashSet<i64>`,
`bans: HashMap<i64, RoomBan { username, nickname, expires_at: Option<i64> }>`.

`RoomInfo` (in `room_list`, `auth_response`) bekommt zusätzlich:
`group_id: String` (`""` = keiner), `owner_id: i64` (`0` = keiner),
`admins: Vec<i64>`, `temporary: bool`, `private: bool`.

**Sichtbarkeit:** `private`-Räume (Anrufräume) stehen nur in der `room_list`
der Nutzer, die drin sind oder zu einem laufenden Anruf gehören (Anrufer und
Angerufener). Alle anderen sehen sie nicht.

**Neue/geänderte Nachrichten (C→S):**

| Typ | Daten | Wer | Wirkung |
|---|---|---|---|
| `room_create` | `{ name, password?, max_users?, sample_rate?, bit_depth?, channels?, bitrate? }` | jeder | Legt einen **temporären** Raum an: `owner_id = uid`, `temporary = 1`, `parent_id = null`. Antwort an den Ersteller: `room_created { room_id }`; danach `room_list` an alle im Tenant. Der Ersteller tritt **nicht** automatisch bei (Client schickt `room_join`). Grenzen: max. 3 temporäre Räume je Eigentümer, max. 500 Räume gesamt → `error`. Serveradmins können mit `persistent: true` weiterhin dauerhafte Räume anlegen. |
| `room_join_group` | `{ group_id, name }` | jeder | Raum mit dieser `group_id` im Tenant suchen; fehlt er, anlegen (`temporary = 1`, `owner_id = 0`, `group_id` gesetzt, `name` wie angegeben). Dann beitreten wie `room_join` (ohne Passwort). Es gibt je `group_id` höchstens einen Raum. |
| `room_join` | wie bisher | | Zusätzlich: Antwort `room_joined { room_id }` an den Beitretenden (VOR der `room_list`). Gesperrte Nutzer bekommen `error { message: "banned" }`. |
| `room_leave` | wie bisher | | Danach Aufräumen (unten). |
| `room_admin_set` | `{ room_id, user_id, admin: bool }` | Eigentümer oder Serveradmin; **nicht** in Gruppenräumen | Pflegt `admins`. An alle im Raum: `room_admin_changed { room_id, user_id, username, admin }`, dann `room_list` an alle im Tenant. |
| `room_kick` | `{ room_id, user_id, reason? }` | Raum-Moderator (1.3) | Ziel muss im Raum sein und darf nicht selbst Raum-Moderator sein. Ziel: `room_kicked { room_id, room_name, reason }` und `room_id = None`; Raum: `room_user_left`. |
| `room_ban` | `{ room_id, user_id, duration_minutes?, reason? }` | Raum-Moderator | Wie `room_kick` plus Eintrag in `bans` (`duration_minutes` fehlt/0 = bis der Raum verschwindet). Ziel: `room_banned { room_id, room_name, reason, expires_at? }`. |
| `room_unban` | `{ room_id, user_id }` | Raum-Moderator | Eintrag entfernen. Antwort: `room_bans_result` (s. u.). |
| `room_bans` | `{ room_id }` | Raum-Moderator | Antwort `room_bans_result { room_id, bans: [{ user_id, username, nickname, expires_at? }] }`. |
| `room_mute` | `{ room_id, user_id, muted: bool }` | Raum-Moderator | Setzt `admin_muted` des Ziels (wie `admin_mute`), Ziel muss im Raum sein. An alle im Raum: `audio_user_state`. `admin_muted` wird beim Verlassen des Raums zurückgesetzt. |
| `room_delete` | wie bisher | zusätzlich Eigentümer des Raums | Nutzer werden in **keinen** Raum verschoben (`room_id = None`), sie bekommen `room_closed { room_id, room_name }`. |
| `room_update` | wie bisher | zusätzlich Eigentümer | |
| `user_lookup` | `{ username }` | jeder | Antwort `user_lookup_result { username, online: bool, user_id?, nickname?, room_id? }`. |

Alle Fehler: `error { message }` an den Absender. Die bestehenden `admin_*`-
Nachrichten bleiben Serveradmin/-moderator vorbehalten.

**Kein Standardraum.** Im Klango-Modus wird die „Lobby" der Migration beim
Start gelöscht (`db/schema.rs`). Ein Raum, in dem jeder landet und den niemand
schließen kann, hätte keinen Zweck: die Klango-Raumliste zeigt stattdessen die
**Gruppen des Nutzers** (jede, auch ohne bestehenden Raum — er entsteht beim
Betreten) und die **offenen Räume**, die jemand frei erzeugt hat.

**Aufräumen:** Nach jedem Verlassen (auch Verbindungsabbruch, Kick, Anrufende)
gilt: ist der Raum `temporary` und leer → löschen (samt `admins`/`bans`) und
`room_list` an alle im Tenant. Gruppenräume sind ebenfalls temporär (werden
beim nächsten `room_join_group` neu angelegt). Einen Standardraum
(`is_default`) gibt es im Klango-Modus nicht (siehe oben).

### 1.3 Rechte

```
is_room_mod(uid, room) =
       user.role ∈ {admin, moderator}                 -- Serverrolle
    || room.owner_id == uid
    || room.admins ∋ uid
    || (room.group_id != "" && user.klango_groups ∋ room.group_id)

can_manage_admins(uid, room) =
       user.role == admin
    || (room.group_id == "" && room.owner_id == uid)
```

Ein Raum-Moderator darf: kicken, sperren, entsperren, stummschalten, den Raum
bearbeiten. Kicken/sperren geht nicht gegen andere Raum-Moderatoren.

### 1.4 Anrufe

Zustand im Server: `calls: HashMap<room_id, PendingCall { caller, callee, since }>`.

| Typ | Daten | Wirkung |
|---|---|---|
| `call_invite` (C→S) | `{ to_username }` | Ziel online im Tenant? sonst `call_failed { to_username, reason: "offline" }`. Ziel hat schon einen offenen Anruf oder ist in einem `private`-Raum? → `reason: "busy"`. Sonst: privaten Raum anlegen (`name = "<caller-nick> & <callee-nick>"`, `private = 1`, `temporary = 1`, `owner_id = caller`, Audio-Defaults), **Anrufer beitreten lassen** (verlässt seinen bisherigen Raum; bekommt `room_joined` + `room_list`), `PendingCall` eintragen. An den Angerufenen: `room_list` (damit er den Raum kennt) und `call_incoming { room_id, from_user_id, from_username, from_nickname }`. An den Anrufer: `call_ringing { room_id, to_user_id, to_username }`. |
| `call_answer` (C→S) | `{ room_id, accept: bool }` | Nur vom Angerufenen. `accept`: `PendingCall` entfernen, an den Anrufer `call_answered { room_id, user_id, username, accept: true }`. Der Angerufene tritt **selbst** mit `room_join` bei (der Client entscheidet, wann seine Audiokette bereit ist). Ablehnen: `call_answered { …, accept: false, reason: "declined" }` an den Anrufer; der Raum wird beim Verlassen des Anrufers aufgeräumt. |
| `call_cancel` (C→S) | `{ room_id }` | Nur vom Anrufer, solange der Anruf offen ist. Angerufener: `call_cancelled { room_id, reason: "cancelled" }`. Anrufer verlässt den Raum (Server setzt `room_id = None`), Aufräumen. |
| Zeitablauf | 60 s nach `call_invite` | Angerufener: `call_cancelled { room_id, reason: "timeout" }`; Anrufer: `call_answered { …, accept: false, reason: "timeout" }`; Anrufer aus dem Raum nehmen, Aufräumen. |
| Abbruch | Anrufer trennt/verlässt den Raum | Angerufener: `call_cancelled { reason: "cancelled" }`. Angerufener trennt | Anrufer: `call_answered { accept: false, reason: "offline" }`. |

Ein Nutzer kann höchstens **einen** offenen Anruf haben (als Anrufer oder
Angerufener).

### 1.5 Konfiguration und Betrieb

```toml
[server]
klango_secret = ""      # TC_KLANGO_SECRET — leer = Klango-Modus aus
internal_port = 9502    # TC_INTERNAL_PORT — interner Push-Endpunkt, 0 = aus
klango_url = "http://127.0.0.1:8000"  # TC_KLANGO_URL — leer = keine Anwesenheit
```

Auf klango.online: Steuerport 9500 (TLS, selbstsigniert — der Klango-Client
prüft das Zertifikat nicht), Audio-UDP 9501. systemd-Unit
`klango-conf.service` (liegt in `klango_server/deploy/`).

### 1.6 Push und Anwesenheit

Klango fragte bisher alle zwei Minuten nach, ob es etwas Neues gibt — je
angemeldetem Client. Der Konferenzserver hält aber ohnehin zu jedem Client
eine ständige, authentifizierte Verbindung. Über die sagt der Klango-Server
„für dich liegt etwas an", und der Client holt es sich sofort ab. Das erspart
einen zweiten Daemon, einen zweiten Port und einen WebSocket in Flask.

**Push-Zuhörer.** Jede erfolgreich angemeldete Klango-Verbindung wird als
Zuhörer eingetragen, auch eine zweite desselben Kontos, und beim Trennen
wieder ausgetragen. Das ist bewusst getrennt von der Konferenz-Sitzung: von
der gibt es weiterhin **eine je Konto** (in zwei Sprachräumen gleichzeitig zu
stehen ergäbe keinen Sinn), Benachrichtigungen dagegen sollen **alle** Rechner
erreichen, an denen jemand angemeldet ist.

**`session_replaced`** (S→C). Verdrängt eine zweite Anmeldung die
Konferenz-Sitzung, bekommt die alte Verbindung `{"type":"session_replaced",
"data":{}}`. Vorher verstummte sie still: sie hielt sich weiter für
angemeldet, konnte aber keinem Raum mehr beitreten. Als Push-Zuhörer bleibt
sie bestehen. Trennt sie sich später, räumt sie die Sitzung der neuen
Verbindung **nicht** mit weg.

**Klango → Konferenzserver: `POST http://127.0.0.1:<internal_port>/push`**

Bindet ausschließlich auf die Loopback-Adresse; `internal_port = 0` schaltet
den Endpunkt ab. Nur im Klango-Modus.

| | |
|---|---|
| Kopfzeile | `X-Klango-Secret: <klango_secret>` (Vergleich in konstanter Zeit) |
| Rumpf | `{"to": ["felix", …], "kind": "whatsnew"}` — höchstens 64 KB, höchstens 500 Namen; `kind` leer = `"whatsnew"` |
| Wirkung | `{"type":"klango_push","data":{"kind":"<kind>"}}` an alle Zuhörer dieser Konten |
| `200` | `{"ok":true,"delivered":<Zahl der erreichten Verbindungen>}` |
| `403` | falsches oder fehlendes Geheimnis — ohne Hinweis worauf |
| `400` | Rumpf unlesbar, zu groß oder zu viele Empfänger |
| `404` | jeder andere Pfad oder jede andere Methode |

Namen werden kleingeschrieben verglichen. Ein unbekannter Empfänger ist kein
Fehler, er zählt nur nicht mit.

**Konferenzserver → Klango: `POST <klango_url>/internal/presence`**

Dieselbe Kopfzeile, Rumpf `{"online": ["felix", …]}` — die **volle** Liste
aller Konten mit mindestens einer Verbindung, nicht ein Unterschied. Dadurch
erholt sich ein neu gestarteter Klango-Server von selbst, statt auf Dauer ein
falsches Bild zu behalten. Leere `klango_url` = keine Meldungen.

Ausgelöst nach jeder Anmeldung und jedem Trennen, entprellt auf höchstens eine
Anfrage je zwei Sekunden (eine Welle von Anmeldungen ergibt eine Anfrage), und
zusätzlich fest alle 120 Sekunden. Zeitlimit drei Sekunden; Fehlschläge werden
protokolliert und sonst ignoriert — Konferenzen dürfen nicht darunter leiden,
dass der Klango-Server gerade neu startet.

Nötig ist das, weil der Klango-Server „online" bisher daran erkannte, dass in
den letzten zehn Minuten eine Anfrage kam — eine Nebenwirkung genau des Polls,
der hier wegfällt.

---

## 2. Kern-Bibliothek: C-API (`lib/src/ffi.rs`)

Ein Singleton; alle Zeichenketten UTF-8, nullterminiert. Rückgabe `1` = ok,
`0` = nein/fehlgeschlagen, sofern nichts anderes steht. Alle Funktionen sind
von **einem** Aufrufer-Thread zu benutzen (Klangos Lua-Thread); die Bibliothek
arbeitet intern mit eigener Tokio-Runtime und eigenen Audio-Threads.

```c
int         tc_create(void);                       // Runtime starten
void        tc_destroy(void);                      // alles beenden, trennen
const char* tc_version(void);
const char* tc_last_error(void);                   // letzter Fehlertext ("" wenn keiner)

// Verbindung. login_json = das data-Objekt von auth_login, z. B.
// {"klango_token":"...","nickname":"Felix"} oder {"username":"admin","password":"admin"}
// Asynchron: Ergebnis kommt als Ereignis (auth_response / connect_failed / connection_lost).
int   tc_connect(const char* host, int port, int udp_port, int ssl, const char* login_json);
void  tc_disconnect(void);
int   tc_is_connected(void);
int   tc_is_authenticated(void);
long long tc_user_id(void);                        // eigene user_id, 0 wenn nicht angemeldet

// Steuerkanal. json = vollständige Nachricht {"type":..,"data":{..}}.
int   tc_send(const char* json);
// Nächstes Ereignis als JSON {"type":..,"data":..} nach buf kopieren.
// Rückgabe: Länge (ohne NUL); 0 = nichts anliegend; negativ = -benötigte Länge
// (Ereignis bleibt in der Schlange). Zusätzlich zu den Servernachrichten liefert
// die Bibliothek: connect_failed{message}, connection_lost{}, client_error{message},
// stream_finished{} (Dateistream zu Ende).
int   tc_poll_event(char* buf, int cap);

// Räume — übernehmen die Audio-Parameter des Raums und schicken audio_config
// (Logik aus client/src/actions.rs::join_room). password darf NULL sein.
int   tc_join_room(long long room_id, const char* password);
int   tc_join_group_room(const char* group_id, const char* name);
void  tc_leave_room(void);
long long tc_current_room(void);                   // 0 = keiner

// Mikrofon / Ton
void  tc_set_mute(int muted);        int tc_get_mute(void);      // schickt audio_mute
void  tc_set_deafen(int deafened);   int tc_get_deafen(void);    // schickt audio_deafen
void  tc_set_volume(float gain);                                 // Empfang gesamt (1.0 = normal)
void  tc_set_user_volume(long long user_id, float gain);
int   tc_set_input_device(const char* name_or_null);            // NULL = Standard; wirkt ab nächstem Raumbeitritt
int   tc_list_input_devices(char* buf, int cap);                 // JSON-Liste ["name",...]

// Dateistream (Quelle 1, parallel zum Mikrofon; lokal hörbar)
int   tc_stream_file(const char* path);
void  tc_stream_stop(void);
void  tc_stream_pause(int paused);   int tc_stream_is_paused(void);
void  tc_stream_seek(int delta_seconds);
void  tc_stream_set_volume(float gain);
int   tc_stream_is_active(void);

// Audio-Ausgabe: KEIN eigener Lautsprecher. Die Bibliothek mischt alle
// empfangenen Quellen (wie udp_client.rs) und legt das Ergebnis in einen Ring,
// den der Aufrufer leert. Format: i16 little-endian, interleaved, immer
// 48000 Hz; Kanäle liefert tc_audio_format (2). Nicht blockierend; 0 = nichts.
// Wird länger nicht gelesen, verwirft der Ring das Älteste (max. ~500 ms).
void  tc_audio_format(int* sample_rate, int* channels);
int   tc_read_audio(unsigned char* buf, int cap);              // Bytes
void  tc_clear_audio(void);

// Dateien im Raum (optional, wenn Zeit bleibt)
int   tc_upload_file(long long room_id, const char* path);
int   tc_download_file(long long file_id, const char* dest_path);
```

**Mikrofon:** die Bibliothek nimmt selbst per cpal auf (wie der Desktop-
Client, `start_capture_owned`) — sie startet die Aufnahme, sobald der Server
`audio_config_ack` schickt, und beendet sie beim Verlassen des Raums / Trennen.
Der Klango-Host holt VOR `tc_connect` die macOS-Mikrofonfreigabe
(`audio::request_microphone()`), sonst nimmt cpal still Stille auf.

**Wiederverbindung** macht die Bibliothek nicht; sie meldet `connection_lost`,
der Klango-Client verbindet neu (mit frischem Token).

---

## 3. Klango-Host: `k_Conf_*` (Lua-Globals, `klango_rs/src/engine/conf.rs`)

Die Bibliothek wird beim ersten `k_Conf_*`-Aufruf gesucht und geladen:
`$KLANGO_TCLIB` → `<exe>/teamconference/<lib>` (Windows-Release) →
`<exe>/../Resources/teamconference/<lib>` (macOS-Bundle) →
`<exe>/../../vendor/teamconference_<os>/<lib>` (Dev, `target/release/`).
Dateiname: `libteamconference_core.dylib` bzw. `teamconference_core.dll`.

| Lua | Bedeutung |
|---|---|
| `k_Conf_Available() -> bool, err` | Bibliothek gefunden und `tc_create` ok |
| `k_Conf_Connect{ host=, port=, udpport=, ssl=, login=<json> } -> bool, err` | holt vorher die Mikrofonfreigabe |
| `k_Conf_Disconnect()` | |
| `k_Conf_IsConnected() -> bool`, `k_Conf_IsAuthenticated() -> bool`, `k_Conf_UserId() -> int` | |
| `k_Conf_Send(json) -> bool` | |
| `k_Conf_Poll() -> json \| nil` | ein Ereignis je Aufruf |
| `k_Conf_JoinRoom(room_id, password) -> bool`, `k_Conf_JoinGroupRoom(gid, name) -> bool`, `k_Conf_LeaveRoom()`, `k_Conf_CurrentRoom() -> int` | |
| `k_Conf_SetMute(b)`, `k_Conf_GetMute()`, `k_Conf_SetDeafen(b)`, `k_Conf_GetDeafen()`, `k_Conf_SetVolume(f)`, `k_Conf_SetUserVolume(uid, f)` | |
| `k_Conf_StreamFile(path) -> bool, err`, `k_Conf_StreamStop()`, `k_Conf_StreamPause(b)`, `k_Conf_StreamIsPaused()`, `k_Conf_StreamSeek(sec)`, `k_Conf_StreamVolume(f)`, `k_Conf_StreamIsActive()` | |
| `k_Conf_OpenAudio() -> stream` | IcyFeed (48 kHz, Kanäle aus `tc_audio_format`, `set_owned(true)`), eigener RX-Thread wie in `sip.rs::start_rx`; Lua hängt ihn per `_Snd_Load("__conf", stream, {use3d=0})` + `k_SoundPlay` an den Mixer |
| `k_Conf_CloseAudio()` | RX-Thread beenden, Feed abbrechen |
| `k_Conf_InputDevices() -> {name,...}`, `k_Conf_SetInputDevice(name\|nil)` | |

Pfade aus Lua (`k_Conf_StreamFile`) sind Klango-virtuell und werden mit
`crate::host::resolve` aufgelöst wie in `k_Sip_Load`.

---

## 4. Klango-Lua (zur Orientierung; Vertrag ist Abschnitt 1–3)

- `llib/llib_conf.lua`: Verbindung nach der Anmeldung (Token über KRPC-Bus
  `/conf/KRPC/krpc.php`, Methode `_GetToken` → `{ token, host, port, udpport, ssl }`),
  Hintergrund-Tick aus `k_LoopWithRawInput` (`_k_BackgroundTick`), Klingeln
  mit dem Alarmton des Soundthemas, Gesprächsbildschirm als Formular
  (Nutzerliste, Verlauf, Eingabezeile, Auflegen-Knopf), Konferenzliste unter
  *Klango Netzwerk → Konferenzen*, „Anrufen" im Kontaktmenü.
