#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod capture;
mod config;
mod disk_space;
mod encode;
mod highlight;
mod hotkeys;
mod i18n;
mod error;
mod self_update;
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
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("PolyRec")
        // 900px left a large empty gap on the right of the center panel —
        // actual content (Quality/Hotkeys buttons, output-dir row, REC
        // button) tops out around 420-450px; 760 keeps the resizable
        // 200-380px source-list panel comfortable without the excess.
        //
        // Height bumped 600 -> 680: each checked audio device's volume
        // slider adds a row to the left panel's AUDIO section, and that
        // section's own internal scroll area reserves its full capped
        // height once content exceeds it (rather than shrinking to fit) —
        // with just the default-checked device, this already pushed
        // "App audio only" a few pixels below the window's bottom edge at
        // 600, with no scrollbar reaching it. Measured empirically (UI
        // Automation bounding rects: at 600, "App audio only"'s bottom
        // sat 9px past the window's bottom edge with only the
        // default-checked device; at 680, it clears the edge by 43px
        // even with both Speakers and Microphone checked).
        .with_inner_size([760.0, 680.0])
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
