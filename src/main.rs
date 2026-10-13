#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;

fn main() -> eframe::Result {
    // Tokio worker threads run the load engine; the GUI thread only renders.
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let _guard = rt.enter();
    eframe::run_native(
        "powerwizz",
        eframe::NativeOptions::default(),
        Box::new(|cc| {
            cc.egui_ctx.set_theme(eframe::egui::Theme::Dark);
            Ok(Box::new(app::App::default()))
        }),
    )
}
