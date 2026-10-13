//! reswizz-style single page: status strip on top, configuration column left,
//! load / latency / errors boxes right.

use std::sync::atomic::Ordering;
use std::time::Duration;

use eframe::egui::{
    self, Align2, Color32, FontFamily, FontId, Margin, Pos2, Rect, RichText, Sense, Stroke, Ui,
    Vec2, vec2,
};
use powerwizz::engine::{self, ErrorKind, Handle, Status};
use powerwizz::metrics;
use powerwizz::model::{
    self, Account, AuthStrategy, MAX_DURATION_SECS, MAX_USERS, Method, OnError, Scenario, Step,
    TestDefinition,
};

// Palette shared with reswizz.
const BG: Color32 = Color32::from_rgb(0x10, 0x12, 0x18);
const WELL: Color32 = Color32::from_rgb(0x16, 0x19, 0x21);
const DIM: Color32 = Color32::from_rgb(0x2c, 0x31, 0x3c);
const TEXT: Color32 = Color32::from_rgb(0xc8, 0xcc, 0xd4);
const MUTED: Color32 = Color32::from_rgb(0x7a, 0x80, 0x8c);
const C_LOAD: Color32 = Color32::from_rgb(0x7f, 0xc8, 0x8f);
const C_AUTH: Color32 = Color32::from_rgb(0xe0, 0xb0, 0x6a);
const C_ERR: Color32 = Color32::from_rgb(0xe0, 0x7f, 0x86);
const C_SCEN: Color32 = Color32::from_rgb(0xb3, 0x8f, 0xe8);
const C_LAT: Color32 = Color32::from_rgb(0x6a, 0xd0, 0xc8);
const C_CFG: Color32 = Color32::from_rgb(0x6f, 0xb6, 0xe0);
const LOW: Color32 = Color32::from_rgb(0x50, 0xd0, 0x80);
const MID: Color32 = Color32::from_rgb(0xf0, 0xd0, 0x60);
const HIGH: Color32 = Color32::from_rgb(0xf0, 0x55, 0x55);
const WARN: Color32 = MID;
const ERR: Color32 = HIGH;

pub fn setup_style(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Dark);
    ctx.all_styles_mut(|s| {
        for f in s.text_styles.values_mut() {
            f.family = FontFamily::Monospace;
            f.size = f.size.min(13.0);
        }
        s.visuals.panel_fill = BG;
        s.visuals.window_fill = BG;
        s.visuals.extreme_bg_color = WELL;
        s.visuals.override_text_color = Some(TEXT);
        s.visuals.widgets.noninteractive.bg_stroke.color = DIM;
        s.spacing.item_spacing = vec2(6.0, 3.0);
    });
}

/// Green → yellow → red for 0.0 → 1.0.
fn heat(t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    if t < 0.5 {
        LOW.lerp_to_gamma(MID, t * 2.0)
    } else {
        MID.lerp_to_gamma(HIGH, (t - 0.5) * 2.0)
    }
}

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
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let running = self.running();
        if running {
            ui.ctx().request_repaint_after(Duration::from_millis(200));
        }
        let panel = |m| egui::Frame::NONE.fill(BG).inner_margin(m);
        egui::Panel::top("top")
            .show_separator_line(false)
            .frame(panel(Margin::symmetric(8, 6)))
            .show(ui, |ui| self.top_bar(ui, running));
        let side_w = (ui.available_width() * 0.36).clamp(420.0, 600.0);
        egui::Panel::left("config")
            .default_size(side_w)
            .resizable(true)
            .show_separator_line(false)
            .frame(panel(Margin::symmetric(6, 8)))
            .show(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.add_enabled_ui(!running, |ui| self.config(ui));
                });
            });
        egui::CentralPanel::default()
            .frame(panel(Margin::symmetric(6, 8)))
            .show(ui, |ui| self.dashboard(ui));
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
            _ if !self.start_errors.is_empty() => ("fehler", HIGH),
            None => ("bereit", MUTED),
            Some(Status::Running) => ("läuft", LOW),
            Some(Status::Stopping) => ("stoppt", MID),
            Some(Status::Finished) => ("abgeschlossen", C_CFG),
        }
    }

    fn top_bar(&mut self, ui: &mut Ui, running: bool) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("powerwizz").color(C_LOAD).strong().size(15.0));
            ui.add_space(8.0);
            let valid = self.def.validate().is_empty();
            if ui
                .add_enabled(!running && valid, egui::Button::new("▶ start"))
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
                .add_enabled(stoppable, egui::Button::new("■ stop"))
                .clicked()
                && let Some(h) = &self.run
            {
                h.stop();
            }
            let (text, color) = self.status_text();
            ui.label(RichText::new(text).color(color).strong());
            if let Some(h) = &self.run {
                let sh = &h.shared;
                let (rt, total) = (sh.runtime().as_secs_f32(), sh.duration.as_secs_f32());
                ui.add_space(8.0);
                meter(ui, rt / total, 160.0, |_| C_CFG);
                ui.label(
                    RichText::new(format!("{:.0} / {:.0} s", rt.min(total), total)).color(MUTED),
                );
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    RichText::new("nur Systeme testen, für die eine Berechtigung besteht")
                        .color(WARN),
                );
            });
        });
        for e in &self.start_errors {
            ui.colored_label(ERR, e);
        }
    }

    fn config(&mut self, ui: &mut Ui) {
        let d = &mut self.def;
        boxed(ui, "konfiguration", C_CFG, false, |ui| {
            egui::Grid::new("general")
                .num_columns(2)
                .spacing([10.0, 3.0])
                .show(ui, |ui| {
                    key(ui, "basis-url");
                    ui.add(
                        egui::TextEdit::singleline(&mut d.base_url).desired_width(f32::INFINITY),
                    );
                    ui.end_row();
                    key(ui, "benutzer");
                    ui.add(egui::DragValue::new(&mut d.users).range(1..=MAX_USERS));
                    ui.end_row();
                    key(ui, "ramp-up s");
                    ui.add(egui::DragValue::new(&mut d.ramp_up_secs).range(0..=d.duration_secs));
                    ui.end_row();
                    key(ui, "dauer s");
                    ui.add(egui::DragValue::new(&mut d.duration_secs).range(1..=MAX_DURATION_SECS));
                    ui.end_row();
                    key(ui, "request-limit");
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
                    key(ui, "timeout s");
                    ui.add(egui::DragValue::new(&mut d.timeout_secs).range(1..=300));
                    ui.end_row();
                    key(ui, "bei fehler");
                    egui::ComboBox::from_id_salt("on_error")
                        .selected_text(on_error_text(d.on_error))
                        .show_ui(ui, |ui| {
                            for o in [OnError::AbortIteration, OnError::StopUser] {
                                ui.selectable_value(&mut d.on_error, o, on_error_text(o));
                            }
                        });
                    ui.end_row();
                    key(ui, "details");
                    ui.checkbox(&mut self.keep_records, "requests behalten (max. 100 000)");
                    ui.end_row();
                    key(ui, "header");
                    header_lines(ui, "default_headers", &mut d.headers);
                    ui.end_row();
                });
        });

        boxed(ui, "anmeldung", C_AUTH, false, |ui| {
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
                    hint(
                        ui,
                        "nur, wenn das Ziel parallele Sessions eines Accounts erlaubt",
                        WARN,
                    );
                    egui::Grid::new("shared")
                        .spacing([10.0, 3.0])
                        .show(ui, |ui| {
                            key(ui, "benutzer");
                            ui.text_edit_singleline(&mut d.shared_username);
                            ui.end_row();
                            key(ui, "passwort");
                            ui.add(
                                egui::TextEdit::singleline(&mut d.shared_password).password(true),
                            );
                            ui.end_row();
                        });
                }
                AuthStrategy::PerUser => {
                    ui.horizontal(|ui| {
                        key(ui, "accounts-csv");
                        ui.text_edit_singleline(&mut self.accounts_path);
                        if ui.button("importieren").clicked() {
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
                    hint(
                        ui,
                        &format!(
                            "{} accounts geladen · spalten username,password · nur im speicher",
                            self.accounts.len()
                        ),
                        MUTED,
                    );
                }
            }
            if d.auth != AuthStrategy::None {
                hint(
                    ui,
                    "login-/registrierungsschritte, einmal pro benutzer und erneut bei 401 · {{username}} {{password}} {{vu_id}} + extrahierte werte (z. B. jwt)",
                    MUTED,
                );
                steps_editor(ui, "login", &mut d.login_steps);
            }
        });

        boxed(ui, "szenarien", C_SCEN, false, |ui| {
            let mut remove = None;
            for (i, sc) in d.scenarios.iter_mut().enumerate() {
                ui.push_id(("scenario", i), |ui| {
                    ui.horizontal(|ui| {
                        key(ui, "name");
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
                ui.add_space(4.0);
            }
            if let Some(i) = remove {
                d.scenarios.remove(i);
            }
            if ui.button("+ szenario").clicked() {
                d.scenarios.push(Scenario::default());
            }
        });

        boxed(ui, "testdefinition", MUTED, false, |ui| {
            ui.horizontal(|ui| {
                ui.text_edit_singleline(&mut self.json_path);
                if ui.button("laden").clicked() {
                    match TestDefinition::load(&self.json_path) {
                        Ok(def) => {
                            self.def = def;
                            self.message = Some((false, "Geladen".into()));
                        }
                        Err(e) => self.message = Some((true, e)),
                    }
                }
                if ui.button("speichern").clicked() {
                    self.message = Some(match self.def.save(&self.json_path) {
                        Ok(()) => (false, "Gespeichert (ohne Passwörter)".into()),
                        Err(e) => (true, e),
                    });
                }
            });
            if let Some((is_err, text)) = &self.message {
                hint(ui, text, if *is_err { ERR } else { MUTED });
            }
            for e in self.def.validate() {
                hint(ui, &e, ERR);
            }
        });
    }

    fn dashboard(&mut self, ui: &mut Ui) {
        let Some(h) = &self.run else {
            boxed(ui, "last", C_LOAD, true, |ui| {
                hint(
                    ui,
                    "noch kein test gelaufen · konfigurieren, dann ▶ start",
                    MUTED,
                );
            });
            return;
        };
        let sh = &h.shared;
        let runtime = sh.runtime();
        let secs = runtime.as_secs_f64().max(0.001);
        let finished = sh.status() == Status::Finished;
        let active = sh.active.load(Ordering::Relaxed);
        let started = sh.started.load(Ordering::Relaxed);
        let ms = |d: Duration| d.as_secs_f64() * 1000.0;
        // The time axis spans the planned duration, so graphs fill up as the test runs.
        let span = sh.duration.as_secs() as usize;

        // Copy what we draw so the aggregator is blocked only briefly.
        let m = sh.metrics.lock().unwrap();
        let col = |f: &dyn Fn(&metrics::Point) -> f64| m.series.iter().map(f).collect::<Vec<_>>();
        let rps = col(&|p| p.requests as f64);
        let fails = col(&|p| p.failures as f64);
        let (p50s, p95s, p99s) = (
            col(&|p| ms(p.p50)),
            col(&|p| ms(p.p95)),
            col(&|p| ms(p.p99)),
        );
        let load_rows = [
            ("aktiv", format!("{active} / {}", sh.users)),
            ("gestartet", started.to_string()),
            (
                "rps",
                format!("{} (ø {:.1})", m.current_rps(), m.requests as f64 / secs),
            ),
            ("requests", m.requests.to_string()),
            (
                "iterationen",
                sh.iterations.load(Ordering::Relaxed).to_string(),
            ),
            (
                "fehlerrate",
                format!("{:.2} % ({})", m.error_rate() * 100.0, m.failures),
            ),
        ];
        let error_rate = m.error_rate() as f32;
        let lat_rows = [
            ("ø", fmt_ms(m.latency.mean())),
            ("p50", fmt_ms(m.latency.percentile(0.50))),
            ("p95", fmt_ms(m.latency.percentile(0.95))),
            ("p99", fmt_ms(m.latency.percentile(0.99))),
            ("timeouts", m.kind_count(ErrorKind::Timeout).to_string()),
            (
                "transport",
                (m.kind_count(ErrorKind::Connect) + m.kind_count(ErrorKind::Other)).to_string(),
            ),
        ];
        let statuses = m
            .status_counts
            .iter()
            .map(|(k, v)| format!("{k}:{v}"))
            .collect::<Vec<_>>()
            .join(" ");
        let dropped = m.dropped_records;
        let has_records = !m.records.is_empty();
        let errors = m.errors.iter().rev().cloned().collect::<Vec<_>>();
        drop(m);

        let w = ui.available_width();
        let box_h = ((ui.available_height() - 60.0) * 0.3).clamp(150.0, 260.0);
        let kv_w = 260.0;
        ui.allocate_ui(vec2(w, box_h), |ui| {
            boxed(ui, "last", C_LOAD, true, |ui| {
                ui.horizontal_top(|ui| {
                    ui.vertical(|ui| {
                        ui.set_width(kv_w);
                        kv_grid(ui, "load", &load_rows);
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            key(ui, "benutzer  ");
                            meter(ui, active as f32 / sh.users as f32, 140.0, |_| C_LOAD);
                        });
                        ui.horizontal(|ui| {
                            key(ui, "fehler    ");
                            meter(ui, (error_rate * 10.0).min(1.0), 140.0, heat)
                                .on_hover_text("voll bei ≥ 10 % fehlerrate");
                        });
                    });
                    let size = ui.available_size();
                    bar_graph(ui, &rps, &fails, span, size, C_LOAD, C_ERR, "/s");
                });
            });
        });
        ui.add_space(6.0);
        ui.allocate_ui(vec2(w, box_h), |ui| {
            boxed(ui, "latenz", C_LAT, true, |ui| {
                ui.horizontal_top(|ui| {
                    ui.vertical(|ui| {
                        ui.set_width(kv_w);
                        kv_grid(ui, "lat", &lat_rows);
                        ui.add(
                            egui::Label::new(RichText::new(&statuses).color(MUTED).size(12.0))
                                .truncate(),
                        )
                        .on_hover_text(format!("statuscodes: {statuses}"));
                        legend(ui, &[("p50", C_LAT), ("p95", MID), ("p99", HIGH)]);
                    });
                    let size = ui.available_size();
                    line_graph(
                        ui,
                        &[(&p50s, C_LAT), (&p95s, MID), (&p99s, HIGH)],
                        span,
                        size,
                        "ms",
                    );
                });
            })
            .on_hover_text("perzentile aus histogramm, genauigkeit ca. ±2,5 %");
        });
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            key(ui, "export");
            ui.add(egui::TextEdit::singleline(&mut self.export_path).desired_width(160.0))
                .on_hover_text(
                    "basisname: <name>-zusammenfassung.csv, <name>-requests.csv, <name>-bericht.json",
                );
            if ui
                .add_enabled(finished, egui::Button::new("zusammenfassung.csv"))
                .clicked()
            {
                let csv = metrics::summary_csv(&sh.metrics.lock().unwrap(), runtime);
                self.message = Some(write_file(
                    &format!("{}-zusammenfassung.csv", self.export_path),
                    &csv,
                ));
            }
            if ui
                .add_enabled(finished && has_records, egui::Button::new("requests.csv"))
                .on_disabled_hover_text("nur mit aktivierten request-details, nach testende")
                .clicked()
            {
                let csv = metrics::records_csv(&sh.metrics.lock().unwrap());
                self.message = Some(write_file(
                    &format!("{}-requests.csv", self.export_path),
                    &csv,
                ));
            }
            if ui
                .add_enabled(finished, egui::Button::new("bericht.json"))
                .on_hover_text("powerwizz.report/v1, z. B. für docwizz")
                .clicked()
            {
                self.message = Some(write_file(
                    &format!("{}-bericht.json", self.export_path),
                    &sh.report_json(),
                ));
            }
            if dropped > 0 {
                ui.colored_label(WARN, format!("{dropped} details verworfen"));
            }
            if let Some((is_err, text)) = &self.message {
                ui.colored_label(if *is_err { ERR } else { MUTED }, text);
            }
        });
        ui.add_space(6.0);
        boxed(
            ui,
            &format!("fehler · letzte {}", errors.len()),
            C_ERR,
            true,
            |ui| {
                if errors.is_empty() {
                    hint(ui, "keine fehler", MUTED);
                    return;
                }
                egui::ScrollArea::vertical()
                    .id_salt("errors")
                    .show(ui, |ui| {
                        egui::Grid::new("errors")
                            .num_columns(4)
                            .spacing([12.0, 1.0])
                            .striped(true)
                            .show(ui, |ui| {
                                for col in ["zeit", "schritt", "status", "meldung"] {
                                    key(ui, col);
                                }
                                ui.end_row();
                                for e in &errors {
                                    ui.label(format!("{:>7.1}s", e.at.as_secs_f64()));
                                    ui.label(&e.step);
                                    ui.colored_label(
                                        C_ERR,
                                        match (e.status, e.error) {
                                            (Some(s), _) => s.to_string(),
                                            (None, Some(k)) => k.as_str().to_string(),
                                            _ => String::new(),
                                        },
                                    );
                                    ui.add(egui::Label::new(&e.message).truncate());
                                    ui.end_row();
                                }
                            });
                    });
            },
        );
    }

    fn confirm_dialog(&mut self, ctx: &egui::Context) {
        let d = &self.def;
        let mut start = false;
        let resp = egui::Modal::new(egui::Id::new("confirm")).show(ctx, |ui| {
            ui.label(
                RichText::new("test starten?")
                    .color(C_LOAD)
                    .strong()
                    .size(15.0),
            );
            ui.add_space(4.0);
            let names: Vec<_> = d
                .scenarios
                .iter()
                .map(|s| format!("{} ({} schritte)", s.name, s.steps.len()))
                .collect();
            kv_grid(
                ui,
                "confirm_grid",
                &[
                    ("ziel", d.base_url.clone()),
                    (
                        "last",
                        format!(
                            "{} benutzer · ramp-up {} s · dauer {} s",
                            d.users, d.ramp_up_secs, d.duration_secs
                        ),
                    ),
                    (
                        "limit",
                        d.request_limit.map_or("keins".into(), |n| n.to_string()),
                    ),
                    ("szenarien", names.join(", ")),
                    (
                        "anmeldung",
                        match d.auth {
                            AuthStrategy::PerUser => {
                                format!("{} ({} accounts)", auth_text(d.auth), self.accounts.len())
                            }
                            a => auth_text(a).into(),
                        },
                    ),
                ],
            );
            ui.add_space(4.0);
            if !model::is_local_host(&d.base_url) {
                hint(
                    ui,
                    "das ziel ist kein lokaler oder privater host – evtl. ein produktivsystem",
                    HIGH,
                );
            }
            hint(
                ui,
                "nur starten, wenn eine berechtigung für diesen lasttest besteht",
                WARN,
            );
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                start = ui.button("berechtigt – starten").clicked();
                if ui.button("abbrechen").clicked() {
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
        Ok(()) => (false, format!("gespeichert: {path}")),
        Err(e) => (true, format!("{path}: {e}")),
    }
}

fn fmt_ms(d: Duration) -> String {
    format!("{:.1} ms", d.as_secs_f64() * 1000.0)
}

// ---------- widgets (after reswizz) ----------

fn key(ui: &mut Ui, text: &str) -> egui::Response {
    ui.label(RichText::new(text).color(MUTED).size(12.0))
}

fn hint(ui: &mut Ui, text: &str, color: Color32) {
    ui.add(egui::Label::new(RichText::new(text).color(color).size(12.0)).wrap());
}

fn kv_grid(ui: &mut Ui, id: &str, rows: &[(&str, String)]) {
    egui::Grid::new(id)
        .num_columns(2)
        .spacing(vec2(10.0, 1.0))
        .show(ui, |ui| {
            for (k, v) in rows {
                key(ui, k);
                ui.add(egui::Label::new(RichText::new(v).size(12.0)).truncate())
                    .on_hover_text(v);
                ui.end_row();
            }
        });
}

fn legend(ui: &mut Ui, items: &[(&str, Color32)]) {
    ui.horizontal(|ui| {
        for (name, c) in items {
            ui.label(RichText::new(format!("━ {name}")).color(*c).size(12.0));
        }
    });
}

/// Rounded border with the title cut into the top edge. `fill` takes all
/// remaining space, otherwise the box is as tall as its content.
fn boxed<R>(
    ui: &mut Ui,
    title: &str,
    color: Color32,
    fill: bool,
    add: impl FnOnce(&mut Ui) -> R,
) -> egui::Response {
    ui.add_space(8.0);
    let r = egui::Frame::NONE
        .stroke(Stroke::new(1.0, color.gamma_multiply(0.7)))
        .corner_radius(6)
        .inner_margin(Margin {
            left: 8,
            right: 8,
            top: 10,
            bottom: 6,
        })
        .show(ui, |ui| {
            if fill {
                ui.set_min_size(ui.available_size());
            } else {
                ui.set_min_width(ui.available_width());
            }
            add(ui)
        });
    let font = FontId::monospace(12.0);
    let galley = ui
        .painter()
        .layout_no_wrap(format!(" {title} "), font, color);
    let pos = r.response.rect.left_top() + vec2(12.0, -galley.size().y / 2.0);
    ui.painter()
        .rect_filled(Rect::from_min_size(pos, galley.size()), 0.0, BG);
    ui.painter().galley(pos, galley, color);
    r.response
}

/// Segmented bar; `color` maps segment position 0..1 to its colour.
fn meter(ui: &mut Ui, frac: f32, width: f32, color: impl Fn(f32) -> Color32) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(width, 9.0), Sense::hover());
    let (seg, gap) = (4.0, 1.0);
    let n = ((width + gap) / (seg + gap)).floor().max(1.0) as usize;
    let lit = (frac.clamp(0.0, 1.0) * n as f32).round() as usize;
    for i in 0..n {
        let x = rect.left() + i as f32 * (seg + gap);
        let c = if i < lit {
            color(i as f32 / n as f32)
        } else {
            DIM
        };
        ui.painter().rect_filled(
            Rect::from_min_size(Pos2::new(x, rect.top()), vec2(seg, rect.height())),
            1.0,
            c,
        );
    }
    resp
}

/// Fits `slots` seconds (planned duration or more) into at most `n` columns:
/// returns the bucket peaks of `v` and the number of columns the axis spans.
fn downsample(v: &[f64], slots: usize, n: usize) -> (Vec<f64>, usize) {
    let chunk = slots.max(v.len()).div_ceil(n.max(1)).max(1);
    let peaks = v
        .chunks(chunk)
        .map(|c| c.iter().copied().fold(0.0, f64::max))
        .collect();
    (peaks, slots.max(v.len()).div_ceil(chunk).max(1))
}

fn graph_area(ui: &mut Ui, size: Vec2) -> (egui::Painter, Rect) {
    let (rect, _) = ui.allocate_exact_size(size.max(vec2(40.0, 40.0)), Sense::hover());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 3.0, WELL);
    (p, rect.shrink2(vec2(2.0, 3.0)))
}

fn scale_label(p: &egui::Painter, rect: Rect, max: f64, unit: &str, secs: usize) {
    p.text(
        rect.left_top() + vec2(4.0, 0.0),
        Align2::LEFT_TOP,
        format!("{max:.0} {unit}"),
        FontId::monospace(10.0),
        MUTED,
    );
    p.text(
        rect.right_bottom() - vec2(4.0, 0.0),
        Align2::RIGHT_BOTTOM,
        format!("{secs} s"),
        FontId::monospace(10.0),
        MUTED,
    );
}

/// Bars for `v`, with `over` drawn on top from the same baseline (e.g. failures within requests).
#[allow(clippy::too_many_arguments)]
fn bar_graph(
    ui: &mut Ui,
    v: &[f64],
    over: &[f64],
    span: usize,
    size: Vec2,
    c: Color32,
    co: Color32,
    unit: &str,
) {
    let (p, rect) = graph_area(ui, size);
    let n = (rect.width() / 3.0) as usize;
    let ((v2, cols), (o2, _)) = (downsample(v, span, n), downsample(over, span, n));
    let max = v2.iter().copied().fold(1.0, f64::max);
    let w = rect.width() / cols as f32;
    let gap = if w > 3.0 { 1.0 } else { 0.0 };
    for (series, color) in [(&v2, c), (&o2, co)] {
        for (i, &y) in series.iter().enumerate() {
            let t = (y / max).clamp(0.0, 1.0) as f32;
            if t <= 0.0 {
                continue;
            }
            let x = rect.left() + i as f32 * w;
            let top = rect.bottom() - (t * rect.height()).max(1.0);
            p.rect_filled(
                Rect::from_min_max(Pos2::new(x, top), Pos2::new(x + w - gap, rect.bottom())),
                0.0,
                color,
            );
        }
    }
    scale_label(&p, rect, max, unit, v.len());
}

/// Lines on a shared scale, oldest left.
fn line_graph(ui: &mut Ui, lines: &[(&[f64], Color32)], span: usize, size: Vec2, unit: &str) {
    let (p, rect) = graph_area(ui, size);
    let n = (rect.width() / 2.0) as usize;
    let secs = lines.first().map_or(0, |(v, _)| v.len());
    let mut cols = 1;
    let lines: Vec<_> = lines
        .iter()
        .map(|(v, c)| {
            let (d, k) = downsample(v, span, n);
            cols = k;
            (d, *c)
        })
        .collect();
    let max = lines
        .iter()
        .flat_map(|(v, _)| v.iter().copied())
        .fold(1.0, f64::max);
    for (v, c) in &lines {
        if v.len() < 2 {
            continue;
        }
        let dx = rect.width() / (cols - 1).max(1) as f32;
        let pts = v
            .iter()
            .enumerate()
            .map(|(i, &y)| {
                let t = (y / max).clamp(0.0, 1.0) as f32;
                Pos2::new(
                    rect.left() + i as f32 * dx,
                    rect.bottom() - t * rect.height(),
                )
            })
            .collect();
        p.add(egui::Shape::line(pts, Stroke::new(1.5, *c)));
    }
    scale_label(&p, rect, max, unit, secs);
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

fn steps_editor(ui: &mut Ui, salt: &str, steps: &mut Vec<Step>) {
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
                        ui.add(egui::TextEdit::singleline(&mut s.url).hint_text("/pfad oder url"));
                    });
                    ui.horizontal(|ui| {
                        key(ui, "pause ms");
                        ui.add(egui::DragValue::new(&mut s.pause_min_ms).range(0..=s.pause_max_ms));
                        key(ui, "–");
                        ui.add(
                            egui::DragValue::new(&mut s.pause_max_ms)
                                .range(s.pause_min_ms..=600_000),
                        );
                        key(ui, "status");
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
                    egui::CollapsingHeader::new(
                        RichText::new(if s.extract.is_empty() {
                            "header / body / extrahieren".to_string()
                        } else {
                            format!("header / body / extrahieren ({})", s.extract.len())
                        })
                        .color(MUTED)
                        .size(12.0),
                    )
                    .show(ui, |ui| {
                        header_lines(ui, "headers", &mut s.headers);
                        ui.add(
                            egui::TextEdit::multiline(&mut s.body)
                                .code_editor()
                                .desired_rows(2)
                                .hint_text("Body (Text/JSON)"),
                        );
                        ui.horizontal(|ui| {
                            key(ui, "extrahieren").on_hover_text(
                                "Werte aus der JSON-Antwort, je Zeile name = /json/pointer; \
                                 danach als {{name}} nutzbar, z. B. token = /access_token",
                            );
                            pair_lines(ui, "extract", &mut s.extract, '=', "token = /access_token");
                        });
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
    if ui.small_button("+ schritt").clicked() {
        steps.push(Step::default());
    }
}

/// Text field that edits `value` through a text representation. The text is kept
/// across frames so half-typed input ("200,") survives; `value` changes only when
/// the text parses.
fn buffered<T>(
    ui: &mut Ui,
    salt: &str,
    value: &mut T,
    // Some(hint) = multi-line field with that placeholder text.
    multiline: Option<&str>,
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
    let edit = if let Some(hint) = multiline {
        egui::TextEdit::multiline(&mut st.0)
            .desired_rows(1)
            .hint_text(hint)
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

fn status_list(ui: &mut Ui, codes: &mut Vec<u16>) {
    buffered(
        ui,
        "status",
        codes,
        None,
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

fn header_lines(ui: &mut Ui, salt: &str, headers: &mut Vec<(String, String)>) {
    pair_lines(ui, salt, headers, ':', "Name: Wert");
}

/// One `key<sep> value` pair per line.
fn pair_lines(ui: &mut Ui, salt: &str, pairs: &mut Vec<(String, String)>, sep: char, hint: &str) {
    buffered(
        ui,
        salt,
        pairs,
        Some(hint),
        |h| {
            h.iter()
                .map(|(k, v)| format!("{k}{sep} {v}"))
                .collect::<Vec<_>>()
                .join("\n")
        },
        |t| {
            t.lines()
                .filter(|l| !l.trim().is_empty())
                .map(|l| {
                    l.split_once(sep)
                        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
                })
                .collect()
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downsample_keeps_peaks() {
        let v: Vec<f64> = (0..10).map(f64::from).collect();
        assert_eq!(downsample(&v, 0, 20), (v.clone(), 10));
        assert_eq!(downsample(&v, 0, 5), (vec![1.0, 3.0, 5.0, 7.0, 9.0], 5));
        assert_eq!(downsample(&v, 0, 3), (vec![3.0, 7.0, 9.0], 3));
        // 10 s into a 60 s test on 20 columns: 3 s per column, axis spans 20 columns.
        assert_eq!(downsample(&v, 60, 20), (vec![2.0, 5.0, 8.0, 9.0], 20));
        assert_eq!(downsample(&[], 60, 600), (vec![], 60));
    }

    #[test]
    fn heat_spans_green_to_red() {
        assert_eq!(heat(0.0), LOW);
        assert_eq!(heat(1.0), HIGH);
    }
}
