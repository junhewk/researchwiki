use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    net::SocketAddr,
    sync::Arc,
};

use askama::Template;
use axum::{
    Json, Router,
    body::Body,
    extract::{Form, Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
};
use include_dir::{Dir, include_dir};
use pulldown_cmark::{CowStr, Event, HeadingLevel, LinkType, Options, Parser, Tag, TagEnd};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::{net::TcpListener, sync::Mutex};
use tower_http::trace::TraceLayer;

use crate::{
    config::{AppConfig, EmbeddingConfig, LlmConfig},
    db,
    error::AppError,
    models::{
        article::ArticleListQuery,
        job::JobCreateRequest,
        knowledge_graph::{KGGraphDataQuery, KGSynthesisListQuery},
        prompt::{PromptCreate, PromptFileConfig},
        settings::{SchedulerSettings, SettingsUpdate, UiLanguage},
        trace::TraceListQuery,
        workspace::{Workspace, WorkspaceCreate, WorkspaceSummary, WorkspaceUpdate},
    },
    services::{
        llm::LlmOutputMode,
        pipeline::{GATHER_SOURCE_IDS, source_label},
        settings::{SettingsService, load_overrides_sync},
        workspace::WorkspaceService,
    },
    state::AppState,
};

static STATIC_DIR: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/web/static");
const WORKSPACE_COOKIE: &str = "rw_workspace_id";

#[derive(Clone)]
pub struct WebState {
    inner: Arc<WebStateInner>,
}

struct WebStateInner {
    base_config: AppConfig,
    workspace_service: Arc<WorkspaceService>,
    settings_service: Arc<SettingsService>,
    cache: Mutex<HashMap<i64, Arc<AppState>>>,
}

#[derive(Clone)]
struct WorkspaceOption {
    id: i64,
    name: String,
    selected: bool,
}

#[derive(Clone)]
struct NavItem {
    href: String,
    label: &'static str,
    active: bool,
}

#[derive(Template)]
#[template(path = "web/page.html")]
struct PageTemplate {
    title: String,
    active_workspace_id: i64,
    workspaces: Vec<WorkspaceOption>,
    nav: Vec<NavItem>,
    notice: String,
    error: String,
    body: String,
}

#[derive(Clone)]
struct PageContext {
    workspace: Workspace,
    workspaces: Vec<WorkspaceOption>,
    nav: Vec<NavItem>,
    notice: String,
    error: String,
}

#[derive(Debug)]
pub struct WebError {
    status: StatusCode,
    message: String,
}

type WebResult<T> = Result<T, WebError>;

impl WebError {
    fn internal(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: message.into(),
        }
    }
}

impl From<AppError> for WebError {
    fn from(error: AppError) -> Self {
        let status = match error {
            AppError::BadRequest(_) => StatusCode::BAD_REQUEST,
            AppError::Conflict(_) => StatusCode::CONFLICT,
            AppError::NotFound(_) => StatusCode::NOT_FOUND,
            AppError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        Self {
            status,
            message: error.to_string(),
        }
    }
}

impl From<anyhow::Error> for WebError {
    fn from(error: anyhow::Error) -> Self {
        Self::internal(error.to_string())
    }
}

impl IntoResponse for WebError {
    fn into_response(self) -> Response {
        let body = format!(
            "<!doctype html><title>ResearchWiki error</title><main style=\"font-family:system-ui;padding:32px\"><h1>{}</h1><p>{}</p><p><a href=\"/\">Back to ResearchWiki</a></p></main>",
            self.status,
            esc(&self.message)
        );
        (self.status, Html(body)).into_response()
    }
}

impl WebState {
    pub fn new(config: AppConfig) -> Self {
        let root = config
            .storage
            .database_path
            .parent()
            .map(std::path::Path::to_path_buf)
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        let workspace_service = Arc::new(WorkspaceService::new(root.join("meta.db"), root));
        let settings_service = Arc::new(SettingsService::new(config.storage.settings_file.clone()));
        Self {
            inner: Arc::new(WebStateInner {
                base_config: config,
                workspace_service,
                settings_service,
                cache: Mutex::new(HashMap::new()),
            }),
        }
    }

    async fn app_for_workspace(&self, workspace_id: i64) -> WebResult<Arc<AppState>> {
        if let Some(state) = self.inner.cache.lock().await.get(&workspace_id).cloned() {
            return Ok(state);
        }

        let workspace = self.inner.workspace_service.get(workspace_id).await?;
        let mut config = self.effective_config();
        let workspace_db_path = self
            .inner
            .workspace_service
            .db_path_for(&workspace.db_filename);
        db::initialize_workspace_db(workspace_db_path.clone(), config.embedding_dimensions).await?;

        let app_state = Arc::new(AppState::new(
            config.clone(),
            workspace_db_path,
            workspace_id,
        ));
        app_state.prompt_service.seed_prompt_versions().await?;
        app_state.job_service.recover_interrupted_runs().await?;

        let mut cache = self.inner.cache.lock().await;
        let state = cache
            .entry(workspace_id)
            .or_insert_with(|| app_state.clone())
            .clone();
        config.embedding.api_key.clear();
        config.llm.api_key.clear();
        Ok(state)
    }

    fn effective_config(&self) -> AppConfig {
        let mut config = self.inner.base_config.clone();
        let overrides = load_overrides_sync(&config.storage.settings_file);
        if let Some(llm) = overrides.llm {
            config.llm = llm;
        }
        if let Some(embedding) = overrides.embedding {
            config.embedding = embedding;
        }
        if let Some(dimensions) = overrides.embedding_dimensions {
            config.embedding_dimensions = dimensions;
        }
        if let Some(contact_email) = overrides.contact_email {
            config.contact_email = contact_email;
        }
        if let Some(key) = overrides.semantic_scholar_api_key {
            config.semantic_scholar_api_key = key;
        }
        config
    }

    async fn clear_cache(&self) {
        self.inner.cache.lock().await.clear();
    }
}

pub fn router(state: WebState) -> Router {
    Router::new()
        .route("/", get(dashboard_page))
        .route("/workspaces", get(workspaces_page))
        .route("/workspaces/select", post(select_workspace))
        .route("/workspaces/create", post(create_workspace))
        .route("/workspaces/{id}/update", post(update_workspace))
        .route("/gather", get(gather_page))
        .route("/gather/run", post(run_gather))
        .route("/gather/{run_id}/cancel", post(cancel_job))
        .route("/gather/kg-backfill", post(start_kg_backfill))
        .route("/gather/full-backfill", post(start_full_backfill))
        .route("/gather/full-backfill/stop", post(stop_full_backfill))
        .route("/gather/wiki-compile", post(start_wiki_compile))
        .route("/articles", get(articles_page))
        .route("/articles/{uid}", get(article_detail_page))
        .route("/articles/{uid}/pdf", get(article_pdf))
        .route("/articles/{uid}/reextract", post(reextract_article))
        .route("/knowledge-graph", get(knowledge_graph_page))
        .route("/wiki", get(wiki_page))
        .route("/gap-bridge", get(gap_bridge_page))
        .route("/gap-bridge/save", post(save_gap_bridge))
        .route("/gap-bridge/run", post(run_gap_bridge))
        .route("/prompts", get(prompts_page))
        .route("/prompts/{name}", get(prompt_detail_page))
        .route("/prompts/{name}/save", post(save_prompt))
        .route("/prompts/{name}/rewrite", post(rewrite_prompt))
        .route("/traces", get(traces_page))
        .route("/traces/{id}", get(trace_detail_page))
        .route("/settings", get(settings_page))
        .route("/settings/scheduler", post(save_scheduler))
        .route("/settings/llm", post(save_llm))
        .route("/settings/embedding", post(save_embedding))
        .route("/settings/misc", post(save_misc_settings))
        .route("/api/graph-data", get(api_graph_data))
        .route("/api/entity", get(api_entity))
        .route("/api/jobs", get(api_jobs))
        .route("/api/ops", get(api_ops))
        .route("/static/{*path}", get(static_asset))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

pub async fn serve(listener: TcpListener, router: Router) -> std::io::Result<()> {
    axum::serve(listener, router).await
}

pub async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut signal) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            signal.recv().await;
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}

pub async fn bind_addr(addr: &str) -> std::io::Result<(SocketAddr, TcpListener)> {
    let listener = TcpListener::bind(addr).await?;
    let local_addr = listener.local_addr()?;
    Ok((local_addr, listener))
}

#[derive(Debug, Default, Deserialize)]
struct BaseQuery {
    workspace_id: Option<i64>,
    notice: Option<String>,
    error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct WorkspaceSelectForm {
    workspace_id: i64,
}

#[derive(Debug, Deserialize)]
struct WorkspaceForm {
    name: String,
    #[serde(default)]
    primary_question: String,
    #[serde(default)]
    gap_note: String,
    #[serde(default)]
    refined_question: String,
    #[serde(default)]
    topic_descriptor: String,
    #[serde(default)]
    seed_concepts: String,
    #[serde(default)]
    override_queries: String,
    lookback_days: Option<i32>,
    #[serde(default)]
    cadence_days: String,
    cadence_auto: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GatherForm {
    workspace_id: i64,
    source: String,
    days_back: i32,
}

#[derive(Debug, Deserialize)]
struct KgBackfillForm {
    workspace_id: i64,
    batch_size: Option<u32>,
    offset: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct FullBackfillForm {
    workspace_id: i64,
    kg_batch_size: Option<u32>,
    wiki_batch_size: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct WikiCompileForm {
    workspace_id: i64,
    batch_size: Option<u32>,
    force_all: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct ArticlesQuery {
    workspace_id: Option<i64>,
    notice: Option<String>,
    error: Option<String>,
    page: Option<u32>,
    page_size: Option<u32>,
    date_from: Option<String>,
    date_to: Option<String>,
    category: Option<String>,
    search: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct GraphPageQuery {
    workspace_id: Option<i64>,
    notice: Option<String>,
    error: Option<String>,
    entity: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct WikiQuery {
    workspace_id: Option<i64>,
    notice: Option<String>,
    error: Option<String>,
    entity: Option<String>,
    q: Option<String>,
    offset: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct GapSaveForm {
    workspace_id: i64,
    primary_question: String,
    gap_note: String,
    refined_question: String,
}

#[derive(Debug, Deserialize)]
struct GapRunForm {
    workspace_id: i64,
    primary_question: String,
    gap_note: String,
}

#[derive(Debug, Deserialize)]
struct PromptSaveForm {
    workspace_id: i64,
    content: String,
}

#[derive(Debug, Deserialize)]
struct PromptRewriteForm {
    workspace_id: i64,
    content: String,
}

#[derive(Debug, Default, Deserialize)]
struct PromptQuery {
    workspace_id: Option<i64>,
    notice: Option<String>,
    error: Option<String>,
    rewritten: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct TracesQuery {
    workspace_id: Option<i64>,
    notice: Option<String>,
    error: Option<String>,
    page: Option<u32>,
    page_size: Option<u32>,
    prompt_name: Option<String>,
    article_uid: Option<String>,
    model: Option<String>,
    success: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct SchedulerForm {
    workspace_id: i64,
    enabled: Option<String>,
    arxiv_schedule_hour: u8,
    arxiv_schedule_minute: u8,
    pmc_schedule_hour: u8,
    pmc_schedule_minute: u8,
    pubmed_schedule_hour: u8,
    pubmed_schedule_minute: u8,
}

#[derive(Debug, Deserialize)]
struct LlmForm {
    workspace_id: i64,
    base_url: String,
    model: String,
    api_key: String,
    disable_thinking: Option<String>,
    connect_timeout_seconds: u64,
    request_timeout_seconds: u64,
    max_attempts: usize,
    max_concurrent_requests: usize,
}

#[derive(Debug, Deserialize)]
struct EmbeddingForm {
    workspace_id: i64,
    base_url: String,
    model: String,
    api_key: String,
    embedding_dimensions: u32,
}

#[derive(Debug, Deserialize)]
struct MiscSettingsForm {
    workspace_id: i64,
    contact_email: String,
    semantic_scholar_api_key: String,
    ui_language: String,
}

#[derive(Debug, Deserialize)]
struct ApiWorkspaceQuery {
    workspace_id: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct ApiGraphQuery {
    workspace_id: Option<i64>,
    limit: Option<u32>,
    min_degree: Option<u32>,
    entity_types: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ApiEntityQuery {
    workspace_id: Option<i64>,
    entity: String,
}

async fn page_context(
    state: &WebState,
    headers: &HeaderMap,
    query: &BaseQuery,
    active: &'static str,
) -> WebResult<PageContext> {
    let summaries = state.inner.workspace_service.list().await?;
    let requested = query
        .workspace_id
        .or_else(|| workspace_id_from_cookie(headers));
    let selected_id = requested
        .filter(|id| summaries.iter().any(|workspace| workspace.id == *id))
        .unwrap_or(state.inner.workspace_service.active_or_default_id().await?);
    let workspace = state.inner.workspace_service.get(selected_id).await?;
    let workspaces = summaries
        .into_iter()
        .map(|workspace| WorkspaceOption {
            id: workspace.id,
            name: workspace.name,
            selected: workspace.id == selected_id,
        })
        .collect();
    let nav = nav_items(selected_id, active);

    Ok(PageContext {
        workspace,
        workspaces,
        nav,
        notice: query.notice.clone().unwrap_or_default(),
        error: query.error.clone().unwrap_or_default(),
    })
}

fn nav_items(workspace_id: i64, active: &'static str) -> Vec<NavItem> {
    [
        ("dashboard", "Dashboard", "/"),
        ("gather", "Gather", "/gather"),
        ("articles", "Articles", "/articles"),
        ("knowledge-graph", "Graph", "/knowledge-graph"),
        ("wiki", "Wiki", "/wiki"),
        ("workspaces", "Input Set", "/workspaces"),
        ("gap-bridge", "Gap Bridge", "/gap-bridge"),
        ("prompts", "Prompts", "/prompts"),
        ("traces", "Traces", "/traces"),
        ("settings", "Settings", "/settings"),
    ]
    .into_iter()
    .map(|(key, label, href)| NavItem {
        href: format!("{href}?workspace_id={workspace_id}"),
        label,
        active: key == active,
    })
    .collect()
}

fn render_page(ctx: &PageContext, title: impl Into<String>, body: String) -> WebResult<Response> {
    let template = PageTemplate {
        title: title.into(),
        active_workspace_id: ctx.workspace.id,
        workspaces: ctx.workspaces.clone(),
        nav: ctx.nav.clone(),
        notice: ctx.notice.clone(),
        error: ctx.error.clone(),
        body,
    };
    let html = template
        .render()
        .map_err(|error| WebError::internal(error.to_string()))?;
    Ok(Html(html).into_response())
}

async fn dashboard_page(
    State(state): State<WebState>,
    headers: HeaderMap,
    Query(query): Query<BaseQuery>,
) -> WebResult<Response> {
    let ctx = page_context(&state, &headers, &query, "dashboard").await?;
    let app = state.app_for_workspace(ctx.workspace.id).await?;
    let stats = app
        .article_service
        .get_stats(Some(ctx.workspace.id))
        .await?;
    let daily = app
        .article_service
        .get_daily_stats(30, Some(ctx.workspace.id))
        .await?;
    let recent = app
        .article_service
        .get_latest_articles(14, 8, Some(ctx.workspace.id))
        .await?;
    let kg = app
        .knowledge_graph_service
        .get_stats(ctx.workspace.id)
        .await?;

    let max_daily = daily
        .days
        .iter()
        .map(|day| day.count)
        .max()
        .unwrap_or(1)
        .max(1);
    let mut bars = String::new();
    for day in daily.days {
        let height = ((day.count as f64 / max_daily as f64) * 100.0).max(4.0);
        bars.push_str(&format!(
            "<div class=\"bar\" style=\"height:{height:.1}%\" title=\"{}: {}\"><span>{}</span></div>",
            attr(&day.date),
            day.count,
            day.count
        ));
    }

    let mut recent_rows = String::new();
    for article in recent {
        recent_rows.push_str(&format!(
            "<tr><td><a href=\"/articles/{}?workspace_id={}\">{}</a></td><td>{}</td><td>{}</td><td>{}</td></tr>",
            urlencoding::encode(&article.uid),
            ctx.workspace.id,
            esc(article.title.as_deref().unwrap_or(&article.uid)),
            esc(article.first_author.as_deref().unwrap_or("")),
            esc(article.pub_date.as_deref().unwrap_or("")),
            pdf_badge(&article.text_extraction_status, article.pdf_path.as_deref())
        ));
    }

    let body = format!(
        r#"
<section class="page-head">
  <div>
    <h1>{}</h1>
    <p>{}</p>
  </div>
  <a class="button primary" href="/gather?workspace_id={}">Run gather</a>
</section>
<section class="metric-grid">
  <div class="metric"><span>Total articles</span><strong>{}</strong></div>
  <div class="metric"><span>This week</span><strong>{}</strong></div>
  <div class="metric"><span>Evaluated</span><strong>{}</strong></div>
  <div class="metric"><span>Pending</span><strong>{}</strong></div>
  <div class="metric"><span>KG nodes</span><strong>{}</strong></div>
  <div class="metric"><span>KG edges</span><strong>{}</strong></div>
</section>
<section class="grid two">
  <div class="panel">
    <div class="panel-head"><h2>30-day article intake</h2></div>
    <div class="bar-chart">{bars}</div>
  </div>
  <div class="panel">
    <div class="panel-head"><h2>Input set framing</h2><a href="/workspaces?workspace_id={}">Edit</a></div>
    <dl class="kv">
      <dt>Topic</dt><dd>{}</dd>
      <dt>Primary question</dt><dd>{}</dd>
      <dt>Gap note</dt><dd>{}</dd>
      <dt>Query source</dt><dd>{}</dd>
    </dl>
  </div>
</section>
<section class="panel">
  <div class="panel-head"><h2>Recent articles</h2><a href="/articles?workspace_id={}">View all</a></div>
  <table><thead><tr><th>Title</th><th>Author</th><th>Date</th><th>PDF</th></tr></thead><tbody>{}</tbody></table>
</section>
"#,
        esc(&ctx.workspace.name),
        esc(nonempty(
            &ctx.workspace.refined_question,
            &ctx.workspace.primary_question
        )),
        ctx.workspace.id,
        stats.total_articles,
        stats.this_week,
        stats.evaluated_count,
        stats.pending_evaluation,
        kg.nodes,
        kg.edges,
        ctx.workspace.id,
        esc(&ctx.workspace.topic_descriptor),
        esc(&ctx.workspace.primary_question),
        esc(&ctx.workspace.gap_note),
        esc(if ctx.workspace.override_queries.is_empty() {
            "seed concepts"
        } else {
            "override queries"
        }),
        ctx.workspace.id,
        recent_rows
    );
    render_page(&ctx, "Dashboard", body)
}

async fn workspaces_page(
    State(state): State<WebState>,
    headers: HeaderMap,
    Query(query): Query<BaseQuery>,
) -> WebResult<Response> {
    let ctx = page_context(&state, &headers, &query, "workspaces").await?;
    let all = state.inner.workspace_service.list().await?;
    let mut rows = String::new();
    for workspace in all {
        rows.push_str(&workspace_row(&workspace, ctx.workspace.id));
    }

    let body = format!(
        r#"
<section class="page-head"><div><h1>Input Set</h1><p>Research framing shared by gather, screening, KG extraction, wiki synthesis, and Gap Bridge.</p></div></section>
<section class="grid two">
  <form class="panel form" method="post" action="/workspaces/{}/update">
    <div class="panel-head"><h2>Current set</h2><button class="button primary" type="submit">Save</button></div>
    {}
  </form>
  <form class="panel form" method="post" action="/workspaces/create">
    <div class="panel-head"><h2>New set</h2><button class="button primary" type="submit">Create</button></div>
    {}
  </form>
</section>
<section class="panel">
  <div class="panel-head"><h2>Available sets</h2></div>
  <table><thead><tr><th>Name</th><th>Slug</th><th>Status</th><th></th></tr></thead><tbody>{rows}</tbody></table>
</section>
"#,
        ctx.workspace.id,
        workspace_fields(&ctx.workspace, true),
        workspace_fields(&empty_workspace(), false),
    );
    render_page(&ctx, "Input Set", body)
}

fn workspace_row(workspace: &WorkspaceSummary, active_id: i64) -> String {
    let status = if workspace.id == active_id {
        "<span class=\"pill good\">selected</span>"
    } else if workspace.is_active {
        "<span class=\"pill\">registry active</span>"
    } else {
        ""
    };
    format!(
        "<tr><td>{}</td><td>{}</td><td>{status}</td><td><form method=\"post\" action=\"/workspaces/select\"><input type=\"hidden\" name=\"workspace_id\" value=\"{}\"><button class=\"button\" type=\"submit\">Open</button></form></td></tr>",
        esc(&workspace.name),
        esc(&workspace.slug),
        workspace.id
    )
}

fn workspace_fields(workspace: &Workspace, include_refined: bool) -> String {
    let cadence = workspace
        .cadence_days
        .map(|value| value.to_string())
        .unwrap_or_default();
    let refined = if include_refined {
        format!(
            "<label><span>Refined question</span><textarea name=\"refined_question\" rows=\"3\">{}</textarea></label>",
            esc(&workspace.refined_question)
        )
    } else {
        String::new()
    };
    format!(
        r#"
<label><span>Name</span><input name="name" value="{}" required></label>
<label><span>Topic descriptor</span><input name="topic_descriptor" value="{}"></label>
<label><span>Primary question</span><textarea name="primary_question" rows="3">{}</textarea></label>
<label><span>Gap note</span><textarea name="gap_note" rows="3">{}</textarea></label>
{refined}
<label><span>Seed concepts</span><textarea name="seed_concepts" rows="4">{}</textarea></label>
<label><span>Override queries</span><textarea name="override_queries" rows="4">{}</textarea></label>
<div class="form-row">
  <label><span>Lookback days</span><input type="number" min="1" name="lookback_days" value="{}"></label>
  <label><span>Cadence days</span><input type="number" min="1" name="cadence_days" value="{cadence}" placeholder="off"></label>
  <label class="check"><input type="checkbox" name="cadence_auto" value="1" {}> Auto gather</label>
</div>
"#,
        attr(&workspace.name),
        attr(&workspace.topic_descriptor),
        esc(&workspace.primary_question),
        esc(&workspace.gap_note),
        esc(workspace.seed_concepts.join("\n")),
        esc(workspace.override_queries.join("\n")),
        workspace.lookback_days,
        checked(workspace.cadence_auto),
    )
}

fn empty_workspace() -> Workspace {
    Workspace {
        id: 0,
        name: String::new(),
        slug: String::new(),
        db_filename: String::new(),
        primary_question: String::new(),
        gap_note: String::new(),
        refined_question: String::new(),
        seed_concepts: Vec::new(),
        override_queries: Vec::new(),
        topic_descriptor: String::new(),
        lookback_days: 180,
        is_active: false,
        created_at: None,
        updated_at: None,
        cadence_days: None,
        cadence_auto: false,
        last_gathered_at: None,
    }
}

async fn select_workspace(
    State(state): State<WebState>,
    Form(form): Form<WorkspaceSelectForm>,
) -> WebResult<Response> {
    state
        .inner
        .workspace_service
        .set_active(form.workspace_id)
        .await?;
    let location = format!(
        "/?workspace_id={}&notice=Input%20set%20selected",
        form.workspace_id
    );
    Ok(redirect_with_cookie(&location, form.workspace_id))
}

async fn create_workspace(
    State(state): State<WebState>,
    Form(form): Form<WorkspaceForm>,
) -> WebResult<Response> {
    let workspace = state
        .inner
        .workspace_service
        .create(WorkspaceCreate {
            name: form.name,
            primary_question: form.primary_question,
            gap_note: form.gap_note,
            topic_descriptor: form.topic_descriptor,
            seed_concepts: split_lines(&form.seed_concepts),
            override_queries: split_lines(&form.override_queries),
            lookback_days: form.lookback_days.unwrap_or(180).max(1),
        })
        .await?;
    state
        .inner
        .workspace_service
        .update(
            workspace.id,
            WorkspaceUpdate {
                cadence_days: Some(parse_cadence(&form.cadence_days)),
                cadence_auto: Some(form.cadence_auto.is_some()),
                ..WorkspaceUpdate::default()
            },
        )
        .await?;
    state
        .inner
        .workspace_service
        .set_active(workspace.id)
        .await?;
    Ok(redirect_with_cookie(
        &format!(
            "/workspaces?workspace_id={}&notice=Input%20set%20created",
            workspace.id
        ),
        workspace.id,
    ))
}

async fn update_workspace(
    State(state): State<WebState>,
    Path(id): Path<i64>,
    Form(form): Form<WorkspaceForm>,
) -> WebResult<Response> {
    state
        .inner
        .workspace_service
        .update(
            id,
            WorkspaceUpdate {
                name: Some(form.name),
                primary_question: Some(form.primary_question),
                gap_note: Some(form.gap_note),
                refined_question: Some(form.refined_question),
                topic_descriptor: Some(form.topic_descriptor),
                seed_concepts: Some(split_lines(&form.seed_concepts)),
                override_queries: Some(split_lines(&form.override_queries)),
                lookback_days: Some(form.lookback_days.unwrap_or(180).max(1)),
                cadence_days: Some(parse_cadence(&form.cadence_days)),
                cadence_auto: Some(form.cadence_auto.is_some()),
            },
        )
        .await?;
    Ok(redirect_with_cookie(
        &format!("/workspaces?workspace_id={id}&notice=Input%20set%20saved"),
        id,
    ))
}

async fn gather_page(
    State(state): State<WebState>,
    headers: HeaderMap,
    Query(query): Query<BaseQuery>,
) -> WebResult<Response> {
    let ctx = page_context(&state, &headers, &query, "gather").await?;
    let app = state.app_for_workspace(ctx.workspace.id).await?;
    let jobs = app.job_service.list_jobs(25, ctx.workspace.id).await?;
    let settings = app.settings_service.get_settings().await?;
    let scheduler = app.job_service.scheduler_status(&settings.scheduler);
    let kg = app
        .knowledge_graph_service
        .get_stats(ctx.workspace.id)
        .await?;
    let backfill = app.knowledge_graph_service.get_backfill_status()?;
    let full = app.knowledge_graph_service.get_full_backfill_status()?;
    let compile = app.knowledge_graph_service.get_synthesis_compile_status()?;

    let mut source_options = String::new();
    for source in GATHER_SOURCE_IDS {
        source_options.push_str(&format!(
            "<option value=\"{}\">{}</option>",
            attr(source),
            esc(source_label(source).unwrap_or(source))
        ));
    }

    let mut job_rows = String::new();
    for job in jobs {
        let cancel = if job.status == "queued" || job.status == "running" {
            format!(
                "<form method=\"post\" action=\"/gather/{}/cancel\"><input type=\"hidden\" name=\"workspace_id\" value=\"{}\"><button class=\"button danger\" type=\"submit\">Cancel</button></form>",
                attr(&job.run_id),
                ctx.workspace.id
            )
        } else {
            String::new()
        };
        job_rows.push_str(&format!(
            "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}/{}/{}</td><td>{}</td><td>{cancel}</td></tr>",
            esc(&job.source),
            status_pill(&job.status),
            esc(job.requested_at.as_deref().unwrap_or("")),
            job.candidates_found,
            job.candidates_saved,
            job.errors,
            esc(job.current_step.as_deref().unwrap_or(""))
        ));
    }

    let body = format!(
        r#"
<section class="page-head"><div><h1>Gather</h1><p>Run source harvests, monitor jobs, and backfill the knowledge graph and wiki.</p></div></section>
<section class="grid two">
  <form class="panel form" method="post" action="/gather/run">
    <div class="panel-head"><h2>Run gather</h2><button class="button primary" type="submit">Queue</button></div>
    <input type="hidden" name="workspace_id" value="{}">
    <label><span>Source</span><select name="source">{source_options}</select></label>
    <label><span>Days back</span><input type="number" min="1" max="3650" name="days_back" value="{}"></label>
    <p class="hint">PDFs are saved under the configured PDF directory and extracted into article text for KG/wiki use.</p>
  </form>
  <div class="panel">
    <div class="panel-head"><h2>Scheduler</h2><a href="/settings?workspace_id={}">Edit</a></div>
    <dl class="kv">
      <dt>Status</dt><dd>{}</dd>
      <dt>Jobs</dt><dd>{}</dd>
      <dt>KG</dt><dd>{} nodes, {} edges</dd>
    </dl>
  </div>
</section>
<section class="grid three">
  <form class="panel form" method="post" action="/gather/kg-backfill">
    <div class="panel-head"><h2>KG backfill</h2><button class="button" type="submit">Start</button></div>
    <input type="hidden" name="workspace_id" value="{}">
    <label><span>Batch size</span><input type="number" min="1" name="batch_size" value="20"></label>
    <label><span>Offset</span><input type="number" min="0" name="offset" value="0"></label>
    <pre data-status-url="/api/ops?workspace_id={}">{}</pre>
  </form>
  <form class="panel form" method="post" action="/gather/wiki-compile">
    <div class="panel-head"><h2>Wiki compile</h2><button class="button" type="submit">Start</button></div>
    <input type="hidden" name="workspace_id" value="{}">
    <label><span>Batch size</span><input type="number" min="1" name="batch_size" value="20"></label>
    <label class="check"><input type="checkbox" name="force_all" value="1"> Force all</label>
    <pre>{}</pre>
  </form>
  <form class="panel form" method="post" action="/gather/full-backfill">
    <div class="panel-head"><h2>Full rebuild</h2><button class="button" type="submit">Start</button></div>
    <input type="hidden" name="workspace_id" value="{}">
    <label><span>KG batch</span><input type="number" min="1" name="kg_batch_size" value="20"></label>
    <label><span>Wiki batch</span><input type="number" min="1" name="wiki_batch_size" value="20"></label>
    <pre>{}</pre>
  </form>
</section>
<form class="inline" method="post" action="/gather/full-backfill/stop">
  <input type="hidden" name="workspace_id" value="{}">
  <button class="button danger" type="submit">Stop full rebuild</button>
</form>
<section class="panel">
  <div class="panel-head"><h2>Recent runs</h2></div>
  <table><thead><tr><th>Source</th><th>Status</th><th>Requested</th><th>Found/Saved/Errors</th><th>Step</th><th></th></tr></thead><tbody>{job_rows}</tbody></table>
</section>
"#,
        ctx.workspace.id,
        ctx.workspace.lookback_days,
        ctx.workspace.id,
        esc(&scheduler.status),
        scheduler.jobs.len(),
        kg.nodes,
        kg.edges,
        ctx.workspace.id,
        ctx.workspace.id,
        esc(serde_json::to_string_pretty(&backfill).unwrap_or_default()),
        ctx.workspace.id,
        esc(serde_json::to_string_pretty(&compile).unwrap_or_default()),
        ctx.workspace.id,
        esc(serde_json::to_string_pretty(&full).unwrap_or_default()),
        ctx.workspace.id,
    );
    render_page(&ctx, "Gather", body)
}

async fn run_gather(
    State(state): State<WebState>,
    Form(form): Form<GatherForm>,
) -> WebResult<Response> {
    let app = state.app_for_workspace(form.workspace_id).await?;
    let job = app
        .job_service
        .enqueue_job(
            JobCreateRequest {
                source: form.source,
                days_back: form.days_back,
            },
            form.workspace_id,
        )
        .await?;
    Ok(redirect_with_cookie(
        &format!(
            "/gather?workspace_id={}&notice=Queued%20{}",
            form.workspace_id,
            urlencoding::encode(&job.source)
        ),
        form.workspace_id,
    ))
}

async fn cancel_job(
    State(state): State<WebState>,
    Path(run_id): Path<String>,
    Form(form): Form<WorkspaceSelectForm>,
) -> WebResult<Response> {
    let app = state.app_for_workspace(form.workspace_id).await?;
    app.job_service.cancel_job(&run_id).await?;
    Ok(redirect_with_cookie(
        &format!(
            "/gather?workspace_id={}&notice=Run%20cancelled",
            form.workspace_id
        ),
        form.workspace_id,
    ))
}

async fn start_kg_backfill(
    State(state): State<WebState>,
    Form(form): Form<KgBackfillForm>,
) -> WebResult<Response> {
    let app = state.app_for_workspace(form.workspace_id).await?;
    let response = app
        .knowledge_graph_service
        .start_backfill(form.batch_size.unwrap_or(20), form.offset.unwrap_or(0))
        .await?;
    Ok(redirect_with_cookie(
        &format!(
            "/gather?workspace_id={}&notice={}",
            form.workspace_id,
            urlencoding::encode(&response.message)
        ),
        form.workspace_id,
    ))
}

async fn start_full_backfill(
    State(state): State<WebState>,
    Form(form): Form<FullBackfillForm>,
) -> WebResult<Response> {
    let app = state.app_for_workspace(form.workspace_id).await?;
    let response = app
        .knowledge_graph_service
        .start_full_backfill(
            form.kg_batch_size.unwrap_or(20),
            form.wiki_batch_size.unwrap_or(20),
        )
        .await?;
    Ok(redirect_with_cookie(
        &format!(
            "/gather?workspace_id={}&notice={}",
            form.workspace_id,
            urlencoding::encode(&response.message)
        ),
        form.workspace_id,
    ))
}

async fn stop_full_backfill(
    State(state): State<WebState>,
    Form(form): Form<WorkspaceSelectForm>,
) -> WebResult<Response> {
    let app = state.app_for_workspace(form.workspace_id).await?;
    app.knowledge_graph_service.request_full_backfill_stop()?;
    Ok(redirect_with_cookie(
        &format!(
            "/gather?workspace_id={}&notice=Stop%20requested",
            form.workspace_id
        ),
        form.workspace_id,
    ))
}

async fn start_wiki_compile(
    State(state): State<WebState>,
    Form(form): Form<WikiCompileForm>,
) -> WebResult<Response> {
    let app = state.app_for_workspace(form.workspace_id).await?;
    let response = app
        .knowledge_graph_service
        .start_synthesis_compilation(
            form.batch_size.unwrap_or(20),
            form.force_all.is_some(),
            None,
        )
        .await?;
    Ok(redirect_with_cookie(
        &format!(
            "/gather?workspace_id={}&notice={}",
            form.workspace_id,
            urlencoding::encode(&response.message)
        ),
        form.workspace_id,
    ))
}

async fn articles_page(
    State(state): State<WebState>,
    headers: HeaderMap,
    Query(query): Query<ArticlesQuery>,
) -> WebResult<Response> {
    let base = BaseQuery {
        workspace_id: query.workspace_id,
        notice: query.notice.clone(),
        error: query.error.clone(),
    };
    let ctx = page_context(&state, &headers, &base, "articles").await?;
    let app = state.app_for_workspace(ctx.workspace.id).await?;
    let article_query = ArticleListQuery {
        page: query.page.unwrap_or(1),
        page_size: query.page_size.unwrap_or(25),
        date_from: clean_opt(query.date_from.clone()),
        date_to: clean_opt(query.date_to.clone()),
        category: clean_opt(query.category.clone()),
        search: clean_opt(query.search.clone()),
    };
    let list = app
        .article_service
        .list_articles(article_query, Some(ctx.workspace.id))
        .await?;

    let mut rows = String::new();
    for article in &list.items {
        rows.push_str(&format!(
            "<tr><td><a href=\"/articles/{}?workspace_id={}\">{}</a><div class=\"muted\">{}</div></td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
            urlencoding::encode(&article.uid),
            ctx.workspace.id,
            esc(article.title.as_deref().unwrap_or(&article.uid)),
            esc(article.url.as_deref().unwrap_or("")),
            esc(article.first_author.as_deref().unwrap_or("")),
            esc(article.pub_date.as_deref().unwrap_or("")),
            esc(article.primary_issue.as_deref().unwrap_or("")),
            pdf_badge(&article.text_extraction_status, article.pdf_path.as_deref())
        ));
    }

    let page = list.page;
    let prev = page.saturating_sub(1).max(1);
    let next = (page + 1).min(list.pages.max(1));
    let search_value = query.search.unwrap_or_default();
    let body = format!(
        r#"
<section class="page-head"><div><h1>Articles</h1><p>{} articles in this input set.</p></div></section>
<form class="panel filters" method="get" action="/articles">
  <input type="hidden" name="workspace_id" value="{}">
  <input name="search" value="{}" placeholder="Search title, abstract, evaluation">
  <input type="date" name="date_from" value="{}">
  <input type="date" name="date_to" value="{}">
  <input name="category" value="{}" placeholder="Category">
  <button class="button" type="submit">Filter</button>
</form>
<section class="panel">
  <table><thead><tr><th>Title</th><th>Author</th><th>Date</th><th>Issue</th><th>PDF</th></tr></thead><tbody>{rows}</tbody></table>
  <div class="pager">
    <a class="button" href="/articles?workspace_id={}&page={prev}&search={}">Previous</a>
    <span>Page {} / {}</span>
    <a class="button" href="/articles?workspace_id={}&page={next}&search={}">Next</a>
  </div>
</section>
"#,
        list.total,
        ctx.workspace.id,
        attr(&search_value),
        attr(query.date_from.as_deref().unwrap_or("")),
        attr(query.date_to.as_deref().unwrap_or("")),
        attr(query.category.as_deref().unwrap_or("")),
        ctx.workspace.id,
        urlencoding::encode(&search_value),
        list.page,
        list.pages,
        ctx.workspace.id,
        urlencoding::encode(&search_value),
    );
    render_page(&ctx, "Articles", body)
}

async fn article_detail_page(
    State(state): State<WebState>,
    headers: HeaderMap,
    Path(uid): Path<String>,
    Query(query): Query<BaseQuery>,
) -> WebResult<Response> {
    let ctx = page_context(&state, &headers, &query, "articles").await?;
    let app = state.app_for_workspace(ctx.workspace.id).await?;
    let article = app.article_service.get_article(&uid).await?;
    let pdf_link = if article.pdf_path.is_some() {
        format!(
            "<a class=\"button\" href=\"/articles/{}/pdf?workspace_id={}\">Open PDF</a>",
            urlencoding::encode(&article.uid),
            ctx.workspace.id
        )
    } else {
        String::new()
    };
    let reextract = if article.pdf_path.is_some() {
        format!(
            "<form method=\"post\" action=\"/articles/{}/reextract\"><input type=\"hidden\" name=\"workspace_id\" value=\"{}\"><button class=\"button\" type=\"submit\">Re-extract PDF text</button></form>",
            urlencoding::encode(&article.uid),
            ctx.workspace.id
        )
    } else {
        String::new()
    };
    let body = format!(
        r#"
<section class="page-head">
  <div><h1>{}</h1><p>{}</p></div>
  <div class="actions">{pdf_link}{reextract}</div>
</section>
<section class="grid two">
  <div class="panel">
    <div class="panel-head"><h2>Article</h2></div>
    <dl class="kv">
      <dt>UID</dt><dd>{}</dd>
      <dt>URL</dt><dd>{}</dd>
      <dt>Authors</dt><dd>{}</dd>
      <dt>Journal</dt><dd>{}</dd>
      <dt>Published</dt><dd>{}</dd>
      <dt>Content type</dt><dd>{}</dd>
    </dl>
  </div>
  <div class="panel">
    <div class="panel-head"><h2>PDF</h2></div>
    <dl class="kv">
      <dt>Status</dt><dd>{}</dd>
      <dt>Method</dt><dd>{}</dd>
      <dt>Bytes</dt><dd>{}</dd>
      <dt>SHA-256</dt><dd>{}</dd>
      <dt>Source</dt><dd>{}</dd>
      <dt>Error</dt><dd>{}</dd>
    </dl>
  </div>
</section>
<section class="grid two">
  <div class="panel"><div class="panel-head"><h2>Evaluation</h2></div>{}</div>
  <div class="panel"><div class="panel-head"><h2>Notes</h2></div>{}</div>
</section>
"#,
        esc(article.title.as_deref().unwrap_or(&article.uid)),
        esc(article.byline_summary.as_deref().unwrap_or("")),
        esc(&article.uid),
        link_opt(article.url.as_deref()),
        esc(article.authors.as_deref().unwrap_or("")),
        esc(article.journal.as_deref().unwrap_or("")),
        esc(article.pub_date.as_deref().unwrap_or("")),
        esc(article.content_type.as_deref().unwrap_or("")),
        pdf_badge(&article.text_extraction_status, article.pdf_path.as_deref()),
        esc(article.pdf_fetch_method.as_deref().unwrap_or("")),
        article
            .pdf_bytes
            .map(|bytes| bytes.to_string())
            .unwrap_or_default(),
        esc(article.pdf_sha256.as_deref().unwrap_or("")),
        link_opt(article.pdf_source_url.as_deref()),
        esc(article.text_extraction_error.as_deref().unwrap_or("")),
        field_list(&[
            ("AI tech", article.ai_tech.as_deref()),
            ("Clinical domain", article.clinical_domain.as_deref()),
            ("Ethics framework", article.ethics_framework.as_deref()),
            ("Primary issue", article.primary_issue.as_deref()),
            ("Key stakeholders", article.key_stakeholders.as_deref()),
            (
                "Practical implementation",
                article.practical_impl.as_deref()
            ),
        ]),
        field_list(&[
            ("Abstract", article.abstract_text.as_deref()),
            ("Why it matters", article.why_it_matters.as_deref()),
            ("Key argument", article.key_argument.as_deref()),
            ("Main findings", article.main_findings.as_deref()),
            ("Normative claims", article.normative_claims.as_deref()),
            ("Limitations", article.limitations.as_deref()),
            (
                "Theoretical strengths",
                article.theoretical_strengths.as_deref()
            ),
            (
                "Theoretical weaknesses",
                article.theoretical_weaknesses.as_deref()
            ),
            (
                "Empirical strengths",
                article.empirical_strengths.as_deref()
            ),
            (
                "Empirical weaknesses",
                article.empirical_weaknesses.as_deref()
            ),
        ]),
    );
    render_page(&ctx, "Article", body)
}

async fn article_pdf(
    State(state): State<WebState>,
    Path(uid): Path<String>,
    Query(query): Query<BaseQuery>,
) -> WebResult<Response> {
    let workspace_id = query
        .workspace_id
        .unwrap_or(state.inner.workspace_service.active_or_default_id().await?);
    let app = state.app_for_workspace(workspace_id).await?;
    let article = app.article_service.get_article(&uid).await?;
    let path = article.pdf_path.ok_or_else(|| {
        WebError::from(AppError::NotFound("No PDF saved for this article".into()))
    })?;
    let bytes = tokio::fs::read(&path)
        .await
        .map_err(|error| WebError::from(AppError::NotFound(error.to_string())))?;
    Response::builder()
        .header(header::CONTENT_TYPE, "application/pdf")
        .header(
            header::CONTENT_DISPOSITION,
            format!("inline; filename=\"{}.pdf\"", safe_filename(&uid)),
        )
        .body(Body::from(bytes))
        .map_err(|error| WebError::internal(error.to_string()))
}

async fn reextract_article(
    State(state): State<WebState>,
    Path(uid): Path<String>,
    Form(form): Form<WorkspaceSelectForm>,
) -> WebResult<Response> {
    let app = state.app_for_workspace(form.workspace_id).await?;
    app.job_service
        .re_extract_article(&uid, form.workspace_id)
        .await?;
    Ok(redirect_with_cookie(
        &format!(
            "/articles/{}?workspace_id={}&notice=PDF%20text%20re-extracted",
            urlencoding::encode(&uid),
            form.workspace_id
        ),
        form.workspace_id,
    ))
}

async fn knowledge_graph_page(
    State(state): State<WebState>,
    headers: HeaderMap,
    Query(query): Query<GraphPageQuery>,
) -> WebResult<Response> {
    let base = BaseQuery {
        workspace_id: query.workspace_id,
        notice: query.notice,
        error: query.error,
    };
    let ctx = page_context(&state, &headers, &base, "knowledge-graph").await?;
    let app = state.app_for_workspace(ctx.workspace.id).await?;
    let stats = app
        .knowledge_graph_service
        .get_stats(ctx.workspace.id)
        .await?;
    let entity_detail = if let Some(entity) = query.entity.as_deref().and_then(nonempty_opt) {
        match app.knowledge_graph_service.get_entity(entity).await {
            Ok(entity) => format!(
                "<pre>{}</pre>",
                esc(serde_json::to_string_pretty(&entity).unwrap_or_default())
            ),
            Err(error) => format!("<p class=\"notice error\">{}</p>", esc(error.to_string())),
        }
    } else {
        "<p class=\"muted\">Select a node in the graph to open the wiki/entity view.</p>"
            .to_string()
    };
    let type_counts = stats
        .entity_types
        .iter()
        .map(|(kind, count)| format!("<span class=\"pill\">{} {count}</span>", esc(kind)))
        .collect::<Vec<_>>()
        .join(" ");
    let type_options = graph_entity_type_options(&stats.entity_types);
    let body = format!(
        r#"
<section class="page-head"><div><h1>Knowledge Graph</h1><p>{} nodes and {} edges in this workspace.</p></div></section>
<section class="panel graph-panel">
  <div class="graph-toolbar">
    <label>Limit <input id="graph-limit" type="number" min="20" max="1000" value="250"></label>
    <label>Min degree <input id="graph-min-degree" type="number" min="0" value="0"></label>
    <label>Type <select id="graph-types">{type_options}</select></label>
    <button id="graph-load" class="button" type="button">Reload graph</button>
  </div>
  <canvas id="graph-canvas" width="1200" height="680"></canvas>
</section>
<section class="grid two">
  <div class="panel"><div class="panel-head"><h2>Entity types</h2></div><div class="pills">{type_counts}</div></div>
  <div id="graph-detail" class="panel"><div class="panel-head"><h2>Entity detail</h2></div>{entity_detail}</div>
</section>
"#,
        stats.nodes, stats.edges
    );
    render_page(&ctx, "Knowledge Graph", body)
}

async fn wiki_page(
    State(state): State<WebState>,
    headers: HeaderMap,
    Query(query): Query<WikiQuery>,
) -> WebResult<Response> {
    let base = BaseQuery {
        workspace_id: query.workspace_id,
        notice: query.notice.clone(),
        error: query.error.clone(),
    };
    let ctx = page_context(&state, &headers, &base, "wiki").await?;
    let app = state.app_for_workspace(ctx.workspace.id).await?;
    let list = app
        .knowledge_graph_service
        .list_syntheses(
            KGSynthesisListQuery {
                limit: 200,
                offset: query.offset.unwrap_or(0),
                stale_only: false,
                entity_type: None,
            },
            ctx.workspace.id,
        )
        .await?;
    let q = query.q.unwrap_or_default();
    let q_lower = q.to_lowercase();
    let syntheses = list
        .syntheses
        .into_iter()
        .filter(|item| {
            q_lower.is_empty()
                || item.entity_name.to_lowercase().contains(&q_lower)
                || item.summary.to_lowercase().contains(&q_lower)
        })
        .collect::<Vec<_>>();
    let selected_entity = query
        .entity
        .or_else(|| syntheses.first().map(|item| item.entity_name.clone()));
    let wiki_link_targets = app
        .knowledge_graph_service
        .wiki_link_targets(ctx.workspace.id)
        .await?;
    let mut index = String::new();
    for item in &syntheses {
        index.push_str(&format!(
            "<a class=\"wiki-index-item\" href=\"/wiki?workspace_id={}&entity={}&q={}\"><strong>{}</strong><span>{}</span></a>",
            ctx.workspace.id,
            urlencoding::encode(&item.entity_name),
            urlencoding::encode(&q),
            esc(&item.entity_name),
            esc(&item.summary)
        ));
    }

    let detail = if let Some(entity) = selected_entity {
        match app
            .knowledge_graph_service
            .get_entity_synthesis(&entity)
            .await
        {
            Ok(synthesis) => format!(
                r#"<article class="wiki-article">
<h2>{}</h2>
<p class="muted">{} · {} source articles · version {} {}</p>
<p class="lead">{}</p>
<h3>Key aspects</h3><ul>{}</ul>
<h3>Synthesis</h3>{}
</article>"#,
                esc(&synthesis.entity_name),
                esc(&synthesis.entity_type),
                synthesis.source_article_count,
                synthesis.version,
                if synthesis.stale {
                    "<span class=\"pill warn\">stale</span>"
                } else {
                    ""
                },
                esc(&synthesis.summary),
                synthesis
                    .key_aspects
                    .iter()
                    .map(|item| format!("<li>{}</li>", esc(item)))
                    .collect::<Vec<_>>()
                    .join(""),
                render_wiki_markdown_html(
                    &synthesis.synthesis,
                    ctx.workspace.id,
                    &q,
                    &wiki_link_targets,
                    Some(&synthesis.entity_name),
                )
            ),
            Err(error) => format!("<p class=\"notice error\">{}</p>", esc(error.to_string())),
        }
    } else {
        "<p class=\"muted\">No wiki syntheses yet. Run Wiki compile from Gather.</p>".to_string()
    };
    let body = format!(
        r#"
<section class="page-head"><div><h1>Wiki</h1><p>{} compiled entity pages, {} stale.</p></div><a class="button" href="/gather?workspace_id={}">Compile</a></section>
<form class="panel filters" method="get" action="/wiki">
  <input type="hidden" name="workspace_id" value="{}">
  <input name="q" value="{}" placeholder="Search compiled wiki pages">
  <button class="button" type="submit">Search</button>
</form>
<section class="wiki-layout">
  <aside class="panel wiki-index">{index}</aside>
  <div class="panel">{detail}</div>
</section>
"#,
        list.total,
        list.stale_count,
        ctx.workspace.id,
        ctx.workspace.id,
        attr(&q)
    );
    render_page(&ctx, "Wiki", body)
}

async fn gap_bridge_page(
    State(state): State<WebState>,
    headers: HeaderMap,
    Query(query): Query<BaseQuery>,
) -> WebResult<Response> {
    let ctx = page_context(&state, &headers, &query, "gap-bridge").await?;
    let body = format!(
        r#"
<section class="page-head"><div><h1>Gap Bridge</h1><p>Use the current KG and gap note to refine the next research question.</p></div></section>
<section class="grid two">
  <form class="panel form" method="post" action="/gap-bridge/run">
    <div class="panel-head"><h2>Generate</h2><button class="button primary" type="submit">Run Gap Bridge</button></div>
    <input type="hidden" name="workspace_id" value="{}">
    <label><span>Primary question</span><textarea name="primary_question" rows="5">{}</textarea></label>
    <label><span>Gap note</span><textarea name="gap_note" rows="5">{}</textarea></label>
  </form>
  <form class="panel form" method="post" action="/gap-bridge/save">
    <div class="panel-head"><h2>Saved refinement</h2><button class="button" type="submit">Save</button></div>
    <input type="hidden" name="workspace_id" value="{}">
    <input type="hidden" name="primary_question" value="{}">
    <input type="hidden" name="gap_note" value="{}">
    <label><span>Refined question</span><textarea name="refined_question" rows="8">{}</textarea></label>
  </form>
</section>
"#,
        ctx.workspace.id,
        esc(&ctx.workspace.primary_question),
        esc(&ctx.workspace.gap_note),
        ctx.workspace.id,
        attr(&ctx.workspace.primary_question),
        attr(&ctx.workspace.gap_note),
        esc(&ctx.workspace.refined_question),
    );
    render_page(&ctx, "Gap Bridge", body)
}

async fn save_gap_bridge(
    State(state): State<WebState>,
    Form(form): Form<GapSaveForm>,
) -> WebResult<Response> {
    state
        .inner
        .workspace_service
        .update(
            form.workspace_id,
            WorkspaceUpdate {
                primary_question: Some(form.primary_question),
                gap_note: Some(form.gap_note),
                refined_question: Some(form.refined_question),
                ..WorkspaceUpdate::default()
            },
        )
        .await?;
    Ok(redirect_with_cookie(
        &format!(
            "/gap-bridge?workspace_id={}&notice=Gap%20Bridge%20saved",
            form.workspace_id
        ),
        form.workspace_id,
    ))
}

async fn run_gap_bridge(
    State(state): State<WebState>,
    Form(form): Form<GapRunForm>,
) -> WebResult<Response> {
    let app = state.app_for_workspace(form.workspace_id).await?;
    let refined = app
        .knowledge_graph_service
        .generate_gap_bridge(form.workspace_id, form.primary_question, form.gap_note)
        .await?;
    Ok(redirect_with_cookie(
        &format!(
            "/gap-bridge?workspace_id={}&notice={}",
            form.workspace_id,
            urlencoding::encode(&format!("Refined: {refined}"))
        ),
        form.workspace_id,
    ))
}

async fn prompts_page(
    State(state): State<WebState>,
    headers: HeaderMap,
    Query(query): Query<BaseQuery>,
) -> WebResult<Response> {
    let ctx = page_context(&state, &headers, &query, "prompts").await?;
    let app = state.app_for_workspace(ctx.workspace.id).await?;
    let prompts = app.prompt_service.list_prompts().await?;
    let mut rows = String::new();
    for prompt in prompts {
        rows.push_str(&format!(
            "<tr><td><a href=\"/prompts/{}?workspace_id={}\">{}</a></td><td>{}</td><td>{}</td><td>{}</td></tr>",
            urlencoding::encode(&prompt.name),
            ctx.workspace.id,
            esc(&prompt.name),
            esc(prompt.model.as_deref().unwrap_or("")),
            prompt.temperature.map(|value| format!("{value:.2}")).unwrap_or_default(),
            prompt.execution_count
        ));
    }
    let body = format!(
        r#"
<section class="page-head"><div><h1>Prompts</h1><p>Edit YAML prompts and keep versions in the workspace database.</p></div></section>
<section class="panel">
  <table><thead><tr><th>Name</th><th>Model</th><th>Temperature</th><th>Runs</th></tr></thead><tbody>{rows}</tbody></table>
</section>
"#
    );
    render_page(&ctx, "Prompts", body)
}

async fn prompt_detail_page(
    State(state): State<WebState>,
    headers: HeaderMap,
    Path(name): Path<String>,
    Query(query): Query<PromptQuery>,
) -> WebResult<Response> {
    let base = BaseQuery {
        workspace_id: query.workspace_id,
        notice: query.notice,
        error: query.error,
    };
    let ctx = page_context(&state, &headers, &base, "prompts").await?;
    let app = state.app_for_workspace(ctx.workspace.id).await?;
    let prompt = app.prompt_service.get_prompt(&name).await?;
    let versions = app.prompt_service.list_versions(&name).await?;
    let editor = query.rewritten.unwrap_or(prompt.content);
    let mut version_rows = String::new();
    for version in versions.into_iter().take(12) {
        version_rows.push_str(&format!(
            "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
            version.version,
            esc(version.model.as_deref().unwrap_or("")),
            version
                .temperature
                .map(|value| format!("{value:.2}"))
                .unwrap_or_default(),
            esc(version.created_at.to_rfc3339())
        ));
    }
    let body = format!(
        r#"
<section class="page-head"><div><h1>{}</h1><p>Versioned YAML prompt.</p></div><a class="button" href="/prompts?workspace_id={}">All prompts</a></section>
<section class="grid two wide-left">
  <form class="panel form" method="post" action="/prompts/{}/save">
    <div class="panel-head"><h2>Editor</h2><button class="button primary" type="submit">Save</button></div>
    <input type="hidden" name="workspace_id" value="{}">
    <textarea class="code-editor" name="content" rows="32">{}</textarea>
  </form>
  <div class="panel">
    <div class="panel-head"><h2>Actions</h2></div>
    <form method="post" action="/prompts/{}/rewrite">
      <input type="hidden" name="workspace_id" value="{}">
      <textarea class="hidden" name="content">{}</textarea>
      <button class="button" type="submit">Rewrite for input set</button>
    </form>
    <h3>Recent versions</h3>
    <table><thead><tr><th>Version</th><th>Model</th><th>Temp</th><th>Created</th></tr></thead><tbody>{version_rows}</tbody></table>
  </div>
</section>
"#,
        esc(&name),
        ctx.workspace.id,
        urlencoding::encode(&name),
        ctx.workspace.id,
        esc(&editor),
        urlencoding::encode(&name),
        ctx.workspace.id,
        esc(&editor),
    );
    render_page(&ctx, "Prompt", body)
}

async fn save_prompt(
    State(state): State<WebState>,
    Path(name): Path<String>,
    Form(form): Form<PromptSaveForm>,
) -> WebResult<Response> {
    let app = state.app_for_workspace(form.workspace_id).await?;
    app.prompt_service
        .update_prompt(
            &name,
            PromptCreate {
                content: form.content,
                description: Some("Edited in Axum web UI".to_string()),
            },
        )
        .await?;
    Ok(redirect_with_cookie(
        &format!(
            "/prompts/{}?workspace_id={}&notice=Prompt%20saved",
            urlencoding::encode(&name),
            form.workspace_id
        ),
        form.workspace_id,
    ))
}

async fn rewrite_prompt(
    State(state): State<WebState>,
    Path(name): Path<String>,
    Form(form): Form<PromptRewriteForm>,
) -> WebResult<Response> {
    let app = state.app_for_workspace(form.workspace_id).await?;
    let workspace = state.inner.workspace_service.get(form.workspace_id).await?;
    let mut vars = BTreeMap::new();
    vars.insert("original_prompt".to_string(), form.content);
    vars.insert("topic_descriptor".to_string(), workspace.topic_descriptor);
    vars.insert("primary_question".to_string(), workspace.primary_question);
    vars.insert(
        "seed_concepts".to_string(),
        workspace.seed_concepts.join(", "),
    );
    let response = app
        .llm_service
        .execute_prompt("prompt_rewriter", vars, None, LlmOutputMode::Text)
        .await?;
    let content = strip_code_fences(response.raw_text.trim());
    serde_yaml::from_str::<PromptFileConfig>(&content).map_err(|error| {
        AppError::BadRequest(format!("rewritten prompt is not valid YAML: {error}"))
    })?;
    Ok(redirect_with_cookie(
        &format!(
            "/prompts/{}?workspace_id={}&notice=Prompt%20rewritten&rewritten={}",
            urlencoding::encode(&name),
            form.workspace_id,
            urlencoding::encode(&content)
        ),
        form.workspace_id,
    ))
}

async fn traces_page(
    State(state): State<WebState>,
    headers: HeaderMap,
    Query(query): Query<TracesQuery>,
) -> WebResult<Response> {
    let base = BaseQuery {
        workspace_id: query.workspace_id,
        notice: query.notice,
        error: query.error,
    };
    let ctx = page_context(&state, &headers, &base, "traces").await?;
    let app = state.app_for_workspace(ctx.workspace.id).await?;
    let summary = app.trace_service.get_summary().await?;
    let traces = app
        .trace_service
        .list_traces(TraceListQuery {
            page: query.page.unwrap_or(1),
            page_size: query.page_size.unwrap_or(25),
            prompt_name: clean_opt(query.prompt_name.clone()),
            article_uid: clean_opt(query.article_uid.clone()),
            model: clean_opt(query.model.clone()),
            success: query.success,
        })
        .await?;
    let mut summary_rows = String::new();
    for item in summary {
        summary_rows.push_str(&format!(
            "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
            esc(&item.prompt_name),
            item.total_executions,
            item.successful_executions,
            item.failed_executions,
            item.avg_latency_ms
                .map(|value| format!("{value:.0} ms"))
                .unwrap_or_default()
        ));
    }
    let mut trace_rows = String::new();
    for trace in traces.items {
        trace_rows.push_str(&format!(
            "<tr><td><a href=\"/traces/{}?workspace_id={}\">{}</a></td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
            trace.id,
            ctx.workspace.id,
            trace.id,
            esc(&trace.prompt_name),
            esc(trace.model.as_deref().unwrap_or("")),
            if trace.success { "<span class=\"pill good\">ok</span>" } else { "<span class=\"pill bad\">failed</span>" },
            esc(trace.created_at.as_deref().unwrap_or(""))
        ));
    }
    let body = format!(
        r#"
<section class="page-head"><div><h1>Traces</h1><p>Prompt execution history for debugging extraction, evaluation, and synthesis.</p></div></section>
<section class="grid two">
  <div class="panel"><div class="panel-head"><h2>Summary</h2></div><table><thead><tr><th>Prompt</th><th>Total</th><th>OK</th><th>Failed</th><th>Avg latency</th></tr></thead><tbody>{summary_rows}</tbody></table></div>
  <form class="panel filters" method="get" action="/traces">
    <input type="hidden" name="workspace_id" value="{}">
    <input name="prompt_name" value="{}" placeholder="Prompt">
    <input name="article_uid" value="{}" placeholder="Article UID">
    <input name="model" value="{}" placeholder="Model">
    <button class="button" type="submit">Filter</button>
  </form>
</section>
<section class="panel"><table><thead><tr><th>ID</th><th>Prompt</th><th>Model</th><th>Status</th><th>Created</th></tr></thead><tbody>{trace_rows}</tbody></table></section>
"#,
        ctx.workspace.id,
        attr(query.prompt_name.as_deref().unwrap_or("")),
        attr(query.article_uid.as_deref().unwrap_or("")),
        attr(query.model.as_deref().unwrap_or("")),
    );
    render_page(&ctx, "Traces", body)
}

async fn trace_detail_page(
    State(state): State<WebState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Query(query): Query<BaseQuery>,
) -> WebResult<Response> {
    let ctx = page_context(&state, &headers, &query, "traces").await?;
    let app = state.app_for_workspace(ctx.workspace.id).await?;
    let trace = app.trace_service.get_trace(id).await?;
    let body = format!(
        r#"
<section class="page-head"><div><h1>Trace #{}</h1><p>{} · {}</p></div><a class="button" href="/traces?workspace_id={}">All traces</a></section>
<section class="grid two">
  <div class="panel"><div class="panel-head"><h2>Input</h2></div><pre>{}</pre></div>
  <div class="panel"><div class="panel-head"><h2>Output</h2></div><pre>{}</pre></div>
</section>
<section class="panel"><div class="panel-head"><h2>Metadata</h2></div>{}</section>
"#,
        trace.id,
        esc(&trace.prompt_name),
        esc(trace.model.as_deref().unwrap_or("")),
        ctx.workspace.id,
        esc(trace.input_text.as_deref().unwrap_or("")),
        esc(trace.output_text.as_deref().unwrap_or("")),
        field_list(&[
            ("Article UID", trace.article_uid.as_deref()),
            ("Error", trace.error_message.as_deref()),
            ("Created", trace.created_at.as_deref()),
        ])
    );
    render_page(&ctx, "Trace", body)
}

async fn settings_page(
    State(state): State<WebState>,
    headers: HeaderMap,
    Query(query): Query<BaseQuery>,
) -> WebResult<Response> {
    let ctx = page_context(&state, &headers, &query, "settings").await?;
    let config = state.effective_config();
    let settings = state.inner.settings_service.get_settings().await?;
    let llm = state
        .inner
        .settings_service
        .get_llm_config()
        .await?
        .unwrap_or(config.llm);
    let embedding = state
        .inner
        .settings_service
        .get_embedding_config()
        .await?
        .unwrap_or(config.embedding);
    let embedding_dimensions = state
        .inner
        .settings_service
        .get_embedding_dimensions()
        .await?
        .unwrap_or(config.embedding_dimensions);
    let contact_email = state
        .inner
        .settings_service
        .get_contact_email()
        .await?
        .unwrap_or_default();
    let semantic_key = state
        .inner
        .settings_service
        .get_semantic_scholar_api_key()
        .await?
        .unwrap_or_default();
    let body = format!(
        r#"
<section class="page-head"><div><h1>Settings</h1><p>Runtime settings shared by desktop and web UI. Newsletter settings have been removed.</p></div></section>
<section class="grid two">
  <form class="panel form" method="post" action="/settings/scheduler">
    <div class="panel-head"><h2>Scheduler</h2><button class="button primary" type="submit">Save</button></div>
    <input type="hidden" name="workspace_id" value="{}">
    <label class="check"><input type="checkbox" name="enabled" value="1" {}> Enabled</label>
    <div class="form-row"><label><span>arXiv hour</span><input type="number" min="0" max="23" name="arxiv_schedule_hour" value="{}"></label><label><span>Minute</span><input type="number" min="0" max="59" name="arxiv_schedule_minute" value="{}"></label></div>
    <div class="form-row"><label><span>PMC hour</span><input type="number" min="0" max="23" name="pmc_schedule_hour" value="{}"></label><label><span>Minute</span><input type="number" min="0" max="59" name="pmc_schedule_minute" value="{}"></label></div>
    <div class="form-row"><label><span>PubMed hour</span><input type="number" min="0" max="23" name="pubmed_schedule_hour" value="{}"></label><label><span>Minute</span><input type="number" min="0" max="59" name="pubmed_schedule_minute" value="{}"></label></div>
  </form>
  <form class="panel form" method="post" action="/settings/llm">
    <div class="panel-head"><h2>LLM</h2><button class="button primary" type="submit">Save</button></div>
    <input type="hidden" name="workspace_id" value="{}">
    <label><span>Base URL</span><input name="base_url" value="{}"></label>
    <label><span>Model</span><input name="model" value="{}"></label>
    <label><span>API key</span><input type="password" name="api_key" value="{}"></label>
    <label class="check"><input type="checkbox" name="disable_thinking" value="1" {}> Disable thinking</label>
    <div class="form-row"><label><span>Connect timeout</span><input type="number" min="1" name="connect_timeout_seconds" value="{}"></label><label><span>Request timeout</span><input type="number" min="10" name="request_timeout_seconds" value="{}"></label></div>
    <div class="form-row"><label><span>Attempts</span><input type="number" min="1" max="5" name="max_attempts" value="{}"></label><label><span>Concurrency</span><input type="number" min="1" max="16" name="max_concurrent_requests" value="{}"></label></div>
  </form>
  <form class="panel form" method="post" action="/settings/embedding">
    <div class="panel-head"><h2>Embeddings</h2><button class="button primary" type="submit">Save</button></div>
    <input type="hidden" name="workspace_id" value="{}">
    <label><span>Base URL</span><input name="base_url" value="{}"></label>
    <label><span>Model</span><input name="model" value="{}"></label>
    <label><span>API key</span><input type="password" name="api_key" value="{}"></label>
    <label><span>Dimensions</span><input type="number" min="1" name="embedding_dimensions" value="{}"></label>
  </form>
  <form class="panel form" method="post" action="/settings/misc">
    <div class="panel-head"><h2>Sources and UI</h2><button class="button primary" type="submit">Save</button></div>
    <input type="hidden" name="workspace_id" value="{}">
    <label><span>Contact email</span><input name="contact_email" value="{}"></label>
    <label><span>Semantic Scholar key</span><input type="password" name="semantic_scholar_api_key" value="{}"></label>
    <label><span>Language</span><select name="ui_language">{}</select></label>
  </form>
</section>
"#,
        ctx.workspace.id,
        checked(settings.scheduler.enabled),
        settings.scheduler.arxiv_schedule_hour,
        settings.scheduler.arxiv_schedule_minute,
        settings.scheduler.pmc_schedule_hour,
        settings.scheduler.pmc_schedule_minute,
        settings.scheduler.pubmed_schedule_hour,
        settings.scheduler.pubmed_schedule_minute,
        ctx.workspace.id,
        attr(&llm.base_url),
        attr(&llm.model),
        attr(&llm.api_key),
        checked(llm.disable_thinking),
        llm.connect_timeout_seconds,
        llm.request_timeout_seconds,
        llm.max_attempts,
        llm.max_concurrent_requests,
        ctx.workspace.id,
        attr(&embedding.base_url),
        attr(&embedding.model),
        attr(&embedding.api_key),
        embedding_dimensions,
        ctx.workspace.id,
        attr(&contact_email),
        attr(&semantic_key),
        language_options(settings.ui_language),
    );
    render_page(&ctx, "Settings", body)
}

async fn save_scheduler(
    State(state): State<WebState>,
    Form(form): Form<SchedulerForm>,
) -> WebResult<Response> {
    state
        .inner
        .settings_service
        .update_settings(SettingsUpdate {
            scheduler: Some(SchedulerSettings {
                arxiv_schedule_hour: form.arxiv_schedule_hour.min(23),
                arxiv_schedule_minute: form.arxiv_schedule_minute.min(59),
                pmc_schedule_hour: form.pmc_schedule_hour.min(23),
                pmc_schedule_minute: form.pmc_schedule_minute.min(59),
                pubmed_schedule_hour: form.pubmed_schedule_hour.min(23),
                pubmed_schedule_minute: form.pubmed_schedule_minute.min(59),
                enabled: form.enabled.is_some(),
            }),
            ui_language: None,
        })
        .await?;
    Ok(settings_redirect(form.workspace_id, "Scheduler%20saved"))
}

async fn save_llm(State(state): State<WebState>, Form(form): Form<LlmForm>) -> WebResult<Response> {
    state
        .inner
        .settings_service
        .set_llm_config(LlmConfig {
            base_url: form.base_url.trim_end_matches('/').to_string(),
            model: form.model,
            api_key: form.api_key,
            disable_thinking: form.disable_thinking.is_some(),
            connect_timeout_seconds: form.connect_timeout_seconds.clamp(1, 120),
            request_timeout_seconds: form.request_timeout_seconds.clamp(10, 900),
            max_attempts: form.max_attempts.clamp(1, 5),
            max_concurrent_requests: form.max_concurrent_requests.clamp(1, 16),
        })
        .await?;
    state.clear_cache().await;
    Ok(settings_redirect(
        form.workspace_id,
        "LLM%20settings%20saved",
    ))
}

async fn save_embedding(
    State(state): State<WebState>,
    Form(form): Form<EmbeddingForm>,
) -> WebResult<Response> {
    state
        .inner
        .settings_service
        .set_embedding_config(EmbeddingConfig {
            base_url: form.base_url.trim_end_matches('/').to_string(),
            model: form.model,
            api_key: form.api_key,
        })
        .await?;
    state
        .inner
        .settings_service
        .set_embedding_dimensions(form.embedding_dimensions)
        .await?;
    state.clear_cache().await;
    Ok(settings_redirect(
        form.workspace_id,
        "Embedding%20settings%20saved",
    ))
}

async fn save_misc_settings(
    State(state): State<WebState>,
    Form(form): Form<MiscSettingsForm>,
) -> WebResult<Response> {
    state
        .inner
        .settings_service
        .set_contact_email(clean_opt(Some(form.contact_email)))
        .await?;
    state
        .inner
        .settings_service
        .set_semantic_scholar_api_key(clean_opt(Some(form.semantic_scholar_api_key)))
        .await?;
    let language = if form.ui_language == "korean" {
        UiLanguage::Korean
    } else {
        UiLanguage::English
    };
    state
        .inner
        .settings_service
        .set_ui_language(language)
        .await?;
    state.clear_cache().await;
    Ok(settings_redirect(form.workspace_id, "Settings%20saved"))
}

async fn api_graph_data(
    State(state): State<WebState>,
    Query(query): Query<ApiGraphQuery>,
) -> WebResult<Json<serde_json::Value>> {
    let workspace_id = query
        .workspace_id
        .unwrap_or(state.inner.workspace_service.active_or_default_id().await?);
    let app = state.app_for_workspace(workspace_id).await?;
    let graph = app
        .knowledge_graph_service
        .get_graph_data(
            KGGraphDataQuery {
                limit: query.limit.unwrap_or(250),
                min_degree: query.min_degree.unwrap_or(0),
                entity_types: clean_opt(query.entity_types),
            },
            workspace_id,
        )
        .await?;
    Ok(Json(json!(graph)))
}

async fn api_entity(
    State(state): State<WebState>,
    Query(query): Query<ApiEntityQuery>,
) -> WebResult<Json<serde_json::Value>> {
    let workspace_id = query
        .workspace_id
        .unwrap_or(state.inner.workspace_service.active_or_default_id().await?);
    let app = state.app_for_workspace(workspace_id).await?;
    let entity = app
        .knowledge_graph_service
        .get_entity(&query.entity)
        .await?;
    Ok(Json(json!(entity)))
}

async fn api_jobs(
    State(state): State<WebState>,
    Query(query): Query<ApiWorkspaceQuery>,
) -> WebResult<Json<serde_json::Value>> {
    let workspace_id = query
        .workspace_id
        .unwrap_or(state.inner.workspace_service.active_or_default_id().await?);
    let app = state.app_for_workspace(workspace_id).await?;
    let jobs = app.job_service.list_jobs(20, workspace_id).await?;
    Ok(Json(json!({ "jobs": jobs })))
}

async fn api_ops(
    State(state): State<WebState>,
    Query(query): Query<ApiWorkspaceQuery>,
) -> WebResult<Json<serde_json::Value>> {
    let workspace_id = query
        .workspace_id
        .unwrap_or(state.inner.workspace_service.active_or_default_id().await?);
    let app = state.app_for_workspace(workspace_id).await?;
    Ok(Json(json!({
        "backfill": app.knowledge_graph_service.get_backfill_status()?,
        "full_backfill": app.knowledge_graph_service.get_full_backfill_status()?,
        "wiki_compile": app.knowledge_graph_service.get_synthesis_compile_status()?,
    })))
}

async fn static_asset(Path(path): Path<String>) -> WebResult<Response> {
    let path = path.trim_start_matches('/');
    let file = STATIC_DIR.get_file(path).ok_or_else(|| {
        WebError::from(AppError::NotFound(format!(
            "static asset not found: {path}"
        )))
    })?;
    let mime = mime_guess::from_path(path).first_or_octet_stream();
    Response::builder()
        .header(header::CONTENT_TYPE, mime.as_ref())
        .body(Body::from(file.contents().to_vec()))
        .map_err(|error| WebError::internal(error.to_string()))
}

fn redirect_with_cookie(location: &str, workspace_id: i64) -> Response {
    let mut response = Redirect::to(location).into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        format!("{WORKSPACE_COOKIE}={workspace_id}; Path=/; SameSite=Lax")
            .parse()
            .expect("cookie header is valid"),
    );
    response
}

fn settings_redirect(workspace_id: i64, notice: &str) -> Response {
    redirect_with_cookie(
        &format!("/settings?workspace_id={workspace_id}&notice={notice}"),
        workspace_id,
    )
}

fn workspace_id_from_cookie(headers: &HeaderMap) -> Option<i64> {
    let cookie = headers.get(header::COOKIE)?.to_str().ok()?;
    cookie.split(';').find_map(|part| {
        let (key, value) = part.trim().split_once('=')?;
        (key == WORKSPACE_COOKIE)
            .then(|| value.parse::<i64>().ok())
            .flatten()
    })
}

fn split_lines(input: &str) -> Vec<String> {
    input
        .split(['\n', ','])
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn parse_cadence(input: &str) -> Option<i32> {
    input.trim().parse::<i32>().ok().filter(|days| *days >= 1)
}

fn clean_opt(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn nonempty<'a>(preferred: &'a str, fallback: &'a str) -> &'a str {
    if preferred.trim().is_empty() {
        fallback
    } else {
        preferred
    }
}

fn nonempty_opt(value: &str) -> Option<&str> {
    (!value.trim().is_empty()).then_some(value)
}

fn checked(value: bool) -> &'static str {
    if value { "checked" } else { "" }
}

fn language_options(selected: UiLanguage) -> String {
    UiLanguage::ALL
        .into_iter()
        .map(|language| {
            let value = match language {
                UiLanguage::English => "english",
                UiLanguage::Korean => "korean",
            };
            format!(
                "<option value=\"{value}\" {}>{}</option>",
                if language == selected { "selected" } else { "" },
                esc(language.label())
            )
        })
        .collect::<Vec<_>>()
        .join("")
}

fn status_pill(status: &str) -> String {
    let class = match status {
        "completed" => "pill good",
        "failed" | "cancelled" => "pill bad",
        "running" | "queued" => "pill warn",
        _ => "pill",
    };
    format!("<span class=\"{class}\">{}</span>", esc(status))
}

fn pdf_badge(status: &Option<String>, pdf_path: Option<&str>) -> String {
    match (pdf_path, status.as_deref()) {
        (Some(_), Some("ok" | "extracted")) => {
            "<span class=\"pill good\">extracted</span>".to_string()
        }
        (Some(_), Some(value)) if !value.is_empty() => {
            format!("<span class=\"pill warn\">{}</span>", esc(value))
        }
        (Some(_), _) => "<span class=\"pill\">saved</span>".to_string(),
        (None, _) => "<span class=\"pill muted-pill\">none</span>".to_string(),
    }
}

fn field_list(fields: &[(&str, Option<&str>)]) -> String {
    let mut out = String::from("<dl class=\"kv\">");
    for (label, value) in fields {
        out.push_str(&format!(
            "<dt>{}</dt><dd>{}</dd>",
            esc(label),
            render_plain_markdown_html(value.unwrap_or(""))
        ));
    }
    out.push_str("</dl>");
    out
}

fn link_opt(url: Option<&str>) -> String {
    match url.and_then(nonempty_opt) {
        Some(url) => format!(
            "<a href=\"{}\" rel=\"noreferrer\" target=\"_blank\">{}</a>",
            attr(url),
            esc(url)
        ),
        None => String::new(),
    }
}

fn graph_entity_type_options(entity_types: &BTreeMap<String, i64>) -> String {
    let mut options = vec!["<option value=\"\">All types</option>".to_string()];
    options.extend(entity_types.iter().map(|(kind, count)| {
        format!(
            "<option value=\"{}\">{} ({count})</option>",
            attr(kind),
            esc(entity_type_label(kind))
        )
    }));
    options.join("")
}

fn entity_type_label(entity_type: &str) -> String {
    entity_type
        .split('_')
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => {
                    first.to_uppercase().collect::<String>() + &chars.as_str().to_lowercase()
                }
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn render_plain_markdown_html(input: &str) -> String {
    render_markdown_html(input, None)
}

fn render_wiki_markdown_html(
    input: &str,
    workspace_id: i64,
    q: &str,
    link_targets: &[String],
    current_entity: Option<&str>,
) -> String {
    let current = current_entity.map(normalize_link_target);
    let mut targets = link_targets
        .iter()
        .filter_map(|name| {
            let name = name.trim();
            if name.is_empty() {
                return None;
            }
            let normalized = normalize_link_target(name);
            if current.as_ref() == Some(&normalized) {
                return None;
            }
            Some(WikiLinkTarget {
                name: name.to_string(),
                normalized,
            })
        })
        .collect::<Vec<_>>();
    targets.sort_by(|left, right| {
        right
            .name
            .len()
            .cmp(&left.name.len())
            .then_with(|| left.name.cmp(&right.name))
    });
    targets.dedup_by(|left, right| left.normalized == right.normalized);

    let mut context = WikiLinkContext {
        workspace_id,
        q,
        targets,
        used_auto_links: BTreeSet::new(),
    };
    render_markdown_html(input, Some(&mut context))
}

struct WikiLinkTarget {
    name: String,
    normalized: String,
}

struct WikiLinkContext<'a> {
    workspace_id: i64,
    q: &'a str,
    targets: Vec<WikiLinkTarget>,
    used_auto_links: BTreeSet<String>,
}

fn render_markdown_html(input: &str, mut link_context: Option<&mut WikiLinkContext<'_>>) -> String {
    let options = Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_FOOTNOTES;
    let rewritten_input = link_context
        .as_deref_mut()
        .map(|context| rewrite_explicit_wiki_links(input, context));
    let parser_input = rewritten_input.as_deref().unwrap_or(input);
    let parser = Parser::new_ext(parser_input, options);
    let mut events = Vec::<Event<'static>>::new();
    let mut link_depth = 0usize;
    let mut code_block_depth = 0usize;

    for event in parser {
        match event {
            Event::Start(tag) => {
                if matches!(tag, Tag::Link { .. }) {
                    link_depth += 1;
                }
                if matches!(tag, Tag::CodeBlock(_)) {
                    code_block_depth += 1;
                }
                events.push(Event::Start(tag.into_static()));
            }
            Event::End(tag) => {
                if tag == TagEnd::Link {
                    link_depth = link_depth.saturating_sub(1);
                }
                if tag == TagEnd::CodeBlock {
                    code_block_depth = code_block_depth.saturating_sub(1);
                }
                events.push(Event::End(tag));
            }
            Event::Text(text) if link_depth == 0 && code_block_depth == 0 => {
                if let Some(context) = link_context.as_deref_mut() {
                    push_auto_linked_text(&mut events, text.as_ref(), context);
                } else {
                    events.push(Event::Text(text.into_static()));
                }
            }
            Event::Html(raw) | Event::InlineHtml(raw) => {
                events.push(Event::Text(raw.into_static()));
            }
            other => events.push(other.into_static()),
        }
    }

    render_markdown_events_to_html(&events)
}

fn render_markdown_events_to_html(events: &[Event<'static>]) -> String {
    let mut out = String::new();
    for event in events {
        match event {
            Event::Start(tag) => push_markdown_start(&mut out, tag),
            Event::End(tag) => push_markdown_end(&mut out, *tag),
            Event::Text(text) => out.push_str(&esc(text)),
            Event::Code(code) => {
                out.push_str("<code>");
                out.push_str(&esc(code));
                out.push_str("</code>");
            }
            Event::InlineMath(math) => {
                out.push_str("<code>");
                out.push_str(&esc(math));
                out.push_str("</code>");
            }
            Event::DisplayMath(math) => {
                out.push_str("<pre><code>");
                out.push_str(&esc(math));
                out.push_str("</code></pre>");
            }
            Event::Html(raw) | Event::InlineHtml(raw) => out.push_str(&esc(raw)),
            Event::FootnoteReference(label) => {
                out.push_str("<sup>");
                out.push_str(&esc(label));
                out.push_str("</sup>");
            }
            Event::SoftBreak => out.push('\n'),
            Event::HardBreak => out.push_str("<br>"),
            Event::Rule => out.push_str("<hr>"),
            Event::TaskListMarker(checked) => {
                out.push_str("<input type=\"checkbox\" disabled");
                if *checked {
                    out.push_str(" checked");
                }
                out.push('>');
            }
        }
    }
    out
}

fn push_markdown_start(out: &mut String, tag: &Tag<'_>) {
    match tag {
        Tag::Paragraph => out.push_str("<p>"),
        Tag::Heading { level, .. } => {
            out.push_str(&format!("<h{}>", heading_level_number(*level)));
        }
        Tag::BlockQuote(_) => out.push_str("<blockquote>"),
        Tag::CodeBlock(_) => out.push_str("<pre><code>"),
        Tag::HtmlBlock => {}
        Tag::List(Some(start)) => {
            if *start == 1 {
                out.push_str("<ol>");
            } else {
                out.push_str(&format!("<ol start=\"{start}\">"));
            }
        }
        Tag::List(None) => out.push_str("<ul>"),
        Tag::Item => out.push_str("<li>"),
        Tag::FootnoteDefinition(label) => {
            out.push_str("<section class=\"footnote\" id=\"fn-");
            out.push_str(&attr(label));
            out.push_str("\">");
        }
        Tag::DefinitionList => out.push_str("<dl>"),
        Tag::DefinitionListTitle => out.push_str("<dt>"),
        Tag::DefinitionListDefinition => out.push_str("<dd>"),
        Tag::Table(_) => out.push_str("<table>"),
        Tag::TableHead => out.push_str("<thead><tr>"),
        Tag::TableRow => out.push_str("<tr>"),
        Tag::TableCell => out.push_str("<td>"),
        Tag::Emphasis => out.push_str("<em>"),
        Tag::Strong => out.push_str("<strong>"),
        Tag::Strikethrough => out.push_str("<del>"),
        Tag::Superscript => out.push_str("<sup>"),
        Tag::Subscript => out.push_str("<sub>"),
        Tag::Link {
            dest_url, title, ..
        } => {
            out.push_str("<a href=\"");
            out.push_str(&attr(dest_url));
            out.push('"');
            if !title.is_empty() {
                out.push_str(" title=\"");
                out.push_str(&attr(title));
                out.push('"');
            }
            out.push('>');
        }
        Tag::Image {
            dest_url, title, ..
        } => {
            out.push_str("<a href=\"");
            out.push_str(&attr(dest_url));
            out.push_str("\" rel=\"noreferrer\" target=\"_blank\"");
            if !title.is_empty() {
                out.push_str(" title=\"");
                out.push_str(&attr(title));
                out.push('"');
            }
            out.push('>');
        }
        Tag::MetadataBlock(_) => {}
    }
}

fn push_markdown_end(out: &mut String, tag: TagEnd) {
    match tag {
        TagEnd::Paragraph => out.push_str("</p>"),
        TagEnd::Heading(level) => {
            out.push_str(&format!("</h{}>", heading_level_number(level)));
        }
        TagEnd::BlockQuote(_) => out.push_str("</blockquote>"),
        TagEnd::CodeBlock => out.push_str("</code></pre>"),
        TagEnd::HtmlBlock => {}
        TagEnd::List(true) => out.push_str("</ol>"),
        TagEnd::List(false) => out.push_str("</ul>"),
        TagEnd::Item => out.push_str("</li>"),
        TagEnd::FootnoteDefinition => out.push_str("</section>"),
        TagEnd::DefinitionList => out.push_str("</dl>"),
        TagEnd::DefinitionListTitle => out.push_str("</dt>"),
        TagEnd::DefinitionListDefinition => out.push_str("</dd>"),
        TagEnd::Table => out.push_str("</table>"),
        TagEnd::TableHead => out.push_str("</tr></thead>"),
        TagEnd::TableRow => out.push_str("</tr>"),
        TagEnd::TableCell => out.push_str("</td>"),
        TagEnd::Emphasis => out.push_str("</em>"),
        TagEnd::Strong => out.push_str("</strong>"),
        TagEnd::Strikethrough => out.push_str("</del>"),
        TagEnd::Superscript => out.push_str("</sup>"),
        TagEnd::Subscript => out.push_str("</sub>"),
        TagEnd::Link | TagEnd::Image => out.push_str("</a>"),
        TagEnd::MetadataBlock(_) => {}
    }
}

fn heading_level_number(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

fn rewrite_explicit_wiki_links(input: &str, context: &mut WikiLinkContext<'_>) -> String {
    let mut rendered = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(start) = rest.find("[[") {
        rendered.push_str(&rest[..start]);
        let after_start = &rest[start + 2..];
        let Some(end) = after_start.find("]]") else {
            rendered.push_str(&rest[start..]);
            return rendered;
        };

        let raw = &after_start[..end];
        let (target, label) = raw
            .split_once('|')
            .map(|(target, label)| (target.trim(), label.trim()))
            .unwrap_or_else(|| {
                let target = raw.trim();
                (target, target)
            });
        if target.is_empty() {
            rendered.push_str("[[]]");
        } else {
            context
                .used_auto_links
                .insert(normalize_link_target(target));
            rendered.push_str(&format!(
                "[{}]({})",
                escape_markdown_link_label(label),
                wiki_href(context.workspace_id, target, context.q)
            ));
        }
        rest = &after_start[end + 2..];
    }
    rendered.push_str(rest);
    rendered
}

fn escape_markdown_link_label(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('[', "\\[")
        .replace(']', "\\]")
}

fn push_auto_linked_text(
    events: &mut Vec<Event<'static>>,
    text: &str,
    context: &mut WikiLinkContext<'_>,
) {
    let mut rest = text;
    while let Some(match_) = next_auto_link(rest, context) {
        if match_.start > 0 {
            events.push(Event::Text(cow(&rest[..match_.start])));
        }
        let target_name = context.targets[match_.target_index].name.clone();
        let normalized = context.targets[match_.target_index].normalized.clone();
        push_internal_wiki_link(
            events,
            &rest[match_.start..match_.end],
            &target_name,
            context.workspace_id,
            context.q,
        );
        context.used_auto_links.insert(normalized);
        rest = &rest[match_.end..];
    }
    if !rest.is_empty() {
        events.push(Event::Text(cow(rest)));
    }
}

struct AutoLinkMatch {
    start: usize,
    end: usize,
    target_index: usize,
}

fn next_auto_link(text: &str, context: &WikiLinkContext<'_>) -> Option<AutoLinkMatch> {
    let mut best: Option<AutoLinkMatch> = None;
    for (target_index, target) in context.targets.iter().enumerate() {
        if context.used_auto_links.contains(&target.normalized) {
            continue;
        }
        let Some((start, end)) = find_entity_name(text, &target.name) else {
            continue;
        };
        let replace = best.as_ref().is_none_or(|current| {
            start < current.start
                || (start == current.start
                    && target.name.len() > context.targets[current.target_index].name.len())
        });
        if replace {
            best = Some(AutoLinkMatch {
                start,
                end,
                target_index,
            });
        }
    }
    best
}

fn find_entity_name(text: &str, target: &str) -> Option<(usize, usize)> {
    if target.len() < 3 || !target.is_ascii() {
        return None;
    }
    for (start, _) in text.char_indices() {
        let end = start + target.len();
        let Some(candidate) = text.get(start..end) else {
            continue;
        };
        if candidate.eq_ignore_ascii_case(target) && has_entity_boundary(text, start, end) {
            return Some((start, end));
        }
    }
    None
}

fn has_entity_boundary(text: &str, start: usize, end: usize) -> bool {
    let before = text[..start].chars().next_back();
    let after = text[end..].chars().next();
    before.is_none_or(|ch| !is_entity_word_char(ch))
        && after.is_none_or(|ch| !is_entity_word_char(ch))
}

fn is_entity_word_char(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_' || ch == '-'
}

fn push_internal_wiki_link(
    events: &mut Vec<Event<'static>>,
    label: &str,
    target: &str,
    workspace_id: i64,
    q: &str,
) {
    events.push(Event::Start(Tag::Link {
        link_type: LinkType::Inline,
        dest_url: cow(wiki_href(workspace_id, target, q)),
        title: cow(""),
        id: cow(""),
    }));
    events.push(Event::Text(cow(label)));
    events.push(Event::End(TagEnd::Link));
}

fn wiki_href(workspace_id: i64, entity: &str, q: &str) -> String {
    format!(
        "/wiki?workspace_id={workspace_id}&entity={}&q={}",
        urlencoding::encode(entity),
        urlencoding::encode(q)
    )
}

fn normalize_link_target(value: &str) -> String {
    value.trim().to_lowercase()
}

fn cow(value: impl AsRef<str>) -> CowStr<'static> {
    CowStr::Boxed(value.as_ref().to_string().into_boxed_str())
}

fn strip_code_fences(text: &str) -> String {
    let trimmed = text.trim();
    if let Some(without_open) = trimmed.strip_prefix("```") {
        let without_lang = without_open
            .strip_prefix("yaml")
            .or_else(|| without_open.strip_prefix("yml"))
            .unwrap_or(without_open)
            .trim_start_matches('\n');
        if let Some((content, _)) = without_lang.rsplit_once("```") {
            return content.trim().to_string();
        }
    }
    trimmed.to_string()
}

fn safe_filename(input: &str) -> String {
    input
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

fn esc(input: impl AsRef<str>) -> String {
    input
        .as_ref()
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn attr(input: impl AsRef<str>) -> String {
    esc(input)
}

#[allow(dead_code)]
#[derive(Serialize)]
struct ApiOk {
    ok: bool,
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{graph_entity_type_options, render_plain_markdown_html, render_wiki_markdown_html};

    #[test]
    fn graph_type_options_include_methodology_and_condition() {
        let mut types = BTreeMap::new();
        types.insert("METHODOLOGY".to_string(), 12);
        types.insert("MEDICAL_CONDITION".to_string(), 7);

        let html = graph_entity_type_options(&types);

        assert!(html.contains("<option value=\"\">All types</option>"));
        assert!(html.contains("value=\"METHODOLOGY\""));
        assert!(html.contains(">Methodology (12)</option>"));
        assert!(html.contains("value=\"MEDICAL_CONDITION\""));
        assert!(html.contains(">Medical Condition (7)</option>"));
    }

    #[test]
    fn markdown_renderer_escapes_raw_html() {
        let html = render_plain_markdown_html("<script>alert(1)</script>");

        assert!(html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
        assert!(!html.contains("<script>"));
    }

    #[test]
    fn wiki_renderer_links_explicit_wiki_syntax() {
        let html = render_wiki_markdown_html(
            "See [[Machine Learning|ML]] and [[Artificial Intelligence]].",
            1,
            "clinical ai",
            &[],
            None,
        );

        assert!(html.contains(
            "<a href=\"/wiki?workspace_id=1&amp;entity=Machine%20Learning&amp;q=clinical%20ai\">ML</a>"
        ));
        assert!(html.contains(
            "<a href=\"/wiki?workspace_id=1&amp;entity=Artificial%20Intelligence&amp;q=clinical%20ai\">Artificial Intelligence</a>"
        ));
    }

    #[test]
    fn wiki_renderer_auto_links_known_targets_once() {
        let targets = vec![
            "Machine Learning".to_string(),
            "Artificial Intelligence".to_string(),
        ];
        let html = render_wiki_markdown_html(
            "Machine learning relates to Artificial Intelligence. Machine Learning appears again.",
            1,
            "",
            &targets,
            None,
        );

        assert_eq!(html.matches("entity=Machine%20Learning").count(), 1);
        assert_eq!(html.matches("entity=Artificial%20Intelligence").count(), 1);
    }

    #[test]
    fn wiki_renderer_does_not_auto_link_code_or_existing_links() {
        let targets = vec!["Machine Learning".to_string()];
        let html = render_wiki_markdown_html(
            "`Machine Learning` [Machine Learning](https://example.com) Machine Learning",
            1,
            "",
            &targets,
            None,
        );

        assert!(html.contains("<code>Machine Learning</code>"));
        assert!(html.contains("<a href=\"https://example.com\">Machine Learning</a>"));
        assert_eq!(html.matches("entity=Machine%20Learning").count(), 1);
    }
}
