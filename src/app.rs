use eframe::egui;

#[derive(Default)]
pub struct App {}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ui, |ui| {
            ui.heading("powerwizz");
            ui.label("Only run load tests against systems you are authorized to test.");
        });
    }
}
