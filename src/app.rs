use std::sync::atomic::Ordering;
use std::time::Duration;

use eframe::egui::{self, Color32, RichText};
use egui_plot::{Legend, Line, Plot};
use powerwizz::engine::{self, ErrorKind, Handle, Status};
use powerwizz::metrics::{self, Point};
use powerwizz::model::{
    self, Account, AuthStrategy, MAX_DURATION_SECS, MAX_USERS, Method, OnError, Scenario, Step,
    TestDefinition,
};

const WARN: Color32 = Color32::from_rgb(230, 170, 60);
const ERR: Color32 = Color32::from_rgb(230, 90, 80);

pub struct App {
    def: TestDefinition,
    accounts: Vec<Account>,
    accounts_path: String,
    json_path: String,
    export_path: String,
    keep_records: bool,
    /// Last file operation result: (is_error, text).
    message: Option<(bool, String)>,
    start_errors: Vec<String>,
    confirm: bool,
    run: Option<Handle>,
}

impl Default for App {
    fn default() -> Self {
        App {
            def: TestDefinition::default(),
            accounts: Vec::new(),
            accounts_path: "accounts.csv".into(),
            json_path: "test.json".into(),
            export_path: "ergebnis".into(),
            keep_records: false,
            message: None,
            start_errors: Vec::new(),
            confirm: false,
            run: None,
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let running = self.running();
        if running {
            ui.ctx().request_repaint_after(Duration::from_millis(200));
        }
        egui::Panel::top("top").show(ui, |ui| self.top_bar(ui, running));
        egui::Panel::left("config")
            .default_size(560.0)
            .resizable(true)
            .show(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.add_enabled_ui(!running, |ui| self.config(ui));
                });
            });
        egui::CentralPanel::default().show(ui, |ui| self.dashboard(ui));
        if self.confirm {
            self.confirm_dialog(ui.ctx());
        }
    }
}

impl App {
    fn running(&self) -> bool {
        self.run
            .as_ref()
            .is_some_and(|h| h.shared.status() != Status::Finished)
    }

    fn status_text(&self) -> (&'static str, Color32) {
        match self.run.as_ref().map(|h| h.shared.status()) {
            _ if !self.start_errors.is_empty() => ("Fehler", ERR),
            None => ("bereit", Color32::GRAY),
            Some(Status::Running) => ("läuft", Color32::LIGHT_GREEN),
            Some(Status::Stopping) => ("stoppt", WARN),
            Some(Status::Finished) => ("abgeschlossen", Color32::LIGHT_BLUE),
        }
    }

    fn top_bar(&mut self, ui: &mut egui::Ui, running: bool) {
        ui.horizontal(|ui| {
            ui.heading("powerwizz");
            ui.separator();
            let valid = self.def.validate().is_empty();
            if ui
                .add_enabled(!running && valid, egui::Button::new("▶ Start"))
                .clicked()
            {
                self.start_errors.clear();
                self.confirm = true;
            }
            let stoppable = self
                .run
                .as_ref()
                .is_some_and(|h| h.shared.status() == Status::Running);
            if ui
                .add_enabled(stoppable, egui::Button::new("■ Stop"))
                .clicked()
                && let Some(h) = &self.run
            {
                h.stop();
            }
            let (text, color) = self.status_text();
            ui.label(RichText::new(text).color(color).strong());
        });
        ui.colored_label(
            WARN,
            "Lasttests nur gegen Systeme durchführen, für die eine Berechtigung besteht.",
        );
        for e in &self.start_errors {
            ui.colored_label(ERR, e);
        }
    }

    fn config(&mut self, ui: &mut egui::Ui) {
        let d = &mut self.def;
        ui.heading("Konfiguration");
        egui::Grid::new("general")
            .num_columns(2)
            .spacing([8.0, 4.0])
            .show(ui, |ui| {
                ui.label("Basis-URL");
                ui.add(egui::TextEdit::singleline(&mut d.base_url).desired_width(280.0));
                ui.end_row();
                ui.label("Virtuelle Benutzer");
                ui.add(egui::DragValue::new(&mut d.users).range(1..=MAX_USERS));
                ui.end_row();
                ui.label("Ramp-up (s)");
                ui.add(egui::DragValue::new(&mut d.ramp_up_secs).range(0..=d.duration_secs));
                ui.end_row();
                ui.label("Dauer (s)");
                ui.add(egui::DragValue::new(&mut d.duration_secs).range(1..=MAX_DURATION_SECS));
                ui.end_row();
                ui.label("Request-Limit");
                ui.horizontal(|ui| {
                    let mut on = d.request_limit.is_some();
                    ui.checkbox(&mut on, "");
                    match (on, &mut d.request_limit) {
                        (true, Some(n)) => {
                            ui.add(egui::DragValue::new(n).range(1..=u64::MAX));
                        }
                        (true, l @ None) => *l = Some(1000),
                        (false, l) => *l = None,
                    }
                });
                ui.end_row();
                ui.label("Timeout (s)");
                ui.add(egui::DragValue::new(&mut d.timeout_secs).range(1..=300));
                ui.end_row();
                ui.label("Bei Fehler");
                egui::ComboBox::from_id_salt("on_error")
                    .selected_text(on_error_text(d.on_error))
                    .show_ui(ui, |ui| {
                        for o in [OnError::AbortIteration, OnError::StopUser] {
                            ui.selectable_value(&mut d.on_error, o, on_error_text(o));
                        }
                    });
                ui.end_row();
                ui.label("Request-Details");
                ui.checkbox(
                    &mut self.keep_records,
                    "für CSV-Export behalten (max. 100 000)",
                );
                ui.end_row();
                ui.label("Standard-Header");
                header_lines(ui, "default_headers", &mut d.headers);
                ui.end_row();
            });

        ui.separator();
        ui.heading("Anmeldung");
        ui.horizontal(|ui| {
            for a in [
                AuthStrategy::None,
                AuthStrategy::Shared,
                AuthStrategy::PerUser,
            ] {
                ui.radio_value(&mut d.auth, a, auth_text(a));
            }
        });
        match d.auth {
            AuthStrategy::None => {}
            AuthStrategy::Shared => {
                ui.label(
                    RichText::new(
                        "Nur, wenn das Zielsystem parallele Sessions eines Accounts erlaubt.",
                    )
                    .color(WARN),
                );
                egui::Grid::new("shared").show(ui, |ui| {
                    ui.label("Benutzername");
                    ui.text_edit_singleline(&mut d.shared_username);
                    ui.end_row();
                    ui.label("Passwort");
                    ui.add(egui::TextEdit::singleline(&mut d.shared_password).password(true));
                    ui.end_row();
                });
            }
            AuthStrategy::PerUser => {
                ui.horizontal(|ui| {
                    ui.label("Accounts-CSV");
                    ui.text_edit_singleline(&mut self.accounts_path);
                    if ui.button("Importieren").clicked() {
                        match model::load_accounts(&self.accounts_path) {
                            Ok(a) => {
                                self.message =
                                    Some((false, format!("{} Accounts importiert", a.len())));
                                self.accounts = a;
                            }
                            Err(e) => self.message = Some((true, e)),
                        }
                    }
                });
                ui.label(format!(
                    "{} Accounts geladen (Spalten username,password; nur im Arbeitsspeicher)",
                    self.accounts.len()
                ));
            }
        }
        if d.auth != AuthStrategy::None {
            ui.label("Login-/Registrierungsschritte (einmal pro Benutzer; Platzhalter {{username}}, {{password}}, {{vu_id}})");
            steps_editor(ui, "login", &mut d.login_steps);
        }

        ui.separator();
        ui.heading("Szenarien");
        let mut remove = None;
        for (i, sc) in d.scenarios.iter_mut().enumerate() {
            ui.push_id(("scenario", i), |ui| {
                ui.horizontal(|ui| {
                    ui.label("Name");
                    ui.text_edit_singleline(&mut sc.name);
                    if ui
                        .small_button("🗑")
                        .on_hover_text("Szenario entfernen")
                        .clicked()
                    {
                        remove = Some(i);
                    }
                });
                steps_editor(ui, "steps", &mut sc.steps);
            });
            ui.add_space(6.0);
        }
        if let Some(i) = remove {
            d.scenarios.remove(i);
        }
        if ui.button("+ Szenario").clicked() {
            d.scenarios.push(Scenario::default());
        }

        ui.separator();
        ui.heading("Testdefinition (JSON)");
        ui.horizontal(|ui| {
            ui.text_edit_singleline(&mut self.json_path);
            if ui.button("Laden").clicked() {
                match TestDefinition::load(&self.json_path) {
                    Ok(def) => {
                        self.def = def;
                        self.message = Some((false, "Geladen".into()));
                    }
                    Err(e) => self.message = Some((true, e)),
                }
            }
            if ui.button("Speichern").clicked() {
                self.message = Some(match self.def.save(&self.json_path) {
                    Ok(()) => (false, "Gespeichert (ohne Passwörter)".into()),
                    Err(e) => (true, e),
                });
            }
        });
        if let Some((is_err, text)) = &self.message {
            ui.colored_label(if *is_err { ERR } else { Color32::GRAY }, text);
        }

        let errs = self.def.validate();
        if !errs.is_empty() {
            ui.separator();
            for e in errs {
                ui.colored_label(ERR, e);
            }
        }
    }

    fn dashboard(&mut self, ui: &mut egui::Ui) {
        let Some(h) = &self.run else {
            ui.label("Noch kein Test gelaufen.");
            return;
        };
        let sh = &h.shared;
        let runtime = sh.runtime();
        let secs = runtime.as_secs_f64().max(0.001);
        // Copy what we draw so the aggregator is blocked only briefly.
        let (m, rps, errs, p50, p95, p99, statuses, errors) = {
            let m = sh.metrics.lock().unwrap();
            let pts = |f: &dyn Fn(&Point) -> f64| -> Vec<[f64; 2]> {
                m.series.iter().map(|p| [p.t as f64, f(p)]).collect()
            };
            let ms = |d: Duration| d.as_secs_f64() * 1000.0;
            (
                Summary {
                    requests: m.requests,
                    failures: m.failures,
                    error_rate: m.error_rate(),
                    current_rps: m.current_rps(),
                    mean: m.latency.mean(),
                    p50: m.latency.percentile(0.50),
                    p95: m.latency.percentile(0.95),
                    p99: m.latency.percentile(0.99),
                    timeouts: m.kind_count(ErrorKind::Timeout),
                    transport: m.kind_count(ErrorKind::Connect) + m.kind_count(ErrorKind::Other),
                    dropped: m.dropped_records,
                },
                pts(&|p| p.requests as f64),
                pts(&|p| p.failures as f64),
                pts(&|p| ms(p.p50)),
                pts(&|p| ms(p.p95)),
                pts(&|p| ms(p.p99)),
                m.status_counts
                    .iter()
                    .map(|(k, v)| format!("{k}: {v}"))
                    .collect::<Vec<_>>(),
                m.errors.iter().rev().cloned().collect::<Vec<_>>(),
            )
        };

        ui.horizontal_wrapped(|ui| {
            kpi(
                ui,
                "Aktive Benutzer",
                format!(
                    "{} (gestartet {} / {})",
                    sh.active.load(Ordering::Relaxed),
                    sh.started.load(Ordering::Relaxed),
                    sh.users
                ),
            );
            kpi(
                ui,
                "RPS",
                format!("{} (Ø {:.1})", m.current_rps, m.requests as f64 / secs),
            );
            kpi(ui, "p95", fmt_ms(m.p95));
            kpi(
                ui,
                "Fehlerrate",
                format!("{:.2} % ({})", m.error_rate * 100.0, m.failures),
            );
            kpi(ui, "Requests", m.requests.to_string());
            kpi(
                ui,
                "Iterationen",
                sh.iterations.load(Ordering::Relaxed).to_string(),
            );
            kpi(ui, "Timeouts", m.timeouts.to_string());
            kpi(ui, "Transportfehler", m.transport.to_string());
            kpi(
                ui,
                "Laufzeit / Rest",
                format!(
                    "{} / {} s",
                    runtime.as_secs(),
                    if sh.status() == Status::Finished {
                        0
                    } else {
                        sh.duration.saturating_sub(runtime).as_secs()
                    }
                ),
            );
        });
        ui.horizontal_wrapped(|ui| {
            ui.label(format!(
                "Latenz Ø {}  p50 {}  p95 {}  p99 {}",
                fmt_ms(m.mean),
                fmt_ms(m.p50),
                fmt_ms(m.p95),
                fmt_ms(m.p99)
            ));
            ui.separator();
            ui.label(format!("Statuscodes: {}", statuses.join("  ")));
            if m.dropped > 0 {
                ui.colored_label(WARN, format!("{} Request-Details verworfen", m.dropped));
            }
        })
        .response
        .on_hover_text("Perzentile aus Histogramm, Genauigkeit ca. ±2,5 %");

        ui.horizontal(|ui| {
            ui.label("CSV-Export");
            ui.add(egui::TextEdit::singleline(&mut self.export_path).desired_width(200.0))
                .on_hover_text("Basisname: <name>-zusammenfassung.csv und <name>-requests.csv");
            let finished = sh.status() == Status::Finished;
            if ui
                .add_enabled(finished, egui::Button::new("Zusammenfassung"))
                .clicked()
            {
                let csv = metrics::summary_csv(&sh.metrics.lock().unwrap(), runtime);
                self.message = Some(write_file(
                    &format!("{}-zusammenfassung.csv", self.export_path),
                    &csv,
                ));
            }
            let has_records = !sh.metrics.lock().unwrap().records.is_empty();
            if ui
                .add_enabled(
                    finished && has_records,
                    egui::Button::new("Request-Details"),
                )
                .on_disabled_hover_text("Nur mit aktivierten Request-Details, nach Testende")
                .clicked()
            {
                let csv = metrics::records_csv(&sh.metrics.lock().unwrap());
                self.message = Some(write_file(
                    &format!("{}-requests.csv", self.export_path),
                    &csv,
                ));
            }
            if let Some((is_err, text)) = &self.message {
                ui.colored_label(if *is_err { ERR } else { Color32::GRAY }, text);
            }
        });

        let height = ((ui.available_height() - 200.0) / 2.0).max(120.0);
        ui.label("Requests und Fehler pro Sekunde");
        Plot::new("rps")
            .height(height)
            .legend(Legend::default())
            .allow_scroll(false)
            .show(ui, |p| {
                p.line(Line::new("RPS", rps).color(Color32::LIGHT_BLUE));
                p.line(Line::new("Fehler/s", errs).color(ERR));
            });
        ui.label("Latenz (ms)");
        Plot::new("latency")
            .height(height)
            .legend(Legend::default())
            .allow_scroll(false)
            .show(ui, |p| {
                p.line(Line::new("p50", p50));
                p.line(Line::new("p95", p95));
                p.line(Line::new("p99", p99));
            });

        ui.label(format!("Fehler (letzte {})", errors.len()));
        egui::ScrollArea::vertical()
            .id_salt("errors")
            .show(ui, |ui| {
                egui::Grid::new("errors")
                    .striped(true)
                    .num_columns(4)
                    .show(ui, |ui| {
                        for e in &errors {
                            ui.label(format!("{:.1} s", e.at.as_secs_f64()));
                            ui.label(&e.step);
                            ui.label(match (e.status, e.error) {
                                (Some(s), _) => s.to_string(),
                                (None, Some(k)) => k.as_str().to_string(),
                                _ => String::new(),
                            });
                            ui.label(&e.message);
                            ui.end_row();
                        }
                    });
            });
    }

    fn confirm_dialog(&mut self, ctx: &egui::Context) {
        let d = &self.def;
        let mut start = false;
        let resp = egui::Modal::new(egui::Id::new("confirm")).show(ctx, |ui| {
            ui.heading("Test starten?");
            egui::Grid::new("confirm_grid").show(ui, |ui| {
                ui.label("Ziel");
                ui.strong(&d.base_url);
                ui.end_row();
                ui.label("Benutzer / Ramp-up / Dauer");
                ui.label(format!("{} / {} s / {} s", d.users, d.ramp_up_secs, d.duration_secs));
                ui.end_row();
                ui.label("Request-Limit");
                ui.label(d.request_limit.map_or("keins".into(), |n| n.to_string()));
                ui.end_row();
                ui.label("Szenarien");
                let names: Vec<_> = d
                    .scenarios
                    .iter()
                    .map(|s| format!("{} ({} Schritte)", s.name, s.steps.len()))
                    .collect();
                ui.label(names.join(", "));
                ui.end_row();
                ui.label("Anmeldung");
                ui.label(match d.auth {
                    AuthStrategy::PerUser => format!("{} ({} Accounts)", auth_text(d.auth), self.accounts.len()),
                    a => auth_text(a).into(),
                });
                ui.end_row();
            });
            if !model::is_local_host(&d.base_url) {
                ui.colored_label(
                    WARN,
                    "Das Ziel ist kein lokaler oder privater Host. Es könnte ein Produktivsystem sein.",
                );
            }
            ui.colored_label(WARN, "Nur starten, wenn eine Berechtigung für diesen Lasttest besteht.");
            ui.horizontal(|ui| {
                start = ui.button("Berechtigt – starten").clicked();
                if ui.button("Abbrechen").clicked() {
                    self.confirm = false;
                }
            });
        });
        if resp.should_close() {
            self.confirm = false;
        }
        if start {
            self.confirm = false;
            match engine::start(self.def.clone(), self.accounts.clone(), self.keep_records) {
                Ok(h) => self.run = Some(h),
                Err(e) => self.start_errors = e,
            }
        }
    }
}

fn write_file(path: &str, text: &str) -> (bool, String) {
    match std::fs::write(path, text) {
        Ok(()) => (false, format!("Gespeichert: {path}")),
        Err(e) => (true, format!("{path}: {e}")),
    }
}

struct Summary {
    requests: u64,
    failures: u64,
    error_rate: f64,
    current_rps: u64,
    mean: Duration,
    p50: Duration,
    p95: Duration,
    p99: Duration,
    timeouts: u64,
    transport: u64,
    dropped: u64,
}

fn kpi(ui: &mut egui::Ui, label: &str, value: String) {
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.vertical(|ui| {
            ui.label(RichText::new(label).small().weak());
            ui.add(egui::Label::new(RichText::new(value).strong().size(18.0)).extend());
        });
    });
}

fn fmt_ms(d: Duration) -> String {
    format!("{:.1} ms", d.as_secs_f64() * 1000.0)
}

fn on_error_text(o: OnError) -> &'static str {
    match o {
        OnError::AbortIteration => "Iteration abbrechen",
        OnError::StopUser => "Benutzer beenden",
    }
}

fn auth_text(a: AuthStrategy) -> &'static str {
    match a {
        AuthStrategy::None => "Keine",
        AuthStrategy::Shared => "Gemeinsamer Account",
        AuthStrategy::PerUser => "Account pro Benutzer",
    }
}

enum Action {
    Up(usize),
    Down(usize),
    Remove(usize),
}

fn steps_editor(ui: &mut egui::Ui, salt: &str, steps: &mut Vec<Step>) {
    let mut action = None;
    ui.push_id(salt, |ui| {
        for (i, s) in steps.iter_mut().enumerate() {
            ui.push_id(i, |ui| {
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.add(egui::TextEdit::singleline(&mut s.name).desired_width(90.0));
                        egui::ComboBox::from_id_salt("method")
                            .width(70.0)
                            .selected_text(s.method.as_str())
                            .show_ui(ui, |ui| {
                                for m in Method::ALL {
                                    ui.selectable_value(&mut s.method, m, m.as_str());
                                }
                            });
                        ui.add(egui::TextEdit::singleline(&mut s.url).hint_text("/pfad oder URL"));
                    });
                    ui.horizontal(|ui| {
                        ui.label("Pause ms");
                        ui.add(egui::DragValue::new(&mut s.pause_min_ms).range(0..=s.pause_max_ms));
                        ui.label("–");
                        ui.add(
                            egui::DragValue::new(&mut s.pause_max_ms)
                                .range(s.pause_min_ms..=600_000),
                        );
                        ui.label("Status")
                            .on_hover_text("Kommagetrennt, leer = 2xx");
                        status_list(ui, &mut s.expected_status);
                        if ui.small_button("⬆").clicked() {
                            action = Some(Action::Up(i));
                        }
                        if ui.small_button("⬇").clicked() {
                            action = Some(Action::Down(i));
                        }
                        if ui.small_button("🗑").clicked() {
                            action = Some(Action::Remove(i));
                        }
                    });
                    egui::CollapsingHeader::new("Header / Body").show(ui, |ui| {
                        header_lines(ui, "headers", &mut s.headers);
                        ui.add(
                            egui::TextEdit::multiline(&mut s.body)
                                .code_editor()
                                .desired_rows(2)
                                .hint_text("Body (Text/JSON)"),
                        );
                    });
                });
            });
        }
    });
    match action {
        Some(Action::Up(i)) if i > 0 => steps.swap(i, i - 1),
        Some(Action::Down(i)) if i + 1 < steps.len() => steps.swap(i, i + 1),
        Some(Action::Remove(i)) => {
            steps.remove(i);
        }
        _ => {}
    }
    if ui.small_button("+ Schritt").clicked() {
        steps.push(Step::default());
    }
}

/// Text field that edits `value` through a text representation. The text is kept
/// across frames so half-typed input ("200,") survives; `value` changes only when
/// the text parses.
fn buffered<T>(
    ui: &mut egui::Ui,
    salt: &str,
    value: &mut T,
    multiline: bool,
    to_text: impl Fn(&T) -> String,
    parse: impl Fn(&str) -> Option<T>,
) {
    let id = ui.id().with(salt);
    let canonical = to_text(value);
    // (edit buffer, canonical text when the buffer was last in sync)
    let mut st: (String, String) = ui.data(|d| d.get_temp(id)).unwrap_or_default();
    if st.1 != canonical {
        st = (canonical.clone(), canonical);
    }
    let color = parse(&st.0).is_none().then_some(ERR);
    let edit = if multiline {
        egui::TextEdit::multiline(&mut st.0)
            .desired_rows(1)
            .hint_text("Name: Wert")
    } else {
        egui::TextEdit::singleline(&mut st.0).desired_width(80.0)
    };
    let resp = ui.add(edit.text_color_opt(color));
    if resp.changed()
        && let Some(v) = parse(&st.0)
    {
        *value = v;
        st.1 = to_text(value);
    }
    ui.data_mut(|d| d.insert_temp(id, st));
}

fn status_list(ui: &mut egui::Ui, codes: &mut Vec<u16>) {
    buffered(
        ui,
        "status",
        codes,
        false,
        |c| c.iter().map(u16::to_string).collect::<Vec<_>>().join(","),
        |t| {
            t.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| s.parse().ok())
                .collect()
        },
    );
}

fn header_lines(ui: &mut egui::Ui, salt: &str, headers: &mut Vec<(String, String)>) {
    buffered(
        ui,
        salt,
        headers,
        true,
        |h| {
            h.iter()
                .map(|(k, v)| format!("{k}: {v}"))
                .collect::<Vec<_>>()
                .join("\n")
        },
        |t| {
            t.lines()
                .filter(|l| !l.trim().is_empty())
                .map(|l| {
                    l.split_once(':')
                        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
                })
                .collect()
        },
    );
}
