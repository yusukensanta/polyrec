#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod capture;
mod config;
mod crash_log;
mod disk_space;
mod encode;
mod error;
mod highlight;
mod hotkeys;
mod i18n;
mod recording_naming;
mod restart;
mod self_update;
mod session;
mod single_instance;
mod sources;
mod types;
mod ui;
mod update_check;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// How long PolyRec waits at exit for recordings still being saved -- see the
/// end of `main`.
const SAVE_ON_EXIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

fn main() -> eframe::Result<()> {
    tracing_subscriber::fmt::init();
    crash_log::install(ui::is_caught_on_purpose);
    let started = std::time::Instant::now();
    let restarted = restart::Reason::of_this_start(std::env::args());

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
    let rt_guard = rt.enter();

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

    // Set by the dashboard when it closes because its graphics device was
    // lost -- see `ui::dashboard::graphics_device`.
    let closed_for_lost_graphics = Arc::new(AtomicBool::new(false));
    // A panic on this thread ends up here (winit catches it in its event
    // handler and raises it again once the event loop has stopped), with the
    // dashboard dropped on the way: a recording that was running is saved
    // below, as on any exit.
    let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
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
                Ok(Box::new(ui::App::new(
                    cc,
                    config,
                    show_requested,
                    Arc::clone(&closed_for_lost_graphics),
                    restarted,
                )))
            }),
        )
    }));

    // The window is gone: a launch from here on runs as the copy that's up,
    // instead of asking this one -- which may still be finishing a save
    // below -- to show a window it no longer has.
    instance.release();

    // A recording still being saved -- closing mid-recording, or right after
    // Stop -- finishes here, on the runtime's blocking threads. Dropping the
    // runtime would wait for that however long it took, so a save that hung
    // left PolyRec running invisibly for good. A real save takes well under
    // a second.
    drop(rt_guard);
    rt.shutdown_timeout(SAVE_ON_EXIT_TIMEOUT);

    let (result, closed_on_its_own) = match run {
        Ok(result) => (
            result,
            closed_for_lost_graphics
                .load(Ordering::Relaxed)
                .then_some(restart::Reason::Graphics),
        ),
        // A panic raised in the graphics code (egui's renderer or wgpu) is
        // almost certainly the device having gone: on a lost device, egui's
        // renderer panics uploading the next frame, often before wgpu has
        // said the device was lost.
        Err(_) => {
            let in_graphics =
                crash_log::last_main_thread_panic_location().is_some_and(|at| at.contains("wgpu"));
            let reason = if in_graphics {
                restart::Reason::Graphics
            } else {
                restart::Reason::Error
            };
            (Ok(()), Some(reason))
        }
    };
    if let Some(reason) = closed_on_its_own {
        if restart::should_restart(restarted.is_some(), started.elapsed()) {
            restart::start_new_copy(reason);
        } else {
            tracing::error!(
                "closed on its own ({reason:?}) again right after a restart; not restarting"
            );
        }
    }
    result
}
