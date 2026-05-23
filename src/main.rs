// Hide the Windows console window in release builds (keep it in debug for logs).
// On macOS the terminal only appears when running the bare binary; a bundled
// .app (see `cargo bundle`) launches with no terminal.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use researchwiki::{
    app::{DesktopApp, bootstrap_db, first_launch_seed},
    config::AppConfig,
    init_tracing, register_sqlite_vec,
    runtime::DesktopRuntime,
    services::settings::{load_overrides_sync, load_setup_complete_sync, load_ui_language_sync},
};

fn main() -> eframe::Result<()> {
    dotenvy::dotenv().ok();
    init_tracing();
    // sqlite-vec must register before any Connection::open in any thread.
    register_sqlite_vec();

    let runtime = DesktopRuntime::new().expect("tokio runtime should build");
    let mut config = AppConfig::from_env().expect("AppConfig should resolve");

    first_launch_seed(&config).expect("first-launch directory setup failed");

    // Overlay persisted settings so the first-run modal only fires on a fresh install.
    let overrides = load_overrides_sync(&config.storage.settings_file);
    if let Some(llm) = overrides.llm {
        config.llm = llm;
    }
    if let Some(embedding) = overrides.embedding {
        config.embedding = embedding;
    }
    if let Some(dim) = overrides.embedding_dimensions {
        config.embedding_dimensions = dim;
    }
    if let Some(email) = overrides.contact_email {
        config.contact_email = email;
    }
    let language = load_ui_language_sync(&config.storage.settings_file);
    // Unknown (legacy) installs that are already configured count as set up, so
    // only genuinely fresh installs see the research-setup step.
    let setup_complete = load_setup_complete_sync(&config.storage.settings_file)
        .unwrap_or_else(|| config.is_ready());

    runtime
        .handle
        .block_on(bootstrap_db(&config))
        .expect("database initialization failed");

    let mut viewport = egui::ViewportBuilder::default()
        .with_title("ResearchWiki")
        .with_inner_size([1100.0, 720.0])
        .with_min_inner_size([720.0, 480.0]);
    if let Some(icon) = load_window_icon() {
        viewport = viewport.with_icon(icon);
    }

    let native_options = eframe::NativeOptions {
        persist_window: true,
        viewport,
        ..Default::default()
    };

    eframe::run_native(
        "ResearchWiki",
        native_options,
        Box::new(move |cc| {
            Ok(Box::new(DesktopApp::new(
                cc,
                runtime,
                config,
                language,
                setup_complete,
            )))
        }),
    )
}

/// Decodes the embedded app icon into an eframe `IconData` for the window /
/// taskbar. Returns `None` if the asset can't be decoded (the app still runs).
fn load_window_icon() -> Option<egui::IconData> {
    let image = image::load_from_memory(include_bytes!("../assets/app-icon.png"))
        .ok()?
        .to_rgba8();
    let (width, height) = image.dimensions();
    Some(egui::IconData {
        rgba: image.into_raw(),
        width,
        height,
    })
}
