# TeamConference Client (Slint)

Barrierefreier Desktop-Client für TeamConference, geschrieben in Rust mit
[Slint](https://slint.dev). Slint nutzt AccessKit und ist damit für
Screenreader (VoiceOver, NVDA, JAWS, Orca) zugänglich; alle Bedienelemente
tragen deutsche `accessible-label`-Beschriftungen und sind vollständig per
Tastatur erreichbar (Tab/Umschalt+Tab).

## Bauen und Starten

```sh
cd client
cargo run --release
```

Voraussetzungen:

- **Rust** (stable); unter Windows die MSVC-Toolchain inkl. Visual Studio Build Tools
- **CMake** — zum Bauen des mitgelieferten Opus-Codecs
  (Windows: `winget install Kitware.CMake`, macOS: `brew install cmake`)
- **Linux**: zusätzlich `libasound2-dev` (ALSA) und die üblichen GUI-Pakete für winit

Unter Windows startet der Release-Build ohne Konsolenfenster
(`windows_subsystem = "windows"`). Für das Gesamtpaket aus Server und Client
gibt es im Projektstamm `start.bat` (Windows) bzw. `start.sh` (macOS/Linux).

## Aufbau des Hauptfensters

- **Chatverlauf** (Textfeld, schreibgeschützt, per Tastatur lesbar)
- **Chateingabefeld** mit Senden-Knopf (Enter sendet)
- **Liste der Räume und Unterräume** (Unterräume eingerückt, Passwort-Räume gekennzeichnet)
- **Liste der Nutzer im aktuellen Raum** (mit Rolle, stumm/taub-Status)
- **Lautstärkeregler** (0–100 %, wirkt auf die Wiedergabe)
- **Dateiliste des aktuellen Raums** mit Herunterladen/Aktualisieren

Alles Weitere läuft über die Menüleiste (Server, Audio, Räume, Dateien,
Verwaltung, Hilfe) oder über Kurztasten.

## Kurztasten

Unter Windows/Linux **Strg**, unter macOS **Cmd** (Strg funktioniert dort ebenfalls):

| Kurztaste | Aktion |
|---|---|
| Strg+M | Mikrofon stumm/laut |
| Strg+D | Ton aus/an (taub) |
| Strg+L | Loopback an/aus |
| Strg+S | Audiodatei streamen |
| Strg+Umschalt+S | Streaming stoppen |
| Strg+J | Ausgewähltem Raum beitreten |
| Strg+U | Datei in aktuellen Raum hochladen |
| Strg+H | Ausgewählte Datei herunterladen |
| Strg+R | Dateiliste aktualisieren |
| Strg+P | Privatnachricht an ausgewählten Nutzer (Text vorher ins Eingabefeld) |
| Strg+Q | Beenden |
| F1 | Kurztasten-Hilfe |
| Escape | Dialog schließen |

## Einstellungen

Server, Benutzername, Spitzname, Audiogeräte und Lautstärke werden in
`~/.config/teamconference/client.json` (macOS: `~/Library/Application
Support/teamconference/client.json`) gespeichert. Audiogeräte lassen sich
über *Server → Einstellungen…* wählen und gelten ab der nächsten Verbindung.

## Technik

- Steuerkanal: WebSocket (optional TLS, selbstsignierte Zertifikate werden akzeptiert)
- Audiokanal: UDP mit Opus-Kodierung (Fallback: rohes PCM), adaptiver Jitter-Puffer
- Datei-Streaming: lokale Audiodatei wird dekodiert (Symphonia), Opus-kodiert
  und in den aktuellen Raum gestreamt; Loopback wird währenddessen automatisch aktiviert
