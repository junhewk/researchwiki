//! Fetch-only health check for one gather source. Lists candidates, then tries
//! to fetch article content/PDFs without screening, evaluation, saving,
//! embedding, KG extraction, or wiki compilation.
//!
//! Usage:
//!   QUERY="healthcare artificial intelligence ethics" SOURCE=arxiv DAYS_BACK=30 \
//!     PDF_DIR=/tmp/researchwiki-fetch-check cargo run --bin check_fetch

use anyhow::Result;
use researchwiki::{
    config::AppConfig,
    models::workspace::WorkspaceResearchContext,
    services::{
        fetcher::{ContentData, ContentFetcher},
        pipeline::{PipelineService, source_label},
    },
};

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();

    let config = AppConfig::from_env()?;
    let query = std::env::var("QUERY")
        .unwrap_or_else(|_| "healthcare artificial intelligence ethics".to_string());
    let source = std::env::var("SOURCE").unwrap_or_else(|_| "arxiv".to_string());
    let days_back = std::env::var("DAYS_BACK")
        .ok()
        .and_then(|value| value.parse::<i32>().ok())
        .unwrap_or(30)
        .clamp(1, 3650);
    let fetch_limit = std::env::var("FETCH_LIMIT")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(1)
        .max(1);

    let context = WorkspaceResearchContext {
        name: "Fetch check".to_string(),
        primary_question: query.clone(),
        seed_concepts: vec![query.clone()],
        topic_descriptor: query.clone(),
        lookback_days: days_back,
        ..Default::default()
    };

    let pipeline = PipelineService::new(
        config.storage.database_path.clone(),
        config.contact_email_opt(),
        config.semantic_scholar_api_key_opt(),
    );
    let client = reqwest::Client::builder()
        .user_agent(concat!("researchwiki/", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(60))
        .build()?;
    let fetcher = ContentFetcher::new(client, config.contact_email_opt(), config.storage.pdf_dir);

    println!(
        "source={}  query={query:?}  days_back={days_back}  fetch_limit={fetch_limit}",
        source_label(&source).unwrap_or(&source)
    );
    println!("{:-<78}", "");

    let candidates = pipeline.list_source(&source, days_back, &context).await?;
    println!("listed {} candidates", candidates.len());

    let mut fetched = 0usize;
    for candidate in candidates.iter().take(fetch_limit) {
        println!(
            "candidate: {} | {}",
            candidate.uid(),
            truncate(&candidate.title, 90)
        );
        match fetcher.fetch(candidate).await {
            Some(content) => {
                fetched += 1;
                let text_chars = match &content.content {
                    ContentData::Text(text) => text.chars().count(),
                    ContentData::Binary(bytes) => bytes.len(),
                };
                println!(
                    "  fetched: type={} method={} chars_or_bytes={} pdf_bytes={} pdf_path={} status={} error={}",
                    content.content_type.as_str(),
                    content.fetch_method,
                    text_chars,
                    content
                        .pdf_bytes
                        .map(|bytes| bytes.to_string())
                        .unwrap_or_default(),
                    content
                        .pdf_path
                        .as_ref()
                        .map(|path| path.display().to_string())
                        .unwrap_or_default(),
                    content.text_extraction_status.unwrap_or_default(),
                    content.text_extraction_error.unwrap_or_default()
                );
            }
            None => println!("  fetch failed"),
        }
    }

    println!("{:-<78}", "");
    println!("fetched {fetched} / {}", candidates.len().min(fetch_limit));
    Ok(())
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        format!("{}...", text.chars().take(max).collect::<String>())
    }
}
