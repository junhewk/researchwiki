use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use researchwiki::{
    app::{bootstrap_db, first_launch_seed},
    config::{AppConfig, EmbeddingConfig, LlmConfig, StorageConfig, normalize_api_key},
    db,
    models::workspace::WorkspaceUpdate,
    register_sqlite_vec,
    services::{
        pipeline::{GATHER_SOURCE_IDS, PipelineService, source_label},
        settings::load_overrides_sync,
    },
    state::AppState,
};
use rusqlite::{Connection, OptionalExtension, params};
use tokio::time::sleep;

const WORKSPACE_SLUG: &str = "diabetes-chatbot-self-management-evidence-map";
const DEMO_LOOKBACK_DAYS: i32 = 30;
const DEMO_BACKFILL_DAYS: i32 = 730;
const DEMO_BACKFILL_TARGET_REAL: i64 = 40;

const DEMO_FOCUSED_QUERIES: &[&str] = &[
    "type 2 diabetes chatbot HbA1c adherence randomized trial",
    "diabetes conversational agent self-management quality of life",
    "large language model diabetes patient education safety escalation misinformation",
];

const DEMO_BACKFILL_QUERIES: &[&str] = &[
    "type 2 diabetes chatbot HbA1c adherence randomized trial",
    "diabetes conversational agent self-management quality of life",
    "large language model diabetes patient education safety escalation misinformation",
    "diabetes digital coaching chatbot",
    "diabetes virtual coach self-management",
    "diabetes ChatGPT patient education",
    "large language model diabetes counseling",
    "CGM conversational agent diabetes counseling",
];

fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    researchwiki::init_tracing();
    register_sqlite_vec();

    let mut config = demo_config_from_env().context("load demo config")?;
    first_launch_seed(&config).context("seed demo directories")?;
    apply_persisted_settings(&mut config);
    apply_env_overrides(&mut config);

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("create runtime")?;

    let options = RunOptions::from_args(std::env::args().skip(1).collect())?;
    runtime.block_on(run(config, options))
}

async fn run(config: AppConfig, options: RunOptions) -> Result<()> {
    if options.kg_wiki_backfill {
        ensure_embedding_configured(&config)
            .context("KG/wiki backfill requires a configured embedding endpoint")?;
    }

    bootstrap_db(&config).await?;

    let root = config
        .storage
        .database_path
        .parent()
        .map(PathBuf::from)
        .context("database path has no parent")?;
    let meta_path = root.join("meta.db");
    let (workspace_id, db_filename) = demo_workspace(&meta_path)?;
    let workspace_db = root.join(db_filename);
    db::initialize_workspace_db(workspace_db.clone(), config.embedding_dimensions).await?;

    let state = AppState::new(config.clone(), workspace_db.clone(), workspace_id);
    state.prompt_service.seed_prompt_versions().await?;
    state.job_service.recover_interrupted_runs().await?;

    let workspace = state.workspace_service.get(workspace_id).await?;
    let queries = options.override_queries();
    state
        .workspace_service
        .update(
            workspace_id,
            WorkspaceUpdate {
                override_queries: Some(queries.iter().map(|query| (*query).to_string()).collect()),
                lookback_days: Some(options.days_back),
                ..WorkspaceUpdate::default()
            },
        )
        .await?;
    set_local_workspace_context(&workspace_db, workspace_id, options.days_back, queries)?;

    let before = article_counts(&workspace_db, workspace_id)?;
    println!("Demo workspace: {}", workspace.name);
    println!(
        "Input Set query source: {}",
        workspace_query_source(&workspace)
    );
    println!(
        "Mode: {}",
        if options.backfill {
            "backfill"
        } else {
            "30-day gather"
        }
    );
    println!("Set gather lookback: {} days", options.days_back);
    println!("Override queries: {}", queries.len());
    for query in queries {
        println!("  - {query}");
    }
    println!(
        "Before gather: {} articles ({} non-demo)",
        before.total, before.real
    );

    if options.kg_wiki_backfill {
        run_kg_wiki_backfill(
            &state,
            &workspace_db,
            workspace_id,
            options.kg_batch_size,
            options.wiki_batch_size,
            &config.storage.wiki_export_dir,
        )
        .await?;
        return Ok(());
    }

    if let Some(target) = options.target_real {
        println!("Target real articles: {target}");
        if before.real >= target {
            println!("Target already satisfied; skipping gather.");
            return Ok(());
        }
    }

    if options.list_only {
        preview_source_candidates(
            &state,
            &workspace_db,
            workspace_id,
            options.days_back,
            options.source_filter.as_deref(),
        )
        .await?;
        return Ok(());
    }

    let run = state
        .job_service
        .enqueue_source("all", options.days_back, workspace_id)
        .await?;
    println!("Started all-source gather: {}", run.run_id);

    let mut last_line = String::new();
    loop {
        let detail = state.job_service.get_job(&run.run_id).await?;
        if let Some(target) = options.target_real {
            let current = article_counts(&workspace_db, workspace_id)?;
            if current.real >= target && detail.run.status == "running" {
                println!(
                    "Target reached ({} real articles); cancelling remaining gather work.",
                    current.real
                );
                let _ = state.job_service.cancel_job(&run.run_id).await;
            }
        }
        let current = detail.run.current_step.as_deref().unwrap_or("idle");
        let item = detail.run.current_item.as_deref().unwrap_or("");
        let line = format!(
            "{} | found={} screened={} relevant={} fetched={} evaluated={} saved={} skipped={} errors={} | {} {}",
            detail.run.status,
            detail.run.candidates_found,
            detail.run.candidates_screened,
            detail.run.candidates_relevant,
            detail.run.candidates_fetched,
            detail.run.candidates_evaluated,
            detail.run.candidates_saved,
            detail.run.candidates_skipped,
            detail.run.errors,
            current,
            item
        );
        if line != last_line {
            println!("{line}");
            last_line = line;
        }
        if matches!(
            detail.run.status.as_str(),
            "completed" | "failed" | "cancelled"
        ) {
            if let Some(error) = detail.run.error_message.as_deref() {
                println!("Last error: {error}");
            }
            break;
        }
        sleep(Duration::from_secs(5)).await;
    }

    let after = article_counts(&workspace_db, workspace_id)?;
    println!(
        "After gather: {} articles ({} non-demo), added {} total / {} non-demo",
        after.total,
        after.real,
        after.total - before.total,
        after.real - before.real
    );
    Ok(())
}

async fn run_kg_wiki_backfill(
    state: &AppState,
    workspace_db: &PathBuf,
    workspace_id: i64,
    kg_batch_size: u32,
    wiki_batch_size: u32,
    wiki_export_dir: &PathBuf,
) -> Result<()> {
    let before = kg_wiki_counts(workspace_db, workspace_id, wiki_export_dir)?;
    println!(
        "Before KG/wiki: {} KG-marked articles, {} entities, {} relationships, {} syntheses, {} wiki files",
        before.kg_articles,
        before.entities,
        before.relationships,
        before.syntheses,
        before.wiki_files
    );
    let response = state
        .knowledge_graph_service
        .start_full_backfill(kg_batch_size, wiki_batch_size)
        .await
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    println!(
        "{} (KG batch {}, wiki batch {})",
        response.message, kg_batch_size, wiki_batch_size
    );

    let mut last_line = String::new();
    loop {
        let status = state
            .knowledge_graph_service
            .get_full_backfill_status()
            .map_err(|error| anyhow::anyhow!("{error}"))?;
        let message = status.message.as_deref().unwrap_or("");
        let line = format!(
            "running={} phase={} | KG batches={} processed={} inserted={} failed={} | Wiki batches={} processed={} compiled={} failed={} | {}",
            status.running,
            status.phase,
            status.kg_batches,
            status.kg_processed,
            status.kg_inserted,
            status.kg_failed,
            status.wiki_batches,
            status.wiki_processed,
            status.wiki_compiled,
            status.wiki_failed,
            message
        );
        if line != last_line {
            println!("{line}");
            last_line = line;
        }
        if !status.running {
            if let Some(error) = status.error {
                bail!("KG/wiki backfill failed: {error}");
            }
            break;
        }
        sleep(Duration::from_secs(5)).await;
    }

    let after = kg_wiki_counts(workspace_db, workspace_id, wiki_export_dir)?;
    println!(
        "After KG/wiki: {} KG-marked articles, {} entities, {} relationships, {} syntheses, {} wiki files",
        after.kg_articles, after.entities, after.relationships, after.syntheses, after.wiki_files
    );
    Ok(())
}

async fn preview_source_candidates(
    state: &AppState,
    workspace_db: &Path,
    workspace_id: i64,
    days_back: i32,
    source_filter: Option<&str>,
) -> Result<()> {
    let context = state
        .workspace_service
        .research_context(workspace_id)
        .await?;
    let contact_email = std::env::var("RESEARCHWIKI_CONTACT_EMAIL")
        .or_else(|_| std::env::var("UNPAYWALL_EMAIL"))
        .ok();
    let pipeline = PipelineService::new(workspace_db.to_path_buf(), contact_email);
    let mut total = 0usize;
    let sources = source_filter
        .map(|source| vec![source])
        .unwrap_or_else(|| GATHER_SOURCE_IDS.to_vec());

    println!("List-only source preview ({days_back} days):");
    for source in sources {
        match pipeline.list_source(source, days_back, &context).await {
            Ok(candidates) => {
                total += candidates.len();
                println!(
                    "  {}: {} candidates",
                    source_label(source).unwrap_or(source),
                    candidates.len()
                );
                for candidate in candidates.iter().take(3) {
                    println!("    - {}", candidate.title);
                }
            }
            Err(error) => {
                println!(
                    "  {}: failed: {error}",
                    source_label(source).unwrap_or(source)
                );
            }
        }
    }
    println!("Total listed candidates: {total}");
    Ok(())
}

fn demo_config_from_env() -> Result<AppConfig> {
    let root = std::env::current_dir()?.join(".demo-data");
    let env_path =
        |key: &str, fallback: PathBuf| std::env::var_os(key).map(PathBuf::from).unwrap_or(fallback);

    Ok(AppConfig {
        storage: StorageConfig {
            database_path: env_path("DATABASE_PATH", root.join("haie.db")),
            prompts_dir: env_path("PROMPTS_DIR", root.join("prompts")),
            settings_file: env_path("SETTINGS_FILE", root.join("settings.json")),
            wiki_export_dir: env_path("WIKI_EXPORT_DIR", root.join("wiki")),
        },
        llm: LlmConfig {
            base_url: std::env::var("LLM_BASE_URL").unwrap_or_default(),
            model: std::env::var("LLM_MODEL").unwrap_or_default(),
            api_key: std::env::var("LLM_API_KEY")
                .map(normalize_api_key)
                .unwrap_or_default(),
            disable_thinking: env_bool("LLM_DISABLE_THINKING", true),
            connect_timeout_seconds: env_parse("LLM_CONNECT_TIMEOUT_SECONDS", 5),
            request_timeout_seconds: env_parse("LLM_REQUEST_TIMEOUT_SECONDS", 300),
            max_attempts: env_parse("LLM_MAX_ATTEMPTS", 1),
            max_concurrent_requests: env_parse("LLM_MAX_CONCURRENT_REQUESTS", 1),
        },
        embedding: EmbeddingConfig {
            base_url: std::env::var("EMBEDDING_BASE_URL").unwrap_or_default(),
            model: std::env::var("EMBEDDING_MODEL").unwrap_or_default(),
            api_key: std::env::var("EMBEDDING_API_KEY")
                .map(normalize_api_key)
                .unwrap_or_default(),
        },
        embedding_dimensions: env_parse("EMBEDDING_DIMENSIONS", 1536),
        contact_email: std::env::var("RESEARCHWIKI_CONTACT_EMAIL")
            .or_else(|_| std::env::var("UNPAYWALL_EMAIL"))
            .unwrap_or_default(),
    })
}

fn ensure_embedding_configured(config: &AppConfig) -> Result<()> {
    if !config.embedding.is_configured() {
        bail!("embedding base URL/model are empty; set EMBEDDING_BASE_URL and EMBEDDING_MODEL");
    }
    if config.embedding.base_url.contains("api.openai.com") && config.embedding.api_key.is_empty() {
        bail!("OpenAI embedding endpoint selected but EMBEDDING_API_KEY/OPENAI_API_KEY is empty");
    }
    Ok(())
}

fn apply_persisted_settings(config: &mut AppConfig) {
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
}

fn apply_env_overrides(config: &mut AppConfig) {
    if let Ok(value) = std::env::var("LLM_BASE_URL") {
        config.llm.base_url = value.trim_end_matches('/').to_string();
    }
    if let Ok(value) = std::env::var("LLM_MODEL") {
        config.llm.model = value;
    }
    if let Ok(value) = std::env::var("LLM_API_KEY") {
        config.llm.api_key = normalize_api_key(value);
    }
    if let Ok(value) = std::env::var("EMBEDDING_BASE_URL") {
        config.embedding.base_url = value.trim_end_matches('/').to_string();
    }
    if let Ok(value) = std::env::var("EMBEDDING_MODEL") {
        config.embedding.model = value;
    }
    if let Ok(value) = std::env::var("EMBEDDING_API_KEY") {
        config.embedding.api_key = normalize_api_key(value);
    }
    if config.embedding.base_url.is_empty() {
        config.embedding.base_url = "https://api.openai.com/v1".to_string();
    }
    if config.embedding.model.is_empty() {
        config.embedding.model = "text-embedding-3-small".to_string();
    }
    if config.embedding.api_key.is_empty() {
        if let Ok(value) = std::env::var("OPENAI_API_KEY") {
            config.embedding.api_key = normalize_api_key(value);
        }
    }
    config.llm.disable_thinking = env_bool("LLM_DISABLE_THINKING", config.llm.disable_thinking);
    config.llm.connect_timeout_seconds = env_parse(
        "LLM_CONNECT_TIMEOUT_SECONDS",
        config.llm.connect_timeout_seconds,
    );
    config.llm.request_timeout_seconds = env_parse(
        "LLM_REQUEST_TIMEOUT_SECONDS",
        config.llm.request_timeout_seconds,
    );
    config.llm.max_attempts = env_parse("LLM_MAX_ATTEMPTS", config.llm.max_attempts);
    config.llm.max_concurrent_requests = env_parse(
        "LLM_MAX_CONCURRENT_REQUESTS",
        config.llm.max_concurrent_requests,
    );
    config.embedding_dimensions = env_parse("EMBEDDING_DIMENSIONS", config.embedding_dimensions);
}

fn demo_workspace(meta_path: &PathBuf) -> Result<(i64, String)> {
    let conn = db::open_connection(meta_path)
        .with_context(|| format!("open meta db {}", meta_path.display()))?;
    let row = conn
        .query_row(
            "SELECT id, db_filename FROM workspaces WHERE slug = ?1",
            [WORKSPACE_SLUG],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    let Some(row) = row else {
        bail!("demo workspace not found; run `cargo run --bin seed_diabetes_demo` first");
    };
    Ok(row)
}

fn set_local_workspace_context(
    db_path: &PathBuf,
    workspace_id: i64,
    days: i32,
    override_queries: &[&str],
) -> Result<()> {
    let conn = db::open_connection(db_path)
        .with_context(|| format!("open workspace db {}", db_path.display()))?;
    let override_json = serde_json::to_string(&override_queries)?;
    conn.execute(
        "UPDATE workspaces
         SET lookback_days = ?2,
             override_queries_json = ?3,
             updated_at = datetime('now')
         WHERE id = ?1",
        params![workspace_id, days, override_json],
    )?;
    Ok(())
}

fn article_counts(db_path: &PathBuf, workspace_id: i64) -> Result<ArticleCounts> {
    let conn = db::open_connection(db_path)
        .with_context(|| format!("open workspace db {}", db_path.display()))?;
    let total = count_articles(&conn, workspace_id, false)?;
    let real = count_articles(&conn, workspace_id, true)?;
    Ok(ArticleCounts { total, real })
}

fn kg_wiki_counts(
    db_path: &PathBuf,
    workspace_id: i64,
    wiki_export_dir: &PathBuf,
) -> Result<KgWikiCounts> {
    let conn = db::open_connection(db_path)
        .with_context(|| format!("open workspace db {}", db_path.display()))?;
    let kg_articles = conn.query_row(
        "SELECT COUNT(*) FROM haie_rev WHERE workspace_id = ?1 AND COALESCE(has_kg_entities, 0) = 1",
        [workspace_id],
        |row| row.get(0),
    )?;
    let entities = conn.query_row(
        "SELECT COUNT(DISTINCT e.id)
         FROM kg_entities e
         JOIN kg_article_entities kae ON kae.entity_id = e.id
         JOIN haie_rev h ON h.uid = kae.article_uid
         WHERE h.workspace_id = ?1",
        [workspace_id],
        |row| row.get(0),
    )?;
    let relationships = conn.query_row(
        "SELECT COUNT(DISTINCT r.id)
         FROM kg_relationships r
         WHERE r.source_entity_id IN (SELECT kae.entity_id FROM kg_article_entities kae JOIN haie_rev h ON h.uid = kae.article_uid WHERE h.workspace_id = ?1)
           AND r.target_entity_id IN (SELECT kae.entity_id FROM kg_article_entities kae JOIN haie_rev h ON h.uid = kae.article_uid WHERE h.workspace_id = ?1)",
        [workspace_id],
        |row| row.get(0),
    )?;
    let syntheses = conn.query_row(
        "SELECT COUNT(DISTINCT s.entity_id)
         FROM kg_entity_syntheses s
         JOIN kg_article_entities kae ON kae.entity_id = s.entity_id
         JOIN haie_rev h ON h.uid = kae.article_uid
         WHERE h.workspace_id = ?1",
        [workspace_id],
        |row| row.get(0),
    )?;
    Ok(KgWikiCounts {
        kg_articles,
        entities,
        relationships,
        syntheses,
        wiki_files: count_markdown_files(wiki_export_dir),
    })
}

fn count_markdown_files(root: &PathBuf) -> i64 {
    let Ok(entries) = std::fs::read_dir(root) else {
        return 0;
    };
    entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .map(|path| {
            if path.is_dir() {
                count_markdown_files(&path)
            } else if path.extension().and_then(|value| value.to_str()) == Some("md") {
                1
            } else {
                0
            }
        })
        .sum()
}

fn count_articles(conn: &Connection, workspace_id: i64, non_demo: bool) -> Result<i64> {
    let sql = if non_demo {
        "SELECT COUNT(*) FROM haie_rev WHERE workspace_id = ?1 AND uid NOT LIKE 'demo-diabetes-%'"
    } else {
        "SELECT COUNT(*) FROM haie_rev WHERE workspace_id = ?1"
    };
    Ok(conn.query_row(sql, [workspace_id], |row| row.get(0))?)
}

fn workspace_query_source(workspace: &researchwiki::models::workspace::Workspace) -> &'static str {
    if !workspace.override_queries.is_empty() {
        "override queries"
    } else if !workspace.seed_concepts.is_empty() {
        "seed concepts"
    } else {
        "source defaults"
    }
}

fn env_bool(key: &str, default: bool) -> bool {
    match std::env::var(key) {
        Ok(value) => match value.to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => true,
            "0" | "false" | "no" | "off" => false,
            _ => default,
        },
        Err(_) => default,
    }
}

fn env_parse<T: std::str::FromStr>(key: &str, default: T) -> T {
    std::env::var(key)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

#[derive(Clone, Copy)]
struct ArticleCounts {
    total: i64,
    real: i64,
}

#[derive(Clone, Copy)]
struct KgWikiCounts {
    kg_articles: i64,
    entities: i64,
    relationships: i64,
    syntheses: i64,
    wiki_files: i64,
}

struct RunOptions {
    list_only: bool,
    backfill: bool,
    kg_wiki_backfill: bool,
    days_back: i32,
    target_real: Option<i64>,
    source_filter: Option<String>,
    kg_batch_size: u32,
    wiki_batch_size: u32,
}

impl RunOptions {
    fn from_args(args: Vec<String>) -> Result<Self> {
        let backfill = args.iter().any(|arg| arg == "--backfill");
        let kg_wiki_backfill = args.iter().any(|arg| arg == "--kg-wiki-backfill");
        let days_default = if backfill {
            DEMO_BACKFILL_DAYS
        } else {
            DEMO_LOOKBACK_DAYS
        };
        let days_back = arg_value(&args, "--days")
            .or_else(|| arg_value(&args, "--backfill-days"))
            .map(|value| parse_arg::<i32>(&value, "--days"))
            .transpose()?
            .unwrap_or(days_default)
            .clamp(1, 3650);
        let target_default = backfill.then_some(DEMO_BACKFILL_TARGET_REAL);
        let target_real = arg_value(&args, "--target-real")
            .map(|value| parse_arg::<i64>(&value, "--target-real"))
            .transpose()?
            .or(target_default)
            .map(|target| target.max(1));

        Ok(Self {
            list_only: args.iter().any(|arg| arg == "--list-only"),
            backfill,
            kg_wiki_backfill,
            days_back,
            target_real,
            source_filter: arg_value(&args, "--source"),
            kg_batch_size: arg_value(&args, "--kg-batch-size")
                .map(|value| parse_arg::<u32>(&value, "--kg-batch-size"))
                .transpose()?
                .unwrap_or(5)
                .max(1),
            wiki_batch_size: arg_value(&args, "--wiki-batch-size")
                .map(|value| parse_arg::<u32>(&value, "--wiki-batch-size"))
                .transpose()?
                .unwrap_or(10)
                .max(1),
        })
    }

    fn override_queries(&self) -> &'static [&'static str] {
        if self.backfill {
            DEMO_BACKFILL_QUERIES
        } else {
            DEMO_FOCUSED_QUERIES
        }
    }
}

fn arg_value(args: &[String], name: &str) -> Option<String> {
    args.windows(2)
        .find_map(|args| (args[0] == name).then(|| args[1].clone()))
}

fn parse_arg<T>(value: &str, name: &str) -> Result<T>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    value
        .parse()
        .map_err(|error| anyhow::anyhow!("invalid {name} value '{value}': {error}"))
}
