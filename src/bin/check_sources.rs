//! Connectivity/health check for every gather source. Lists (does not save)
//! candidates from each source for a generic query and reports counts, so you
//! can see which routes return articles.
//!
//! Usage:
//!   QUERY="diabetes" DAYS_BACK=365 RESEARCHWIKI_CONTACT_EMAIL=you@example.com \
//!     cargo run --bin check_sources

use anyhow::Result;
use researchwiki::{
    models::workspace::WorkspaceResearchContext,
    services::pipeline::{GATHER_SOURCE_IDS, PipelineService, source_label},
};

#[tokio::main]
async fn main() -> Result<()> {
    let query = std::env::var("QUERY").unwrap_or_else(|_| "diabetes".to_string());
    let days_back: i32 = std::env::var("DAYS_BACK")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(365);
    let contact_email = std::env::var("RESEARCHWIKI_CONTACT_EMAIL")
        .or_else(|_| std::env::var("UNPAYWALL_EMAIL"))
        .ok();
    let semantic_scholar_api_key = std::env::var("SEMANTIC_SCHOLAR_API_KEY").ok();

    let context = WorkspaceResearchContext {
        name: "Source check".to_string(),
        primary_question: query.clone(),
        seed_concepts: vec![query.clone()],
        topic_descriptor: query.clone(),
        lookback_days: days_back,
        ..Default::default()
    };

    let pipeline = PipelineService::new(
        std::env::temp_dir().join("researchwiki-source-check.db"),
        contact_email.clone(),
        semantic_scholar_api_key,
    );

    println!(
        "query={query:?}  days_back={days_back}  contact_email={}",
        contact_email
            .as_deref()
            .unwrap_or("(none → Unpaywall skipped)")
    );
    println!("{:-<78}", "");

    let (mut ok, mut empty, mut failed) = (0u32, 0u32, 0u32);
    for &source in GATHER_SOURCE_IDS {
        let label = source_label(source).unwrap_or(source);
        match pipeline.list_source(source, days_back, &context).await {
            Ok(candidates) => {
                let n = candidates.len();
                if n == 0 {
                    empty += 1;
                } else {
                    ok += 1;
                }
                let sample = candidates.first().map(|c| c.title.as_str()).unwrap_or("");
                println!("{label:<18} {n:>4}  {}", truncate(sample, 54));
            }
            Err(error) => {
                failed += 1;
                println!("{label:<18}  ERR  {error}");
            }
        }
    }

    println!("{:-<78}", "");
    println!("with results: {ok}   empty: {empty}   failed: {failed}");
    Ok(())
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        format!("{}…", text.chars().take(max).collect::<String>())
    }
}
