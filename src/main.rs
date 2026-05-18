use researchwiki::{
    app::{DesktopApp, bootstrap_db, first_launch_seed},
    config::AppConfig,
    init_tracing, register_sqlite_vec,
    runtime::DesktopRuntime,
    services::settings::load_overrides_sync,
};

fn main() -> eframe::Result<()> {
    // Load .env if present (development convenience; ignored in production builds).
    dotenvy::dotenv().ok();

    // Tracing first so subsequent setup errors are visible.
    init_tracing();

    // Process-global sqlite-vec auto-extension registration. Must happen before
    // any Connection::open in any thread — including the tokio worker pool.
    register_sqlite_vec();

    let runtime = DesktopRuntime::new().expect("tokio runtime should build");

    let mut config = AppConfig::from_env().expect("AppConfig should resolve");

    // Synchronous bootstrap: seed directories and copy bundled prompts, then
    // run async DB initialization on the runtime so vec0/FTS tables exist
    // before the first frame.
    first_launch_seed(&config).expect("first-launch directory setup failed");

    // Overlay persisted LLM endpoint + embedding dimensions from settings.json,
    // so the first-run modal only appears on a truly fresh install.
    let (persisted_llm, persisted_dim) = load_overrides_sync(&config.storage.settings_file);
    if let Some(llm) = persisted_llm {
        config.llm = llm;
    }
    if let Some(dim) = persisted_dim {
        config.embedding_dimensions = dim;
    }

    runtime
        .handle
        .block_on(bootstrap_db(&config))
        .expect("database initialization failed");

    let native_options = eframe::NativeOptions {
        persist_window: true,
        viewport: egui::ViewportBuilder::default()
            .with_title("ResearchWiki")
            .with_inner_size([1100.0, 720.0])
            .with_min_inner_size([720.0, 480.0]),
        ..Default::default()
    };

    eframe::run_native(
        "ResearchWiki",
        native_options,
        Box::new(move |cc| Ok(Box::new(DesktopApp::new(cc, runtime, config)))),
    )
}
