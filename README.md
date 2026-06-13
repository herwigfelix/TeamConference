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

Alles Erforderliche in einem Befehl installieren:

**macOS** ([Homebrew](https://brew.sh)):

```sh
xcode-select --install        # einmalig: Compiler/Toolchain
brew install rust cmake
```

**Windows** ([Chocolatey](https://chocolatey.org), in einer Admin-PowerShell —
Reihenfolge einhalten):

```powershell
# 1. Visual C++ Build Tools (MSVC-Compiler/Linker + Windows SDK)
choco install visualstudio2022buildtools --package-parameters "--add Microsoft.VisualStudio.Workload.VCTools --includeRecommended --passive --norestart" -y

# 2. CMake (die inneren Anführungszeichen sind nötig, sonst landet CMake nicht im PATH)
choco install cmake --installargs '"ADD_CMAKE_TO_PATH=System"' -y

# 3. Rust
choco install rustup.install -y
```

Danach ein **neues** Terminal öffnen (PATH-Änderungen!) und die
MSVC-Toolchain aktivieren:

```powershell
rustup toolchain install stable-msvc
rustup default stable-msvc
```

Hinweise:
- Exit-Code **3010** beim Build-Tools-Schritt ist **kein Fehler**, sondern
  „erfolgreich, Neustart erforderlich" — einmal neu starten und weitermachen.
- Das Paket `visualstudio2022-workload-vctools` schlägt häufig fehl
  (hängender Installer, Exit-Code 1) — deshalb oben der direkte Weg über
  `visualstudio2022buildtools` mit `--package-parameters`.

Alternative ohne Chocolatey (winget ist auf Windows 10/11 vorinstalliert):

```powershell
winget install --id Microsoft.VisualStudio.2022.BuildTools --override "--add Microsoft.VisualStudio.Workload.VCTools --includeRecommended --passive --norestart"
winget install Kitware.CMake
winget install Rustlang.Rustup
```

**Linux** (Debian/Ubuntu):

```sh
sudo apt install build-essential cmake libasound2-dev
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

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

### Server-Konfiguration

Jeder Wert hat einen eingebauten Default und kann über eine `config.toml`
**und/oder** über Umgebungsvariablen gesetzt werden. Vorrang:
Umgebungsvariable → `config.toml` → Default. Eine Konfigurationsdatei ist
damit optional — fehlt sie, startet der Server mit Defaults und liest nur
die Umgebung (so arbeitet das Docker-Setup).

| `config.toml` | Umgebungsvariable | Default |
|---|---|---|
| `server.name` | `TC_SERVER_NAME` | TeamConference Server |
| `server.welcome_message` | `TC_WELCOME_MESSAGE` | Willkommen … |
| `server.max_users` | `TC_MAX_USERS` | 100 |
| `network.control_host` | `TC_CONTROL_HOST` | 0.0.0.0 |
| `network.control_port` | `TC_CONTROL_PORT` | 9500 |
| `network.audio_host` | `TC_AUDIO_HOST` | 0.0.0.0 |
| `network.audio_port` | `TC_AUDIO_PORT` | 9501 |
| `tls.enabled` | `TC_TLS_ENABLED` | true |
| `tls.cert_file` | `TC_TLS_CERT_FILE` | certs/server.crt |
| `tls.key_file` | `TC_TLS_KEY_FILE` | certs/server.key |
| `tls.auto_generate` | `TC_TLS_AUTO_GENERATE` | true |
| `audio.default_sample_rate` | `TC_AUDIO_DEFAULT_SAMPLE_RATE` | 48000 |
| `audio.default_bit_depth` | `TC_AUDIO_DEFAULT_BIT_DEPTH` | 16 |
| `audio.default_channels` | `TC_AUDIO_DEFAULT_CHANNELS` | 1 |
| `audio.max_sample_rate` | `TC_AUDIO_MAX_SAMPLE_RATE` | 192000 |
| `audio.max_bit_depth` | `TC_AUDIO_MAX_BIT_DEPTH` | 32 |
| `storage.database_path` | `TC_DATABASE_PATH` | data/teamconference.db |
| `storage.upload_dir` | `TC_UPLOAD_DIR` | data/uploads |
| `storage.max_upload_size_mb` | `TC_MAX_UPLOAD_SIZE_MB` | 100 |
| `logging.level` | `TC_LOG_LEVEL` | info |

Dazu kommen `TC_ADMIN_USERNAME` / `TC_ADMIN_PASSWORD` (Standard-Admin,
wird beim Start angelegt, falls er noch nicht existiert) und `RUST_LOG`
(überschreibt das Loglevel zur Laufzeit).

## Docker (Server)

```sh
# Admin-Zugangsdaten in docker-compose.yml anpassen, dann im Projektstamm:
docker compose up -d --build
```

- Die **gesamte Konfiguration** läuft über die `TC_*`-Umgebungsvariablen in
  der `docker-compose.yml` (siehe Tabelle oben) — eine `config.toml` ist
  nicht nötig. Alle Werte stehen dort auskommentiert mit ihren Defaults.
- Datenbank und Uploads liegen im benannten Volume `teamconference-data`,
  TLS-Zertifikate in `teamconference-certs` — beides überlebt Rebuilds.
- Der Standard-Admin wird über `TC_ADMIN_USERNAME` / `TC_ADMIN_PASSWORD`
  angelegt — **Passwort vor dem ersten Start ändern**.

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
