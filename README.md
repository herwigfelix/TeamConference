# TeamConference

TeamConference ist eine selbst gehostete Sprachkonferenz-Anwendung im Stil von
TeamSpeak/Mumble — mit Fokus auf **Barrierefreiheit**: Der Client ist
vollständig per Tastatur bedienbar, alle Bedienelemente tragen
Screenreader-Beschriftungen (VoiceOver, NVDA, JAWS, Orca) und alle wichtigen
Aktionen sind über Kurztasten erreichbar.

- **Server**: Rust (Tokio) — Windows, macOS, Linux, Docker
- **Client**: Rust ([Slint](https://slint.dev)) — Windows, macOS, Linux
- **Audio**: Opus-kodiert über UDP (Fallback: rohes PCM), adaptiver Jitter-Puffer
- **Sicherheit**: TLS (selbstsignierte Zertifikate werden automatisch erzeugt),
  Argon2-Passwort-Hashes

## Features

- Hierarchische Räume und Unterräume, optional mit Passwort und Nutzerlimit
- Raum-Chat, Privatnachrichten, Server-Durchsagen
- Datei-Upload/-Download pro Raum
- Audiodateien in einen Raum streamen (MP3, WAV, FLAC, OGG, M4A, …)
- Mikrofon stumm / Ton aus (taub) / Loopback, Lautstärkeregler
- Admin-Funktionen: Kicken, Bannen (zeitlich oder dauerhaft), Verschieben,
  Stummschalten, Räume verwalten
- Deutsche Oberfläche, Kurztasten mit Strg (Windows/Linux) bzw. Cmd (macOS)

## Projektstruktur

```
teamconference/
├── client/             Slint-Client (aktiv)
│   ├── ui/main.slint   Oberfläche: Chat, Räume, Nutzer, Lautstärke, Dateien, Menüs
│   └── src/            Netzwerk (WebSocket/UDP), Audio (cpal/Opus/Symphonia), Logik
├── server/             Rust-Server
│   └── src/            control/ audio/ room/ user/ chat/ files/ admin/ db/
├── docker/             Dockerfile + Container-Konfiguration
├── docker-compose.yml  Server-Deployment
├── docs/protocol.md    Protokoll-Spezifikation
├── start.sh            Entwicklung: Server + Client bauen und starten (macOS/Linux)
└── start.bat           dito für Windows
```

## Schnellstart (Entwicklung)

Beide Skripte bauen Server und Client im Release-Modus, starten den Server
mit einem Standard-Admin (`admin` / `admin`) und öffnen den Client:

```sh
./start.sh        # macOS / Linux
start.bat         # Windows
```

Im Client dann verbinden mit **Host** `localhost`, **Port** `9500`, **SSL an**,
Benutzername/Passwort `admin` / `admin`.

### Build-Voraussetzungen

| Plattform | Benötigt |
|---|---|
| alle | Rust (stable), CMake (für den Opus-Codec) |
| Windows | MSVC-Toolchain mit Visual Studio Build Tools |
| Linux | `libasound2-dev` (ALSA) sowie die üblichen GUI-Pakete für winit |

## Server

```sh
cd server

# Erster Start: legt den Admin admin/admin an
cargo run --release -- --config config.default.toml --create-admin

# Danach
cargo run --release -- --config config.default.toml
```

Standard-Ports:

- **9500/TCP** — Steuerkanal (WebSocket, optional TLS)
- **9501/UDP** — Audiokanal

TLS-Zertifikate werden beim ersten Start automatisch erzeugt (pure Rust via
`rcgen`, kein externes openssl nötig). Eigene Zertifikate: Pfade in der
Config anpassen.

Ein Admin-Konto kann alternativ über Umgebungsvariablen angelegt werden
(wird beim Start erstellt, falls es noch nicht existiert):

```sh
TC_ADMIN_USERNAME=admin TC_ADMIN_PASSWORD=geheim cargo run --release -- --config config.default.toml
```

### Server-Konfiguration (`config.default.toml`)

| Sektion | Inhalt |
|---|---|
| `[server]` | Servername, Begrüßungsnachricht, max. Nutzer |
| `[network]` | Hosts/Ports für Steuer- und Audiokanal |
| `[tls]` | TLS an/aus, Zertifikatspfade, Auto-Generierung |
| `[audio]` | Standard- und Maximalwerte für Samplerate/Bittiefe/Kanäle |
| `[storage]` | SQLite-Pfad, Upload-Verzeichnis, max. Uploadgröße |
| `[logging]` | Loglevel (überschreibbar per `RUST_LOG`) |

## Docker (Server)

```sh
# Admin-Zugangsdaten in docker-compose.yml anpassen, dann im Projektstamm:
docker compose up -d --build
```

- Datenbank und Uploads liegen im benannten Volume `teamconference-data`,
  TLS-Zertifikate in `teamconference-certs` — beides überlebt Rebuilds.
- Die Server-Konfiguration wird read-only aus `docker/config.toml` gemountet.
- Der Standard-Admin wird über `TC_ADMIN_USERNAME` / `TC_ADMIN_PASSWORD`
  in der Compose-Datei angelegt — **Passwort vor dem ersten Start ändern**.

Logs ansehen: `docker compose logs -f` · Stoppen: `docker compose down`
(Daten bleiben erhalten; `down -v` löscht auch die Volumes).

## Client

```sh
cd client
cargo run --release
```

Das Hauptfenster enthält Chatverlauf, Chateingabe, die Raum-/Unterraumliste,
die Nutzerliste des aktuellen Raums, den Lautstärkeregler und die Dateiliste.
Alles Weitere (Verbindung, Audio, Raum- und Nutzerverwaltung, Datei-Streaming)
läuft über die Menüleiste oder Kurztasten — vollständige Liste mit F1 im
Client oder in [client/README.md](client/README.md).

Die wichtigsten Kurztasten (Strg unter Windows/Linux, Cmd unter macOS):

| Kurztaste | Aktion |
|---|---|
| Strg+M | Mikrofon stumm/laut |
| Strg+D | Ton aus/an (taub) |
| Strg+S | Audiodatei streamen |
| Strg+J | Ausgewähltem Raum beitreten |
| Strg+U / Strg+H | Datei hochladen / herunterladen |
| Strg+P | Privatnachricht an ausgewählten Nutzer |
| F1 | Kurztasten-Hilfe |

Einstellungen (Server, Benutzername, Audiogeräte, Lautstärke) werden
plattformüblich gespeichert:

- Linux: `~/.config/teamconference/client.json`
- macOS: `~/Library/Application Support/teamconference/client.json`
- Windows: `%APPDATA%\teamconference\client.json`

## Barrierefreiheit

- Slint nutzt [AccessKit](https://accesskit.dev) — der Client funktioniert mit
  VoiceOver (macOS), NVDA/JAWS (Windows) und Orca (Linux).
- Jedes Bedienelement hat ein deutsches `accessible-label`; Listen sind mit
  den Pfeiltasten navigierbar, Tab/Umschalt+Tab wechselt zwischen Elementen.
- Statusänderungen (stumm, Raum betreten, Upload fertig, …) werden zusätzlich
  als Textzeile im Chatverlauf protokolliert und sind damit nachlesbar.
- Unterräume werden in der Raumliste durch Einrückung dargestellt und
  passwortgeschützte Räume textuell gekennzeichnet (kein reines Icon).

## Protokoll

Vollständige Spezifikation in [docs/protocol.md](docs/protocol.md).

- **Steuerkanal**: JSON über WebSocket (`{"type": "...", "data": {...}}`) —
  Auth, Räume, Chat, Dateien, Admin
- **Audiokanal**: binäre UDP-Pakete mit 22-Byte-Header (Magic `TCON`,
  Session-Token, Sequenz, Zeitstempel, Format); Payload ist Opus
  (`bit_depth = 0`) oder rohes PCM

## Entwicklung

```sh
cd server && cargo check          # Server prüfen
cd client && cargo check          # Client prüfen (kompiliert auch die .slint-UI)
RUST_LOG=debug cargo run          # mit ausführlichem Logging starten
```

Die veralteten Vorgänger-Clients (`client-tauri/`, `legazy client(abandoned)/`)
sind nicht Teil des Repos und dienen höchstens als Referenz.
