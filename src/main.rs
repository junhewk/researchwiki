use researchwiki::{
    app::{DesktopApp, bootstrap_db, first_launch_seed},
    config::AppConfig,
    init_tracing, register_sqlite_vec,
    runtime::DesktopRuntime,
    services::settings::load_overrides_sync,
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
