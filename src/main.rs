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
