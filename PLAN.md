# powerwizz – Rust Web Load Tester

## 1. Ziel

Entwickle ein kleines, performantes natives Windows-Tool, das eine Webapplikation mit konfigurierbaren virtuellen Benutzern unter Last setzt.

Jeder virtuelle Benutzer (VU):

- besitzt eine eigene Session inklusive Cookies,
- kann sich optional anmelden,
- führt einen konfigurierten Ablauf aus mehreren HTTP-Schritten aus,
- wartet zwischen Aktionen mit konfigurierbarer Zufälligkeit,
- protokolliert Latenzen, HTTP-Statuscodes und Fehler.

Die Anwendung zeigt laufende Last und Ergebnisse live in einer nativen Windows-GUI an und kann einen Testbericht als CSV exportieren.

## 2. YAGNI und verbindliche Leitlinien

1. Implementiere nur Funktionen, die für die Akzeptanzkriterien erforderlich sind.
2. Keine Microservices, Serverkomponente, Datenbank, Telemetrie, Cloud-Anbindung, Logging-Infrastruktur oder Plugin-Architektur.
3. Keine Browserautomatisierung in Version 1. HTTP(S) wird direkt ausgeführt.
4. Keine OS-Threads pro VU. Verwende asynchrone Tasks und begrenze Parallelität.
5. Keine generischen Abstraktionsschichten (Traits, Factories) mit nur einer Implementierung.
6. Konfiguration und Ergebnisse bleiben im Arbeitsspeicher; Testdefinitionen können als JSON-Datei gespeichert und geladen werden.
7. Keine zusätzliche Dependency, wenn Standardbibliothek oder bestehende Abhängigkeit ausreicht.
8. UI-Rendering darf den Load-Generator nicht blockieren.
9. Markiere bewusste Einschränkungen und Performance-Kompromisse mit `// ponytail:` inklusive Begründung und Upgrade-Pfad.
10. Verständlicher, idiomatischer Rust-Code; `unsafe` nur, wenn zwingend erforderlich.
11. Erst eine funktionierende Ende-zu-Ende-Version, danach nur anhand konkreter Anforderungen erweitern.

## 3. Zielplattform und Technologie

- Sprache: Rust stable, Edition 2024.
- Plattform: Windows 10/11 x64 (Entwicklung/Tests laufen auch unter Linux/WSL; nur der Release-Build ist Windows-spezifisch).
- GUI: `eframe`/`egui`, orientiert an der dichten, nativen Dashboard-Optik von reswizz.
- Async Runtime: `tokio` mit minimal benötigten Features (`rt-multi-thread`, `time`, `sync`, `macros`).
- HTTP: `reqwest` mit `rustls-tls` und `cookies`, ohne Default-Features.
- Serialisierung: `serde`, `serde_json`.
- Zufall (Jitter, Szenariowahl): `fastrand`.
- Diagramme: `egui_plot` (offizielles Plot-Crate; egui selbst enthält keine Plots mehr). Keine weitere Chart-Engine.
- CSV: `csv`-Crate – wird für den Import von Zugangsdaten (Quoting, Kommas in Passwörtern) ohnehin korrekt gebraucht und deckt damit auch den Export ab.
- Dateiauswahl: in Version 1 ein Pfad-Textfeld. `// ponytail:` Datei-Dialog (`rfd`) erst, wenn das Textfeld stört.

Vor dem Hinzufügen weiterer Crates prüfen, ob eine bestehende Dependency die Funktion abdeckt.

## 4. Umfang Version 1

### Muss-Funktionen

- Basis-URL und optionale Standard-Header konfigurieren.
- Anzahl virtueller Benutzer festlegen (Obergrenze siehe §10).
- Ramp-up-Dauer: VU-Starts gleichmäßig über diese Zeit verteilen (Start von VU *i* bei `i * rampup / n`).
- Testdauer (Pflicht) und optionales globales Request-Limit; der Test endet bei dem, was zuerst eintritt.
- Szenarien als geordnete Liste von HTTP-Schritten:
  - Methode: GET, POST, PUT, PATCH, DELETE, HEAD.
  - absolute URL oder Pfad relativ zur Basis-URL.
  - optionale Header.
  - optionaler Request-Body als Text/JSON.
  - optionale Pause nach dem Schritt (Min/Max in ms, gleichverteilt).
  - erwartete Statuscodes, standardmäßig 200–299.
- Isolierte Session-Cookies pro VU.
- Optionale Anmeldung über konfigurierbare HTTP-Schritte (siehe §6).
- Start, Stop und klar sichtbarer Teststatus.
- Live-Metriken und Fehlerübersicht.
- CSV-Export der aggregierten Ergebnisse und optional der einzelnen Requests (begrenzt).
- JSON-Testdefinition laden und speichern.
- Eingabevalidierung mit verständlichen Fehlermeldungen.
- Deutliche Warnung, dass Lasttests nur gegen Systeme mit Berechtigung durchgeführt werden dürfen.

### Nicht Teil von Version 1

- Echte Browser, JavaScript, DOM, Selenium/Playwright.
- Verteilte Lastgeneratoren.
- Benutzerverwaltung im Zielsystem außerhalb explizit konfigurierter HTTP-Schritte.
- CAPTCHA-, MFA- oder sonstige Umgehung von Schutzmechanismen.
- Automatische Erkennung von Navigationspfaden.
- Extraktion aus HTML (z. B. CSRF-Token in einem Formular) und per Regex. Werte aus JSON-Antworten (#27), Response-Headern und Set-Cookie (#29) werden unterstützt, siehe §6.
- Cloud-Dienste, Accounts, Telemetrie, Lizenzserver, Datenbank.
- Scripting-Sprache, Plugin-System, Variablen-Engine (Ausnahme: die festen Platzhalter aus §6) oder Workflow-IDE.
- HTTP/3, WebSockets, Browser-Rendering.
- Automatische Laststeigerung bis zum Ausfall des Zielsystems.
- Automatische Retries.

## 5. Benutzer- und Szenariomodell

Ein VU ist eine asynchrone Task, kein OS-Thread. Jeder VU besitzt:

- eine eindeutige ID (0..n),
- einen eigenen `reqwest::Client` mit eigener Cookie-Jar,
- eine eigene Ablaufposition und Zähler für erfolgreiche/fehlgeschlagene Schritte,
- optional ein Szenario, das pro Iteration zufällig (gleichverteilt) aus den konfigurierten Szenarien gewählt wird.

Verbindungen werden innerhalb eines VU wiederverwendet (Keep-Alive des eigenen Clients). Da jeder VU einen eigenen Client hat, werden Verbindungen **nicht** zwischen VUs geteilt – das entspricht realen Browsern und ist gewollt.

### Lebenszyklus eines VU

1. Start zum Ramp-up-Zeitpunkt.
2. Optional: Login-Schritte einmal ausführen. Schlägt ein Login-Schritt fehl, wird der Fehler gezählt und der VU beendet sich (kein Retry).
3. Iteration: Szenario wählen und Schritte der Reihe nach ausführen. Schlägt ein Schritt fehl (Transportfehler, Timeout oder unerwarteter Status), wird der Fehler gezählt und die **aktuelle Iteration** abgebrochen. Antwortet ein Schritt unerwartet mit **401** und es gibt Login-Schritte, meldet sich der VU neu an (abgelaufene Session/JWT) und setzt mit der nächsten Iteration fort; der 401 zählt als Fehler, der Request wird nicht wiederholt. Folgt direkt auf eine Neuanmeldung wieder ein 401, gilt das normale Fehlerverhalten (kein Login-Sturm).
4. Nächste Iteration, bis Testdauer abgelaufen, Request-Limit erreicht oder Stop ausgelöst.

Konfigurierbar ist in v1 nur: „bei Fehler Iteration abbrechen“ (Standard) oder „bei Fehler VU beenden“.

Die UI unterscheidet:

- konfigurierte VUs,
- gestartete VUs,
- aktuell aktive VUs,
- abgeschlossene Iterationen.

## 6. Authentifizierung und Testdaten

Das Programm darf nicht unterstellen, dass beliebig viele Konten existieren oder erstellt werden können.

Authentifizierungsstrategien (eine pro Test):

- **Keine** – keine Login-Schritte.
- **Gemeinsamer Account** – alle VUs nutzen dieselben Zugangsdaten. Nur sinnvoll, wenn das Zielsystem parallele Sessions eines Accounts unterstützt; die UI weist darauf hin.
- **Account pro VU** – Zugangsdaten aus einer CSV-Datei (`username,password`, Header-Zeile). VU *i* erhält Zeile *i*. Gibt es weniger Zeilen als VUs, ist der Start nicht möglich (Validierungsfehler).

Login- und optionale Registrierungsschritte sind normale HTTP-Schritte. In URL, Headern und Body werden ersetzt: die festen Platzhalter `{{username}}`, `{{password}}`, `{{vu_id}}` sowie extrahierte Werte. Keine weitere Template-Logik.

**JWT / Token aus Antworten (#27, #29):** Ein Schritt kann mit `extract` (Zeilen `name = quelle`) Werte aus seiner Antwort übernehmen. Quelle: JSON-Pointer (`/access_token`), `header:Name` (case-insensitiv, führendes `Bearer ` entfernt, erster gleichnamiger Header gilt) oder `cookie:name` (Wert aus `Set-Cookie` ohne Attribute). Die Werte gelten pro VU als `{{name}}` (z. B. Standard-Header `Authorization: Bearer {{token}}`). Header mit noch unbelegten Platzhaltern werden nicht gesendet. Unbekannte Platzhalter sind Validierungsfehler. Extrahierte Werte bleiben im Speicher des VU und erscheinen nie in Records, Exporten oder Fehlermeldungen.

Regeln:

- Zugangsdaten nur im Arbeitsspeicher der laufenden Sitzung.
- Passwörter werden nie in gespeicherte Testdefinitionen, Logs, Fehlermeldungen oder Exporte geschrieben (Feld mit `#[serde(skip)]`; Test prüft das).
- Der Registrierungsablauf wird nie automatisch aus Login-Feldern abgeleitet.
- Vor Testbeginn zeigt die UI die gewählte Strategie und Anzahl verfügbarer Accounts.

## 7. Architektur

Startpunkt sind wenige Dateien; aufteilen erst, wenn eine Datei unübersichtlich wird:

| Datei | Inhalt |
|---|---|
| `main.rs` | GUI-Einstieg, Tokio-Runtime auf eigenem Thread |
| `app.rs` | egui-Oberfläche, Eingaben, Steuerung, Darstellung |
| `model.rs` | Testdefinition, Szenarien, Schritte, Validierung, JSON |
| `engine.rs` | Start/Stop, Ramp-up, Laufzeit, Request-Limit, VU-Ausführung |
| `metrics.rs` | Aggregation, Histogramm, Zeitfenster, CSV-Export |

`http_user.rs` und `export.rs` nur abspalten, wenn `engine.rs`/`metrics.rs` zu groß werden.

### Trennung von GUI und Lastgenerator

- Die Engine ist ohne GUI nutzbar (`engine::start(def, accounts, keep_records) -> Result<Handle, Vec<String>>`, `Handle::stop()`, `Handle::wait()`). Integrationstests treiben die Engine direkt an.
- VU-Tasks senden Request-Ergebnisse über einen **begrenzten** `tokio::mpsc`-Kanal (10 000) an einen Aggregator-Task, der sie gebündelt in `Metrics` einträgt und alle 200 ms das Zeitfenster weiterschaltet.
- `Metrics` liegt hinter einem `Mutex` in `engine::Shared` (dazu atomare Zähler für gestartete/aktive VUs und Iterationen). Die GUI sperrt nur kurz zum Lesen und ruft `ctx.request_repaint_after(200 ms)` auf. `// ponytail:` Snapshots über `watch`, falls GUI-Lesezugriffe den Aggregator messbar bremsen.
- Stop: `tokio::sync::watch<bool>` verhindert neue VU-Starts; jeder VU läuft in einem `select!` gegen das Signal, laufende Requests und Pausen werden dadurch abgebrochen und nicht gezählt. Beim Request-Limit stellen VUs neue Requests ein, laufende werden noch gezählt; sind alle VUs beendet (Limit, fehlgeschlagener Login), endet der Test.
- Request-Timeout (pro Request) und globales Testende (Dauer + kurze Nachlaufzeit) verhindern hängende Tests.

## 8. Messwerte

Mindestens anzeigen:

- RPS (über das letzte 1-s-Fenster und Gesamtdurchschnitt),
- aktive VUs,
- Gesamtzahl Requests,
- Erfolgs- und Fehlerrate,
- Statuscode-Verteilung,
- Latenz: Mittelwert, p50, p95, p99,
- Timeouts und Transportfehler getrennt,
- Laufzeit und Restdauer.

Latenzen werden aus der gemessenen Request-Dauer (bis Body vollständig gelesen) erfasst. Perzentile kommen aus einem **festen logarithmischen Histogramm** (275 Buckets, je 5 % breiter, 100 µs bis 60 s; Ausgabe als geometrische Bucket-Mitte): konstanter Speicher, relativer Fehler ≤ ~2,5 %, Werte außerhalb des Bereichs werden an die Grenzen geklemmt. Ein Histogramm für den Gesamttest, eines pro Schritt und eines pro 1-s-Zeitfenster für die Diagramme (Zeitreihe max. 3700 Punkte). Requests werden dem Fenster zugeordnet, in dem sie enden.

Fehlerarten: `status` (unerwarteter Code), `timeout`, `connect`, `other`.

Ein Request-Datensatz (für den optionalen Detail-Export) enthält höchstens: Zeit relativ zum Teststart, VU-ID, Szenario/Schritt, Methode, Pfad **ohne Query-String**, Statuscode, Dauer, Fehlerart. Keine Passwörter, Cookies, Authorization-Header oder Bodies.

Begrenzungen:

- Detail-Datensätze: max. 100 000, danach verworfen und ein Zähler „verworfen“ angezeigt.
- Fehlerbeispiele: die letzten 200 (Ringpuffer), Meldungen auf 200 Zeichen gekürzt.

## 9. Windows UI

Angelehnt an reswizz: kompakte Dashboard-Oberfläche, dunkles Theme, sparsame Farben, keine Animationen.

1. **Konfiguration** – Basis-URL, VUs, Ramp-up, Dauer, Request-Limit, Timeout, Auth-Strategie, Szenario laden/speichern/bearbeiten.
2. **Steuerung** – Start, Stop, Status: bereit, läuft, stoppt, abgeschlossen, Fehler.
3. **Live-Kennzahlen** – aktive VUs, RPS, p95, Fehlerrate, Requests.
4. **Diagramme** – RPS und Fehlerrate über Zeit; p50/p95/p99 über Zeit.
5. **Fehlerliste** – Zeitpunkt, Schritt, Status/Fehlerart, gekürzte bereinigte Meldung.
6. **Ergebnisse** – Zusammenfassung, CSV-Export.

Szenario-Editor: einfache Tabelle (`egui_extras` nur falls `egui::Grid` nicht reicht); Schritte hinzufügen, entfernen, nach oben/unten verschieben. Kein Drag-and-drop.

Während eines laufenden Tests ist die Konfiguration schreibgeschützt.

## 10. Lastkontrolle und Sicherheit

- `MAX_USERS = 500` als Konstante, bis Messungen eine Anpassung rechtfertigen.
- Request-Timeout (Standard 30 s) und maximale Testdauer (Standard-Obergrenze 1 h).
- Ramp-up gegen unbeabsichtigte Lastspitzen.
- Keine unbegrenzten Tasks, Kanäle, Logs oder Samples.
- Keine automatischen Retries.
- Vor dem Start ein Bestätigungsdialog mit Host, VUs, Dauer, Szenario und Berechtigungshinweis.
- Zusätzliche Warnung, wenn der Host **nicht** `localhost`, eine Loopback- oder private IP ist. Keine Behauptung, Produktionssysteme könnten automatisch erkannt werden.
- Stop beendet neue VU-Starts und bricht laufende Requests ab.
- Keine Umgehung von Rate-Limits, Authentifizierung oder Sicherheitskontrollen.

## 11. Tests

Lokaler Testserver: minimaler HTTP/1.1-Server auf `tokio::net::TcpListener` im Testcode (keine zusätzliche Dependency). Routen für feste Statuscodes, Verzögerung, `Set-Cookie`/Cookie-Echo und Verbindungsabbruch. Er wird **zusammen mit der HTTP-Ausführung** gebaut, nicht erst am Ende.

Unit-Tests:

- Validierung von Testdefinitionen.
- Ramp-up-Zeitpunkte.
- Metrik-Aggregation und RPS.
- Histogramm-Perzentile gegen bekannte Verteilungen innerhalb der dokumentierten Toleranz.
- CSV-Escaping; Passwörter erscheinen weder in JSON noch in CSV.
- Platzhalterersetzung.

Integrationstests (gegen den lokalen Testserver):

- Cookies verschiedener VUs bleiben getrennt.
- Ramp-up und `MAX_USERS` werden eingehalten.
- Stop beendet den Test; danach starten keine VUs mehr.
- Timeout wird als `timeout` gezählt; der Test endet trotzdem pünktlich.
- Request-Limit beendet den Test.

Qualitätschecks (lokal und in GitHub Actions auf `windows-latest`):

- `cargo fmt --check`
- `cargo clippy --all-targets -- -D warnings`
- `cargo test`
- `cargo build --release` (x64-EXE als Build-Artefakt)

## 12. Akzeptanzkriterien

Version 1 ist fertig, wenn:

1. die Anwendung als native Windows-x64-EXE startet,
2. Basis-URL, Szenario, VU-Zahl, Ramp-up und Dauer einstellbar sind,
3. mehrere VUs parallel HTTP-Abläufe ausführen,
4. Session-Cookies zwischen VUs isoliert sind,
5. Start, Stop, Timeout und Testende zuverlässig funktionieren,
6. GUI-Metriken live aktualisiert werden, ohne den Generator zu blockieren,
7. RPS, Fehlerquote und p50/p95/p99 angezeigt werden,
8. Testdefinitionen als JSON geladen und gespeichert werden,
9. Ergebnisse als CSV exportiert werden, ohne Zugangsdaten zu leaken,
10. automatisierte Tests mit lokalem Testserver laufen,
11. keine unbeschränkten Task- oder Ergebnisstrukturen verwendet werden,
12. Formatierung, Clippy, Tests und Windows-Release-Build in CI grün sind.

## 13. Implementierungsreihenfolge

Jeder Schritt ist ein GitHub-Issue.

1. Skeleton: Cargo-Projekt, leeres eframe-Fenster, CI-Workflow.
2. Datenmodell, Validierung, JSON laden/speichern.
3. HTTP-Schrittausführung pro VU (Cookies, Timeout, Statusprüfung, Platzhalter) + lokaler Testserver.
4. Metriken: Histogramm, Zeitfenster, Fehlerbeispiele, begrenzte Request-Datensätze.
5. Load Engine: Ramp-up, Iterationen, Dauer, Request-Limit, Stop, Snapshots.
6. Authentifizierung: Strategien, CSV-Import der Accounts.
7. GUI: Konfiguration, Szenario-Editor, Start/Stop, Bestätigungsdialog.
8. GUI: Live-Dashboard, Diagramme, Fehlerliste.
9. CSV-Export (Aggregat + optional Details).
10. Release-Prüfung: Ressourcenverbrauch messen und dokumentieren.
11. Erst danach konkrete Engpässe messen und gezielt optimieren.

## 14. Arbeitsanweisung an Claude Code

Arbeite direkt im bestehenden Repository. Lies zuerst Struktur und vorhandene PLAN.md/README.md-Konventionen. Setze das Konzept issue-weise um. Implementiere keine ausgeschlossenen Funktionen. Wähle bei unklaren Details die einfachste Lösung, die die Akzeptanzkriterien erfüllt, und dokumentiere Einschränkungen. Führe nach jedem Schritt passende Tests aus. Erfinde keine Performance-Zahlen: miss den Ressourcenverbrauch mit dem lokalen Testserver und dokumentiere die Testbedingungen.
