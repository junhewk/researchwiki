use std::net::SocketAddr;

use anyhow::Result;
use researchwiki::{
    app::{bootstrap_db, first_launch_seed},
    config::AppConfig,
    init_tracing, register_sqlite_vec,
    services::settings::load_overrides_sync,
    web,
};
use tokio::net::TcpListener;
use tracing::info;

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    init_tracing();
    register_sqlite_vec();

    let mut config = AppConfig::from_env()?;
    first_launch_seed(&config)?;
    apply_startup_overrides(&mut config);
    bootstrap_db(&config).await?;

    let state = web::WebState::new(config);
    let (scheduler_shutdown_tx, scheduler_shutdown_rx) = tokio::sync::watch::channel(false);
    let scheduler_handle = state.spawn_scheduler(scheduler_shutdown_rx);
    let router = web::router(state);
    let addr = std::env::var("RESEARCHWIKI_WEB_ADDR")
        .ok()
        .and_then(|value| value.parse::<SocketAddr>().ok())
        .unwrap_or_else(|| SocketAddr::from(([127, 0, 0, 1], 8787)));
    let listener = TcpListener::bind(addr).await?;
    info!("ResearchWiki web UI listening on http://{addr}");

    axum::serve(listener, router)
        .with_graceful_shutdown(async move {
            web::shutdown_signal().await;
            let _ = scheduler_shutdown_tx.send(true);
            let _ = scheduler_handle.await;
        })
        .await?;

    Ok(())
}

fn apply_startup_overrides(config: &mut AppConfig) {
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
    if let Some(key) = overrides.semantic_scholar_api_key {
        config.semantic_scholar_api_key = key;
    }
}
