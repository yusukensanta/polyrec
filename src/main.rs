#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod config;
mod error;
mod session;
mod sources;
mod types;
mod ui;

fn main() -> eframe::Result<()> {
    tracing_subscriber::fmt::init();

    let config = config::Config::load().unwrap_or_default();

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("PolyRec")
            .with_inner_size([900.0, 600.0])
            .with_min_inner_size([700.0, 450.0]),
        ..Default::default()
    };

    eframe::run_native(
        "PolyRec",
        native_options,
        Box::new(|cc| Ok(Box::new(ui::App::new(cc, config)))),
    )
}
