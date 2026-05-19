//! Live eval CLI for ResearchWiki.
//!
//! Verifies that
//! 1. the configured LLM endpoint is reachable and answers chat/completions,
//! 2. the OpenAI embeddings endpoint is reachable (if a key is configured),
//! 3. the SQLite DB has the expected schema (article + chunk + vec + FTS),
//! 4. keyword (FTS5) + semantic (sqlite-vec) + hybrid search wiring is intact.
//!
//! Usage:
//!   cargo run --bin eval                          # read-only checks
//!   cargo run --bin eval -- --seed                # insert a synthetic article
//!                                                 # + embeddings, run search,
//!                                                 # then clean it up
//!   cargo run --bin eval -- --seed --keep        # leave the seeded article in db
//!   cargo run --bin eval -- --query "foo bar"    # custom search query
//!
//! Reads config from env (LLM_*, OPENAI_API_KEY) and from the persisted
//! settings.json, exactly like the desktop app.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use reqwest::Client;
use researchwiki::{
    config::{AppConfig, EmbeddingConfig, LlmConfig},
    init_tracing,
    models::library::{SearchMode, SearchRequest},
    register_sqlite_vec,
    services::{
        embedding::EmbeddingService, library::LibraryService, llm::LlmService,
        prompts::PromptService, settings::load_overrides_sync, traces::TraceService,
    },
};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use zerocopy::IntoBytes;

#[derive(Default)]
struct Summary {
    passed: u32,
    failed: u32,
    skipped: u32,
}

impl Summary {
    fn ok(&mut self, label: &str, detail: impl AsRef<str>) {
        self.passed += 1;
        println!("  [OK]   {label}: {}", detail.as_ref());
    }
    fn fail(&mut self, label: &str, detail: impl AsRef<str>) {
        self.failed += 1;
        println!("  [FAIL] {label}: {}", detail.as_ref());
    }
    fn skip(&mut self, label: &str, reason: impl AsRef<str>) {
        self.skipped += 1;
        println!("  [SKIP] {label}: {}", reason.as_ref());
    }
    fn print(&self) {
        println!();
        println!("──────────────────────────────────────────────");
        println!(
            "Summary: {} passed, {} failed, {} skipped",
            self.passed, self.failed, self.skipped
        );
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    init_tracing();
    register_sqlite_vec();

    let args: Vec<String> = std::env::args().skip(1).collect();
    let seed = args.iter().any(|a| a == "--seed");
    let keep = args.iter().any(|a| a == "--keep");
    let query = args
        .iter()
        .position(|a| a == "--query")
        .and_then(|i| args.get(i + 1))
        .cloned()
        .unwrap_or_else(|| "neural networks".to_string());

    let mut config = AppConfig::from_env().context("AppConfig")?;
    let (persisted_llm, persisted_embedding, persisted_dim) =
        load_overrides_sync(&config.storage.settings_file);
    if let Some(llm) = persisted_llm {
        config.llm = llm;
    }
    if let Some(embedding) = persisted_embedding {
        config.embedding = embedding;
    }
    if let Some(dim) = persisted_dim {
        config.embedding_dimensions = dim;
    }

    // Make sure the DB exists with the current schema. bootstrap_db creates
    // the file if missing and applies migrations idempotently.
    researchwiki::app::bootstrap_db(&config)
        .await
        .context("db bootstrap")?;

    let mut s = Summary::default();
    println!("==============================================");
    println!(" ResearchWiki — live eval");
    println!("==============================================");

    println!("\n[config]");
    println!("  db          : {}", config.storage.database_path.display());
    println!("  settings    : {}", config.storage.settings_file.display());
    println!(
        "  llm base    : {}",
        if config.llm.base_url.is_empty() {
            "<unset>"
        } else {
            &config.llm.base_url
        }
    );
    println!(
        "  llm model   : {}",
        if config.llm.model.is_empty() {
            "<unset>"
        } else {
            &config.llm.model
        }
    );
    println!(
        "  llm key     : {}",
        if config.llm.api_key.is_empty() {
            "<missing>"
        } else {
            "present"
        }
    );
    println!(
        "  embed base  : {}",
        if config.embedding.base_url.is_empty() {
            "<unset>"
        } else {
            &config.embedding.base_url
        }
    );
    println!(
        "  embed model : {}",
        if config.embedding.model.is_empty() {
            "<unset>"
        } else {
            &config.embedding.model
        }
    );
    println!(
        "  embed key   : {}",
        if config.embedding.api_key.is_empty() {
            "<missing>"
        } else {
            "present"
        }
    );
    println!("  embed dims  : {}", config.embedding_dimensions);

    let embed_ready = config.embedding.is_configured();

    println!("\n[llm]");
    if config.llm.is_configured() {
        match probe_llm(&config.llm).await {
            Ok(detail) => s.ok("chat/completions", detail),
            Err(err) => s.fail("chat/completions", format!("{err:#}")),
        }
    } else {
        s.skip(
            "chat/completions",
            "LLM_BASE_URL / LLM_MODEL not configured (env or settings.json)",
        );
    }

    println!("\n[embedding]");
    if embed_ready {
        match probe_embedding(&config.embedding).await {
            Ok(detail) => s.ok("/embeddings", detail),
            Err(err) => s.fail("/embeddings", format!("{err:#}")),
        }
    } else {
        s.skip(
            "/embeddings",
            "embedding base_url/model not configured (Settings → Embedding endpoint)",
        );
    }

    println!("\n[db]");
    let http_client = Client::builder()
        .user_agent("researchwiki-eval/0.1")
        .timeout(Duration::from_secs(30))
        .build()?;
    let embedding_service = Arc::new(EmbeddingService::new(
        http_client.clone(),
        config.embedding.clone(),
    ));
    let prompt_service = Arc::new(PromptService::new(
        config.storage.prompts_dir.clone(),
        config.storage.database_path.clone(),
    ));
    let trace_service = Arc::new(TraceService::new(config.storage.database_path.clone()));
    let llm_service = Arc::new(LlmService::new(
        prompt_service.clone(),
        trace_service.clone(),
        config.llm.clone(),
    ));
    let library_service = LibraryService::new(
        config.storage.database_path.clone(),
        embedding_service.clone(),
        llm_service.clone(),
    );

    match library_service.get_stats().await {
        Ok(stats) => s.ok(
            "stats",
            format!(
                "{} articles ({} with embeddings) · {} chunks · ~{} embedded tokens",
                stats.total_articles,
                stats.articles_with_embeddings,
                stats.total_chunks,
                stats.total_tokens_embedded
            ),
        ),
        Err(err) => s.fail("stats", format!("{err:#}")),
    }

    let seeded_uid: Option<String> = if seed {
        if !embed_ready {
            s.skip("seed", "needs configured embedding endpoint");
            None
        } else {
            match seed_test_article(&config, &embedding_service).await {
                Ok(uid) => {
                    s.ok("seed", format!("inserted {uid} with 2 embedded chunks"));
                    Some(uid)
                }
                Err(err) => {
                    s.fail("seed", format!("{err:#}"));
                    None
                }
            }
        }
    } else {
        None
    };

    println!("\n[search] query = {query:?}");
    run_search(
        &library_service,
        &query,
        SearchMode::Keyword,
        "keyword (FTS5)",
        &mut s,
    )
    .await;
    if embed_ready {
        run_search(
            &library_service,
            &query,
            SearchMode::Semantic,
            "semantic (sqlite-vec)",
            &mut s,
        )
        .await;
        run_search(
            &library_service,
            &query,
            SearchMode::Hybrid,
            "hybrid (RRF)",
            &mut s,
        )
        .await;
    } else {
        s.skip("semantic (sqlite-vec)", "embedding endpoint not configured");
        s.skip("hybrid (RRF)", "embedding endpoint not configured");
    }

    if let Some(uid) = seeded_uid {
        if keep {
            println!("\n  (--keep set; leaving seeded article {uid} in the db)");
        } else {
            match cleanup_test_article(&config, &uid).await {
                Ok(()) => println!("\n  (cleaned up seeded article {uid})"),
                Err(err) => eprintln!("\n  warning: failed to clean up {uid}: {err:#}"),
            }
        }
    }

    s.print();
    if s.failed > 0 {
        std::process::exit(1);
    }
    Ok(())
}

async fn probe_llm(llm: &LlmConfig) -> Result<String> {
    let client = Client::builder()
        .connect_timeout(Duration::from_secs(llm.connect_timeout_seconds))
        .timeout(Duration::from_secs(llm.request_timeout_seconds.min(60)))
        .build()?;
    let endpoint = format!("{}/chat/completions", llm.base_url);
    // No max_tokens — OpenAI's gpt-5 family rejects it in favour of
    // max_completion_tokens, and other providers vary. The "pong" prompt
    // keeps the reply short on its own.
    let body = json!({
        "model": llm.model,
        "messages": [{"role": "user", "content": "Reply with the single word: pong"}],
        "stream": false,
    });
    let started = Instant::now();
    let resp = client
        .post(&endpoint)
        .bearer_auth(&llm.api_key)
        .json(&body)
        .send()
        .await
        .with_context(|| format!("POST {endpoint}"))?;
    let status = resp.status();
    let body_text = resp.text().await?;
    if !status.is_success() {
        bail!("HTTP {status}: {}", trim(&body_text, 200));
    }
    let payload: Value = serde_json::from_str(&body_text).context("parse response JSON")?;
    let content = payload
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|c| c.first())
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(Value::as_str)
        .unwrap_or("");
    Ok(format!(
        "{} ms · reply: {:?}",
        started.elapsed().as_millis(),
        trim(content, 60)
    ))
}

async fn probe_embedding(embed: &EmbeddingConfig) -> Result<String> {
    let client = Client::builder().timeout(Duration::from_secs(30)).build()?;
    let endpoint = format!("{}/embeddings", embed.base_url);
    let started = Instant::now();
    let mut req = client
        .post(&endpoint)
        .json(&json!({"model": embed.model, "input": "ping"}));
    if !embed.api_key.is_empty() {
        req = req.bearer_auth(&embed.api_key);
    }
    let resp = req
        .send()
        .await
        .with_context(|| format!("POST {endpoint}"))?;
    let status = resp.status();
    let body_text = resp.text().await?;
    if !status.is_success() {
        bail!("HTTP {status}: {}", trim(&body_text, 200));
    }
    let payload: Value = serde_json::from_str(&body_text).context("parse response JSON")?;
    let dim = payload
        .get("data")
        .and_then(Value::as_array)
        .and_then(|d| d.first())
        .and_then(|item| item.get("embedding"))
        .and_then(Value::as_array)
        .map(|v| v.len())
        .unwrap_or(0);
    Ok(format!(
        "{} ms · {} dims",
        started.elapsed().as_millis(),
        dim
    ))
}

async fn run_search(
    library: &LibraryService,
    query: &str,
    mode: SearchMode,
    label: &str,
    s: &mut Summary,
) {
    let req = SearchRequest {
        query: query.to_string(),
        limit: 5,
        mode,
        min_score: None,
        date_from: None,
        date_to: None,
        categories: None,
        rrf_k: 60,
    };
    match library.search(&req).await {
        Ok(resp) => {
            let top = resp
                .results
                .first()
                .map(|r| trim(&r.content, 80))
                .unwrap_or_else(|| "<no rows>".to_string());
            s.ok(
                label,
                format!(
                    "{} hits in {} ms · top: {top}",
                    resp.total_found, resp.search_time_ms
                ),
            );
        }
        Err(err) => s.fail(label, format!("{err:#}")),
    }
}

async fn seed_test_article(
    config: &AppConfig,
    embedding_service: &EmbeddingService,
) -> Result<String> {
    let uid = format!(
        "eval-self-test-{}",
        chrono::Utc::now().format("%Y%m%d%H%M%S")
    );
    let chunks: [&str; 2] = [
        "Transformer architectures fundamentally reshaped natural language processing by replacing recurrence with self-attention.",
        "Graph neural networks model relational structure between entities and excel at link prediction and node classification.",
    ];
    let chunk_strings: Vec<String> = chunks.iter().map(|s| s.to_string()).collect();
    let embeddings = embedding_service.embed_texts(&chunk_strings).await?;

    let db_path = config.storage.database_path.clone();
    let uid_clone = uid.clone();
    tokio::task::spawn_blocking(move || -> Result<()> {
        let conn = Connection::open(&db_path)?;
        conn.busy_timeout(Duration::from_millis(5000))?;
        conn.execute_batch("BEGIN")?;
        conn.execute(
            "INSERT INTO haie_rev (uid, title, first_author, category, content_type, has_embeddings)
             VALUES (?1, ?2, ?3, ?4, ?5, 1)",
            params![
                &uid_clone,
                "Eval self-test article",
                "ResearchWiki Eval",
                "eval",
                "text",
            ],
        )?;
        let mut insert_chunk = conn.prepare(
            "INSERT INTO article_chunks (article_uid, chunk_index, chunk_type, content, token_count, embedded_at)
             VALUES (?1, ?2, 'body', ?3, ?4, datetime('now'))",
        )?;
        let mut insert_vec = conn.prepare(
            "INSERT INTO vec_article_chunks (chunk_id, embedding) VALUES (?1, ?2)",
        )?;
        for (i, (text, emb)) in chunks.iter().zip(embeddings.iter()).enumerate() {
            insert_chunk.execute(params![
                &uid_clone,
                i as i64,
                text,
                (text.len() / 4) as i64,
            ])?;
            let chunk_id = conn.last_insert_rowid();
            insert_vec.execute(params![chunk_id, emb.as_bytes()])?;
        }
        conn.execute_batch("COMMIT")?;
        Ok(())
    })
    .await??;

    Ok(uid)
}

async fn cleanup_test_article(config: &AppConfig, uid: &str) -> Result<()> {
    let db_path = config.storage.database_path.clone();
    let uid = uid.to_string();
    tokio::task::spawn_blocking(move || -> Result<()> {
        let conn = Connection::open(&db_path)?;
        conn.busy_timeout(Duration::from_millis(5000))?;
        // vec0 tables have no FK so we have to delete their rows manually
        // before the haie_rev cascade nukes article_chunks.
        let chunk_ids: Vec<i64> = {
            let mut stmt = conn.prepare("SELECT id FROM article_chunks WHERE article_uid = ?1")?;
            stmt.query_map([&uid], |row| row.get::<_, i64>(0))?
                .collect::<Result<_, _>>()?
        };
        for cid in chunk_ids {
            conn.execute("DELETE FROM vec_article_chunks WHERE chunk_id = ?1", [cid])?;
        }
        conn.execute("DELETE FROM haie_rev WHERE uid = ?1", [&uid])?;
        Ok(())
    })
    .await??;
    Ok(())
}

fn trim(s: &str, n: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(n).collect();
        format!("{truncated}…")
    }
}
