#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod capture;
mod config;
mod disk_space;
mod encode;
mod hotkeys;
mod i18n;
mod error;
mod session;
mod sources;
mod types;
mod ui;
mod update_check;

fn main() -> eframe::Result<()> {
    tracing_subscriber::fmt::init();

    // Build a multi-thread runtime so tokio::task::spawn_blocking and mpsc channels
    // work from the egui main thread (eframe is not async, so we enter the runtime
    // without blocking it).
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    let _rt_guard = rt.enter();

    unsafe {
        use windows::Win32::Media::MediaFoundation::{MFStartup, MF_VERSION, MFSTARTUP_FULL};
        MFStartup(MF_VERSION, MFSTARTUP_FULL)
            .expect("MFStartup failed — requires Windows 7+");
    }

    let config = config::Config::load().unwrap_or_else(|e| {
        // load() only errors when config.toml exists but fails to parse (a
        // missing file is Ok(default) already) -- falling back silently would
        // leave a user wondering why their settings reset with no clue why.
        tracing::warn!("failed to load config.toml, using defaults: {e}");
        config::Config::default()
    });
    let icon = eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon-1024.png"))
        .expect("failed to decode app icon");
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("PolyRec")
            // 900px left a large empty gap on the right of the center panel —
            // actual content (Quality/Hotkeys buttons, output-dir row, REC
            // button) tops out around 420-450px; 760 keeps the resizable
            // 200-380px source-list panel comfortable without the excess.
            .with_inner_size([760.0, 600.0])
            .with_min_inner_size([620.0, 450.0])
            .with_icon(icon),
        ..Default::default()
    };

    let result = eframe::run_native(
        "PolyRec",
        native_options,
        Box::new(|cc| Ok(Box::new(ui::App::new(cc, config)))),
    );

    unsafe {
        use windows::Win32::Media::MediaFoundation::MFShutdown;
        let _ = MFShutdown();
    }

    result
}
