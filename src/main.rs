#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod capture;
mod config;
mod disk_space;
mod encode;
mod error;
mod highlight;
mod hotkeys;
mod i18n;
mod recording_naming;
mod self_update;
mod session;
mod single_instance;
mod sources;
mod types;
mod ui;
mod update_check;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

fn main() -> eframe::Result<()> {
    tracing_subscriber::fmt::init();

    // Before anything registers a hotkey or a tray icon.
    let after_update = std::env::args().any(|arg| arg == single_instance::AFTER_UPDATE_ARG);
    let Some(instance) = single_instance::claim(after_update) else {
        tracing::info!("PolyRec is already running; brought that one up instead");
        return Ok(());
    };

    // Build a multi-thread runtime so tokio::task::spawn_blocking and mpsc channels
    // work from the egui main thread (eframe is not async, so we enter the runtime
    // without blocking it).
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    let _rt_guard = rt.enter();

    // Held for the whole run so Media Foundation stays started between the
    // per-recording/per-export sessions the rest of the app opens and closes.
    let _mf = encode::mf::MfSession::start().expect("MFStartup failed — requires Windows 7+");

    let config = config::Config::load().unwrap_or_else(|e| {
        // load() only errors when config.toml exists but fails to parse (a
        // missing file is Ok(default) already) -- falling back silently would
        // leave a user wondering why their settings reset with no clue why.
        tracing::warn!("failed to load config.toml, using defaults: {e}");
        config::Config::default()
    });
    let icon = eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon-1024.png"))
        .expect("failed to decode app icon");
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("PolyRec")
        // 900px left a large empty gap on the right of the center panel —
        // actual content (Quality/Hotkeys buttons, output-dir row, REC
        // button) tops out around 420-450px; 760 keeps the resizable
        // 200-380px source-list panel comfortable without the excess.
        //
        // 740: the left panel's sections (source list, SYSTEM, APPLICATIONS)
        // each scroll independently with a fixed max_height (see
        // render_source_panel), so the panel's total content height no
        // longer varies with how many devices/apps exist or are checked --
        // unlike the per-row height used to before every checkbox+slider
        // was made a single fixed-height row. 740 was measured empirically
        // (via UI Automation bounding rects) to give "App audio only" a
        // comfortable margin below the window's bottom edge at that fixed
        // total.
        .with_inner_size([760.0, 740.0])
        .with_min_inner_size([620.0, 450.0])
        .with_icon(icon);
    // Reopen where the window was left (including across a self-update's
    // relaunch, since config.toml lives in %APPDATA%, untouched by the exe
    // swap) instead of the OS's default placement -- see
    // Config::sane_window_position for why an out-of-range saved value is
    // ignored rather than trusted outright.
    if let Some((x, y)) = config.sane_window_position() {
        viewport = viewport.with_position([x, y]);
    }
    let native_options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    eframe::run_native(
        "PolyRec",
        native_options,
        Box::new(|cc| {
            let show_requested = Arc::new(AtomicBool::new(false));
            let ctx = cc.egui_ctx.clone();
            let flag = Arc::clone(&show_requested);
            instance.listen(move || {
                flag.store(true, Ordering::Relaxed);
                ctx.request_repaint();
            });
            Ok(Box::new(ui::App::new(cc, config, show_requested)))
        }),
    )
}
