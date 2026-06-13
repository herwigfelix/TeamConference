# TeamConference Client (wxWidgets/wxDragon)

Barrierefreier Desktop-Client für TeamConference, geschrieben in Rust mit
[wxDragon](https://github.com/AllenDang/wxDragon) (Bindings für wxWidgets).
Die Oberfläche besteht aus **nativen** Bedienelementen und nutzt damit die
native Barrierefreiheitsschicht jeder Plattform: UI Automation/MSAA (Windows),
NSAccessibility (macOS) und ATK/AT-SPI (Linux). Damit funktioniert sie mit
VoiceOver, NVDA, JAWS und Orca ohne Zusatzschicht. Alle Aktionen sind per
Tastatur erreichbar.

## Bauen und Starten

```sh
cd client
cargo run --release
```

Voraussetzungen:

- **Rust** (stable); unter Windows die MSVC-Toolchain inkl. Visual Studio Build Tools
- **CMake + C++-Compiler** — wxWidgets und der Opus-Codec werden aus dem
  Quellcode gebaut (Windows: `winget install Kitware.CMake`, macOS:
  `brew install cmake` bzw. `xcode-select --install`)
- **Linux**: zusätzlich `libgtk-3-dev` (wxWidgets) und `libasound2-dev` (ALSA)

Beim **ersten** Build wird wxWidgets statisch kompiliert (dauert einige
Minuten, danach gecacht). Unter Windows startet der Release-Build ohne
Konsolenfenster (`windows_subsystem = "windows"`). Für das Gesamtpaket aus
Server und Client gibt es im Projektstamm `start.bat` (Windows) bzw.
`start.sh` (macOS/Linux).

**macOS-Mikrofon:** Die Berechtigungsabfrage erscheint nur, wenn die App als
`.app`-Bundle läuft (`compile.sh` erzeugt es und signiert es ad-hoc). Bei einem
nackten `cargo run` aus dem Terminal fragt macOS nicht — dann erbt der Prozess
die Mikrofonrechte des Terminals.

## Verbindungsansicht (Serverliste)

- **Gespeicherte Server** (Lesezeichen): auswählen füllt das Formular,
  *Als Lesezeichen speichern* legt den aktuellen Server an, *Server entfernen*
  löscht den markierten Eintrag.
- Formular: Host, Port, SSL/TLS, Benutzername, Passwort, Spitzname.
- *Verbinden* stellt die Verbindung her.

## Hauptfenster

- **Raumliste** (native Liste, Unterräume eingerückt) und darunter die
  **Nutzerliste** des aktuellen Raums. Beitreten per Knopf „Beitreten", Strg+J
  oder Doppelklick auf den Raum. (Native Listen statt Tree-Widget, weil
  Tree-Controls in wxWidgets je nach Plattform nicht screenreader-tauglich sind.)
- **Chatverlauf** (schreibgeschützt) und **Chateingabe** (Enter sendet).
- **Lautstärkeregler** (0–100 %, wirkt auf die Wiedergabe).
- **Dateiliste des aktuellen Raums** mit Herunterladen/Aktualisieren.
- Audio-Qualität (Samplerate, Bittiefe, Mono/Stereo) über *Audio → Audio-Einstellungen*.

Alles Weitere läuft über die Menüleiste (Server, Audio, Räume, Dateien,
Verwaltung, Hilfe) oder über Kurztasten.

## Kurztasten

Menü-Beschleuniger; wxWidgets bildet **Strg** auf macOS automatisch auf **Cmd** ab:

| Kurztaste | Aktion |
|---|---|
| Strg+M | Mikrofon stumm/laut |
| Strg+D | Ton aus/an (taub) |
| Strg+L | Loopback an/aus |
| Strg+S | Audiodatei streamen |
| Strg+P | Streaming pausieren/fortsetzen |
| Strg+Umschalt+S | Streaming stoppen |
| Strg+J | Ausgewähltem Raum beitreten |
| Strg+U | Datei in aktuellen Raum hochladen |
| Strg+H | Ausgewählte Datei herunterladen |
| Strg+R | Dateiliste aktualisieren |
| Strg+Umschalt+P | Privatnachricht an ausgewählten Nutzer (Text vorher ins Eingabefeld) |
| Strg+Q | Beenden |
| F1 | Kurztasten-Hilfe |

## Einstellungen

Serverliste und Lautstärke werden unter
`accessyApplications/teamconference/client.json` im plattformüblichen
Konfigverzeichnis gespeichert:

- Linux: `~/.config/accessyApplications/teamconference/client.json`
- macOS: `~/Library/Application Support/accessyApplications/teamconference/client.json`
- Windows: `%APPDATA%\accessyApplications\teamconference\client.json`

## Technik

- Steuerkanal: WebSocket (optional TLS, selbstsignierte Zertifikate werden akzeptiert)
- Audiokanal: UDP mit Opus-Kodierung (Fallback: rohes PCM), adaptiver Jitter-Puffer
- Datei-Streaming: lokale Audiodatei wird dekodiert (Symphonia), Opus-kodiert
  und in den aktuellen Raum gestreamt; Loopback wird währenddessen automatisch aktiviert
- Threading: wxWidgets-Eventloop auf dem Hauptthread, Tokio-Runtime im
  Hintergrund; Server-Nachrichten werden über einen Kanal von einem UI-Timer
  abgeholt (Widgets bleiben im UI-Thread)
