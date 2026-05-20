use std::sync::Arc;

use anyhow::{Context, anyhow};
use chrono::{Duration, FixedOffset, Utc};
use rusqlite::{OptionalExtension, params};
use tokio::task;
use uuid::Uuid;

use crate::{
    error::{AppError, run_blocking, run_blocking_db},
    models::{
        job::{JobCreateRequest, JobEventResponse, JobRunDetailResponse, JobRunResponse},
        settings::{SchedulerJob, SchedulerSettings, SchedulerStatusResponse},
    },
    services::{
        evaluator::ArticleEvaluator,
        fetcher::ContentFetcher,
        knowledge_graph::KnowledgeGraphService,
        library::LibraryService,
        llm::LlmService,
        pipeline::{
            ArticleCandidate, GATHER_SOURCE_IDS, PipelineService, SaveCounters, is_gather_source,
            source_label,
        },
        screener::ArticleScreener,
        settings::SettingsService,
        workspace::WorkspaceService,
    },
};

const JOB_RUN_COLUMNS: &str = r#"
    id, source, days_back, status, requested_at, started_at, finished_at,
    candidates_found, candidates_screened, candidates_relevant,
    candidates_fetched, candidates_evaluated, candidates_saved,
    candidates_embedded, candidates_skipped, errors, current_item,
    current_step, error_message
"#;

#[derive(Clone)]
pub struct JobService {
    database_path: Arc<std::path::PathBuf>,
    pipeline_service: PipelineService,
    screener: ArticleScreener,
    fetcher: ContentFetcher,
    evaluator: ArticleEvaluator,
    library_service: Arc<LibraryService>,
    kg_service: Arc<KnowledgeGraphService>,
    settings_service: Arc<SettingsService>,
    workspace_service: Arc<WorkspaceService>,
}

#[derive(Debug, Default, Clone, Copy)]
struct JobCounters {
    found: i32,
    screened: i32,
    relevant: i32,
    fetched: i32,
    evaluated: i32,
    saved: i32,
    embedded: i32,
    skipped: i32,
    errors: i32,
}

impl JobService {
    pub fn new(
        database_path: std::path::PathBuf,
        llm_service: Arc<LlmService>,
        settings_service: Arc<SettingsService>,
        http_client: reqwest::Client,
        library_service: Arc<LibraryService>,
        kg_service: Arc<KnowledgeGraphService>,
        workspace_service: Arc<WorkspaceService>,
    ) -> Self {
        let pipeline_service = PipelineService::new(database_path.clone());
        let screener = ArticleScreener::new(llm_service.clone());
        let fetcher = ContentFetcher::new(http_client.clone());
        let evaluator = ArticleEvaluator::new(llm_service);
        Self {
            database_path: Arc::new(database_path),
            pipeline_service,
            screener,
            fetcher,
            evaluator,
            library_service,
            kg_service,
            settings_service,
            workspace_service,
        }
    }

    pub async fn recover_interrupted_runs(&self) -> Result<u64, AppError> {
        let database_path = self.database_path.clone();
        run_blocking_db(move || {
            let conn = crate::db::open_connection(&*database_path)?;
            let updated = conn.execute(
                "UPDATE job_runs
                 SET status = 'failed',
                     finished_at = COALESCE(finished_at, datetime('now')),
                     current_item = NULL,
                     current_step = 'interrupted',
                     error_message = 'backend restarted while this job was marked running'
                 WHERE status = 'running'",
                [],
            )?;
            Ok::<_, rusqlite::Error>(updated as u64)
        })
        .await
    }

    pub async fn list_jobs(
        &self,
        limit: u32,
        workspace_id: i64,
    ) -> Result<Vec<JobRunResponse>, AppError> {
        let database_path = self.database_path.clone();
        run_blocking(move || {
            let conn = crate::db::open_connection(&*database_path)?;
            let mut stmt = conn.prepare(&format!(
                "SELECT {JOB_RUN_COLUMNS}
                 FROM job_runs
                 WHERE workspace_id = ?1
                 ORDER BY requested_at DESC
                 LIMIT ?2",
            ))?;
            let rows = stmt.query_map(
                params![workspace_id, i64::from(limit.clamp(1, 200))],
                map_job_row,
            )?;
            rows.collect::<Result<Vec<_>, _>>()
                .context("failed to list job runs")
        })
        .await
    }

    pub async fn get_job(&self, run_id: &str) -> Result<JobRunDetailResponse, AppError> {
        let database_path = self.database_path.clone();
        let run_id = run_id.to_string();

        task::spawn_blocking(move || {
            let conn = crate::db::open_connection(&*database_path)?;
            let run = conn
                .query_row(
                    &format!(
                        "SELECT {JOB_RUN_COLUMNS}
                         FROM job_runs
                         WHERE id = ?1"
                    ),
                    [run_id.as_str()],
                    map_job_row,
                )
                .optional()?
                .ok_or_else(|| anyhow!("Run {run_id} not found"))?;

            let mut stmt = conn.prepare(
                "SELECT id, event_type, payload_json, created_at
                 FROM job_events
                 WHERE run_id = ?1
                 ORDER BY id ASC",
            )?;
            let rows = stmt.query_map([run_id.as_str()], |row| {
                Ok(JobEventResponse {
                    id: row.get(0)?,
                    event_type: row.get(1)?,
                    payload_json: row.get(2)?,
                    created_at: row.get(3)?,
                })
            })?;
            let events = rows.collect::<Result<Vec<_>, _>>()?;

            Ok::<_, anyhow::Error>(JobRunDetailResponse { run, events })
        })
        .await
        .map_err(|error| AppError::Internal(error.to_string()))?
        .map_err(not_found_or_conflict_to_app_error)
    }

    pub async fn enqueue_job(
        &self,
        request: JobCreateRequest,
        workspace_id: i64,
    ) -> Result<JobRunResponse, AppError> {
        let source = normalize_source(&request.source)?;
        self.enqueue_source(&source, request.days_back, workspace_id)
            .await
    }

    pub async fn enqueue_source(
        &self,
        source: &str,
        days_back: i32,
        workspace_id: i64,
    ) -> Result<JobRunResponse, AppError> {
        let source = normalize_source(source)?;
        if let Some((run_id, running_source)) = self.find_conflict(&source).await? {
            return Err(AppError::Conflict(format!(
                "pipeline already running for {running_source} (run_id: {run_id})"
            )));
        }

        let database_path = self.database_path.clone();
        let run_id = Uuid::new_v4().to_string();
        // Allow long lookback windows (per-workspace gather backfill); the old
        // 30-day cap is gone, but keep a sane upper bound.
        let days_back = days_back.clamp(1, 3650);
        let source_for_insert = source.clone();
        let run_id_for_insert = run_id.clone();

        let queued_job = task::spawn_blocking(move || {
            let conn = crate::db::open_connection(&*database_path)?;
            conn.execute(
                "INSERT INTO job_runs (id, source, days_back, status, workspace_id)
                 VALUES (?1, ?2, ?3, 'queued', ?4)",
                params![run_id_for_insert, source_for_insert, days_back, workspace_id],
            )?;
            conn.execute(
                "INSERT INTO job_events (run_id, event_type, payload_json)
                 VALUES (?1, 'queued', ?2)",
                params![
                    run_id,
                    serde_json::json!({
                        "source": source,
                        "days_back": days_back,
                    })
                    .to_string()
                ],
            )?;

            conn.query_row(
                &format!(
                    "SELECT {JOB_RUN_COLUMNS}
                     FROM job_runs
                     WHERE id = ?1"
                ),
                [run_id.as_str()],
                map_job_row,
            )
            .context("failed to fetch queued job")
        })
        .await
        .map_err(|error| AppError::Internal(error.to_string()))?
        .map_err(|error| AppError::Internal(error.to_string()))?;

        self.spawn_worker(
            queued_job.run_id.clone(),
            queued_job.source.clone(),
            queued_job.days_back,
            workspace_id,
        );

        Ok(queued_job)
    }

    pub fn scheduler_status(&self, scheduler: &SchedulerSettings) -> SchedulerStatusResponse {
        let status = if scheduler.enabled {
            "running"
        } else {
            "not_running"
        };

        let mut jobs = vec![
            scheduler_job(
                "arxiv_daily",
                "arXiv Daily Gather",
                scheduler.arxiv_schedule_hour,
                scheduler.arxiv_schedule_minute,
                scheduler.enabled,
            ),
            scheduler_job(
                "pmc_daily",
                "PMC Daily Gather",
                scheduler.pmc_schedule_hour,
                scheduler.pmc_schedule_minute,
                scheduler.enabled,
            ),
            scheduler_job(
                "pubmed_daily",
                "PubMed Daily Gather",
                scheduler.pubmed_schedule_hour,
                scheduler.pubmed_schedule_minute,
                scheduler.enabled,
            ),
            manual_job("all_sources_manual", "All Sources Gather"),
        ];

        for &source in GATHER_SOURCE_IDS {
            if matches!(source, "arxiv" | "pmc" | "pubmed") {
                continue;
            }
            let label = source_label(source).unwrap_or(source);
            let id = format!("{source}_manual");
            let name = format!("{label} Gather");
            jobs.push(manual_job(&id, &name));
        }

        SchedulerStatusResponse {
            status: status.to_string(),
            jobs,
        }
    }

    pub async fn cancel_job(&self, run_id: &str) -> Result<JobRunResponse, AppError> {
        let database_path = self.database_path.clone();
        let run_id = run_id.to_string();

        task::spawn_blocking(move || {
            let conn = crate::db::open_connection(&*database_path)?;
            let updated = conn.execute(
                "UPDATE job_runs
                 SET status = 'cancelled',
                     finished_at = COALESCE(finished_at, datetime('now'))
                 WHERE id = ?1 AND status IN ('queued', 'running')",
                [run_id.as_str()],
            )?;

            if updated == 0 {
                let status: Option<String> = conn
                    .query_row(
                        "SELECT status FROM job_runs WHERE id = ?1",
                        [run_id.as_str()],
                        |row| row.get(0),
                    )
                    .optional()?;
                match status.as_deref() {
                    None => return Err(anyhow!("Run {run_id} not found")),
                    Some(status) => {
                        return Err(anyhow!(
                            "Run {run_id} is already {status} and cannot be cancelled"
                        ));
                    }
                }
            }

            conn.execute(
                "INSERT INTO job_events (run_id, event_type, payload_json)
                 VALUES (?1, 'cancelled', ?2)",
                params![
                    run_id.as_str(),
                    serde_json::json!({ "status": "cancelled" }).to_string()
                ],
            )?;

            conn.query_row(
                &format!(
                    "SELECT {JOB_RUN_COLUMNS}
                     FROM job_runs
                     WHERE id = ?1"
                ),
                [run_id.as_str()],
                map_job_row,
            )
            .context("failed to fetch cancelled job")
        })
        .await
        .map_err(|error| AppError::Internal(error.to_string()))?
        .map_err(not_found_or_conflict_to_app_error)
    }

    async fn find_conflict(&self, source: &str) -> Result<Option<(String, String)>, AppError> {
        let database_path = self.database_path.clone();
        let source = source.to_string();
        run_blocking_db(move || {
            let conn = crate::db::open_connection(&*database_path)?;
            if source == "all" {
                conn.query_row(
                    "SELECT id, source
                     FROM job_runs
                     WHERE status = 'running'
                     ORDER BY requested_at DESC
                     LIMIT 1",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()
            } else {
                conn.query_row(
                    "SELECT id, source
                     FROM job_runs
                     WHERE status = 'running' AND source IN (?1, 'all')
                     ORDER BY requested_at DESC
                     LIMIT 1",
                    [source],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()
            }
        })
        .await
    }

    fn spawn_worker(&self, run_id: String, source: String, days_back: i32, workspace_id: i64) {
        let service = self.clone();
        tokio::spawn(async move {
            if let Err(error) = service
                .run_job(run_id.clone(), source, days_back, workspace_id)
                .await
            {
                tracing::error!(run_id = %run_id, error = %error, "background job failed");
            }
        });
    }

    async fn run_job(
        &self,
        run_id: String,
        source: String,
        days_back: i32,
        workspace_id: i64,
    ) -> Result<(), AppError> {
        if self.is_cancelled(&run_id).await? {
            return Ok(());
        }

        self.mark_running(&run_id).await?;
        self.append_event(
            &run_id,
            "started",
            serde_json::json!({ "source": source, "days_back": days_back }),
        )
        .await?;

        // Per-workspace gather/screening profile (seed concepts, topic
        // descriptor), loaded from the registry (meta DB).
        let profile = self
            .workspace_service
            .query_profile(workspace_id)
            .await
            .unwrap_or_default();

        let sources = if source == "all" {
            GATHER_SOURCE_IDS.to_vec()
        } else {
            vec![source.as_str()]
        };

        let mut counters = JobCounters::default();
        let mut last_error = None::<String>;

        for current_source in sources {
            if self.is_cancelled(&run_id).await? {
                return Ok(());
            }

            self.update_progress(
                &run_id,
                &format!("listing:{current_source}"),
                Some(current_source),
                counters,
            )
            .await?;

            let candidates = match self
                .pipeline_service
                .list_source(current_source, days_back, &profile)
                .await
            {
                Ok(candidates) => candidates,
                Err(error) => {
                    counters.errors += 1;
                    let message = format!("{current_source} listing failed: {error}");
                    last_error = Some(message.clone());
                    self.append_event(
                        &run_id,
                        "source_failed",
                        serde_json::json!({
                            "source": current_source,
                            "step": "listing",
                            "error": message,
                        }),
                    )
                    .await?;
                    if source == "all" {
                        continue;
                    }
                    self.finish_run(&run_id, "failed", counters, last_error.as_deref())
                        .await?;
                    return Ok(());
                }
            };

            let found = candidates.len() as i32;
            counters.found += found;
            self.append_event(
                &run_id,
                "listed",
                serde_json::json!({
                    "source": current_source,
                    "candidates_found": found,
                }),
            )
            .await?;

            if self.is_cancelled(&run_id).await? {
                return Ok(());
            }

            // Early deduplication: filter out articles already in the database.
            let uids: Vec<String> = candidates.iter().map(|c| c.uid()).collect();
            let existing = self
                .pipeline_service
                .check_duplicates_batch(&uids)
                .await
                .unwrap_or_default();
            let dedup_skipped = existing.len() as i32;
            counters.skipped += dedup_skipped;
            let candidates: Vec<_> = candidates
                .into_iter()
                .filter(|c| !existing.contains(&c.uid()))
                .collect();

            if self.is_cancelled(&run_id).await? {
                return Ok(());
            }

            // Screening: filter to relevant articles only.
            self.update_progress(
                &run_id,
                &format!("screening:{current_source}"),
                Some(current_source),
                counters,
            )
            .await?;
            let relevant = self
                .screener
                .filter_relevant(&candidates, &profile.topic_descriptor)
                .await;
            counters.screened += candidates.len() as i32;
            counters.relevant += relevant.len() as i32;
            self.append_event(
                &run_id,
                "screened",
                serde_json::json!({
                    "source": current_source,
                    "screened": candidates.len(),
                    "relevant": relevant.len(),
                    "dedup_skipped": dedup_skipped,
                }),
            )
            .await?;

            if self.is_cancelled(&run_id).await? {
                return Ok(());
            }

            // Fetch, evaluate, and save each relevant article.
            for candidate in &relevant {
                if self.is_cancelled(&run_id).await? {
                    return Ok(());
                }

                let title_snippet: String = candidate.title.chars().take(50).collect();
                self.update_progress(
                    &run_id,
                    &format!("processing:{current_source}"),
                    Some(&title_snippet),
                    counters,
                )
                .await?;

                let save_result = self
                    .process_single_article(
                        candidate,
                        &mut counters,
                        &mut last_error,
                        workspace_id,
                    )
                    .await;

                if let Err(error) = save_result {
                    counters.errors += 1;
                    let message = format!("article {} failed: {error}", candidate.uid());
                    tracing::warn!("{message}");
                    last_error = Some(message);
                }
            }

            self.append_event(
                &run_id,
                "source_completed",
                serde_json::json!({
                    "source": current_source,
                    "saved": counters.saved,
                    "fetched": counters.fetched,
                    "evaluated": counters.evaluated,
                    "errors": counters.errors,
                }),
            )
            .await?;
        }

        if self.is_cancelled(&run_id).await? {
            return Ok(());
        }

        let final_status = if counters.errors > 0 && counters.saved == 0 && counters.found == 0 {
            "failed"
        } else {
            "completed"
        };

        self.finish_run(&run_id, final_status, counters, last_error.as_deref())
            .await?;
        self.append_event(
            &run_id,
            final_status,
            serde_json::json!({
                "candidates_found": counters.found,
                "candidates_saved": counters.saved,
                "candidates_skipped": counters.skipped,
                "errors": counters.errors,
            }),
        )
        .await?;

        if final_status == "completed" && counters.saved > 0 {
            match self
                .kg_service
                .start_synthesis_compilation(20, false, None)
                .await
            {
                Ok(response) => {
                    self.append_event(
                        &run_id,
                        "wiki_compile_started",
                        serde_json::json!({
                            "total_entities": response.total_entities,
                        }),
                    )
                    .await?;
                }
                Err(AppError::Conflict(_)) => {
                    tracing::info!("wiki synthesis compilation already running");
                }
                Err(error) => {
                    tracing::warn!("wiki synthesis compilation could not be started: {error}");
                }
            }
        }

        Ok(())
    }

    async fn process_single_article(
        &self,
        candidate: &ArticleCandidate,
        counters: &mut JobCounters,
        last_error: &mut Option<String>,
        workspace_id: i64,
    ) -> Result<(), AppError> {
        // 1. Fetch content.
        let content = self.fetcher.fetch(candidate).await;
        let Some(content) = content else {
            // Fetching failed — fall back to saving metadata only.
            counters.errors += 1;
            let message = format!("fetch failed for {}, saving metadata only", candidate.uid());
            tracing::warn!("{message}");
            *last_error = Some(message);
            let save_result = self
                .pipeline_service
                .save_candidates(vec![candidate.clone()], workspace_id)
                .await
                .map_err(|error| AppError::Internal(error.to_string()))?;
            apply_save_counters(counters, save_result);
            return Ok(());
        };
        counters.fetched += 1;

        // 2. Evaluate content with LLM.
        let evaluation = match self.evaluator.evaluate(&content, candidate).await {
            Ok(Some(fields)) => {
                counters.evaluated += 1;
                Some(fields)
            }
            Ok(None) => {
                tracing::warn!("evaluation returned nothing for {}", candidate.uid());
                None
            }
            Err(error) => {
                counters.errors += 1;
                tracing::warn!("evaluation failed for {}: {error}", candidate.uid());
                *last_error = Some(format!("eval failed for {}: {error}", candidate.uid()));
                None
            }
        };

        // 3. Save article (with evaluation if available, metadata-only otherwise).
        let save_result = if let Some(ref eval_fields) = evaluation {
            self.pipeline_service
                .save_evaluated_candidate(candidate, eval_fields, workspace_id)
                .await
                .map_err(|error| AppError::Internal(error.to_string()))?
        } else {
            self.pipeline_service
                .save_candidates(vec![candidate.clone()], workspace_id)
                .await
                .map_err(|error| AppError::Internal(error.to_string()))?
        };
        apply_save_counters(counters, save_result);

        // Post-save: embedding and KG extraction (fire-and-forget).
        if save_result.saved > 0 {
            let uid = candidate.uid();
            let (library_enabled, kg_enabled) = self
                .settings_service
                .get_feature_flags()
                .await
                .unwrap_or((true, true));

            if library_enabled {
                match self.library_service.process_article(&uid).await {
                    Ok(result) if result.success => counters.embedded += 1,
                    Ok(result) => {
                        tracing::warn!(
                            "embedding failed for {uid}: {}",
                            result.error.unwrap_or_default()
                        );
                    }
                    Err(error) => {
                        tracing::warn!("embedding failed for {uid}: {error}");
                    }
                }
            }

            if kg_enabled {
                if let Err(error) = self.kg_service.insert_articles(vec![uid.clone()]).await {
                    tracing::warn!("KG extraction failed for {uid}: {error}");
                }
            }
        }

        Ok(())
    }

    async fn mark_running(&self, run_id: &str) -> Result<(), AppError> {
        let database_path = self.database_path.clone();
        let run_id = run_id.to_string();
        run_blocking_db(move || {
            let conn = crate::db::open_connection(&*database_path)?;
            conn.execute(
                "UPDATE job_runs
                 SET status = 'running',
                     started_at = COALESCE(started_at, datetime('now')),
                     current_step = 'starting',
                     current_item = NULL,
                     error_message = NULL
                 WHERE id = ?1 AND status != 'cancelled'",
                [run_id.as_str()],
            )?;
            Ok::<_, rusqlite::Error>(())
        })
        .await
    }

    async fn update_progress(
        &self,
        run_id: &str,
        step: &str,
        current_item: Option<&str>,
        counters: JobCounters,
    ) -> Result<(), AppError> {
        let database_path = self.database_path.clone();
        let run_id = run_id.to_string();
        let step = step.to_string();
        let current_item = current_item.map(ToOwned::to_owned);
        run_blocking_db(move || {
            let conn = crate::db::open_connection(&*database_path)?;
            conn.execute(
                "UPDATE job_runs
                 SET current_step = ?2,
                     current_item = ?3,
                     candidates_found = ?4,
                     candidates_screened = ?5,
                     candidates_relevant = ?6,
                     candidates_fetched = ?7,
                     candidates_evaluated = ?8,
                     candidates_saved = ?9,
                     candidates_embedded = ?10,
                     candidates_skipped = ?11,
                     errors = ?12
                 WHERE id = ?1 AND status != 'cancelled'",
                params![
                    run_id,
                    step,
                    current_item,
                    counters.found,
                    counters.screened,
                    counters.relevant,
                    counters.fetched,
                    counters.evaluated,
                    counters.saved,
                    counters.embedded,
                    counters.skipped,
                    counters.errors,
                ],
            )?;
            Ok::<_, rusqlite::Error>(())
        })
        .await
    }

    async fn finish_run(
        &self,
        run_id: &str,
        status: &str,
        counters: JobCounters,
        error_message: Option<&str>,
    ) -> Result<(), AppError> {
        let database_path = self.database_path.clone();
        let run_id = run_id.to_string();
        let status = status.to_string();
        let error_message = error_message.map(ToOwned::to_owned);
        run_blocking_db(move || {
            let conn = crate::db::open_connection(&*database_path)?;
            conn.execute(
                "UPDATE job_runs
                 SET status = ?2,
                     finished_at = COALESCE(finished_at, datetime('now')),
                     current_step = CASE WHEN ?2 = 'completed' THEN 'completed' ELSE current_step END,
                     current_item = NULL,
                     error_message = ?3,
                     candidates_found = ?4,
                     candidates_screened = ?5,
                     candidates_relevant = ?6,
                     candidates_fetched = ?7,
                     candidates_evaluated = ?8,
                     candidates_saved = ?9,
                     candidates_embedded = ?10,
                     candidates_skipped = ?11,
                     errors = ?12
                 WHERE id = ?1 AND status != 'cancelled'",
                params![
                    run_id,
                    status,
                    error_message,
                    counters.found,
                    counters.screened,
                    counters.relevant,
                    counters.fetched,
                    counters.evaluated,
                    counters.saved,
                    counters.embedded,
                    counters.skipped,
                    counters.errors,
                ],
            )?;
            Ok::<_, rusqlite::Error>(())
        })
        .await
    }

    async fn append_event(
        &self,
        run_id: &str,
        event_type: &str,
        payload: serde_json::Value,
    ) -> Result<(), AppError> {
        let database_path = self.database_path.clone();
        let run_id = run_id.to_string();
        let event_type = event_type.to_string();
        let payload_json = payload.to_string();
        run_blocking_db(move || {
            let conn = crate::db::open_connection(&*database_path)?;
            conn.execute(
                "INSERT INTO job_events (run_id, event_type, payload_json)
                 VALUES (?1, ?2, ?3)",
                params![run_id, event_type, payload_json],
            )?;
            Ok::<_, rusqlite::Error>(())
        })
        .await
    }

    async fn is_cancelled(&self, run_id: &str) -> Result<bool, AppError> {
        let database_path = self.database_path.clone();
        let run_id = run_id.to_string();
        run_blocking_db(move || {
            let conn = crate::db::open_connection(&*database_path)?;
            let status = conn
                .query_row(
                    "SELECT status FROM job_runs WHERE id = ?1",
                    [run_id.as_str()],
                    |row| row.get::<_, String>(0),
                )
                .optional()?;
            Ok::<_, rusqlite::Error>(status.as_deref() == Some("cancelled"))
        })
        .await
    }
}

fn apply_save_counters(counters: &mut JobCounters, result: SaveCounters) {
    counters.saved += result.saved;
    counters.skipped += result.skipped;
    counters.errors += result.errors;
}

fn map_job_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<JobRunResponse> {
    Ok(JobRunResponse {
        run_id: row.get(0)?,
        source: row.get(1)?,
        days_back: row.get(2)?,
        status: row.get(3)?,
        requested_at: row.get(4)?,
        started_at: row.get(5)?,
        completed_at: row.get(6)?,
        candidates_found: row.get(7)?,
        candidates_screened: row.get(8)?,
        candidates_relevant: row.get(9)?,
        candidates_fetched: row.get(10)?,
        candidates_evaluated: row.get(11)?,
        candidates_saved: row.get(12)?,
        candidates_embedded: row.get(13)?,
        candidates_skipped: row.get(14)?,
        errors: row.get(15)?,
        current_item: row.get(16)?,
        current_step: row.get(17)?,
        error_message: row.get(18)?,
    })
}

fn normalize_source(source: &str) -> Result<String, AppError> {
    if source == "all" || is_gather_source(source) {
        Ok(source.to_string())
    } else {
        let mut allowed = GATHER_SOURCE_IDS.join(", ");
        allowed.push_str(", all");
        Err(AppError::BadRequest(format!(
            "source must be one of: {allowed}"
        )))
    }
}

fn manual_job(id: &str, name: &str) -> SchedulerJob {
    SchedulerJob {
        id: id.to_string(),
        name: name.to_string(),
        next_run: None,
    }
}

fn scheduler_job(id: &str, name: &str, hour: u8, minute: u8, enabled: bool) -> SchedulerJob {
    let next_run = if enabled {
        let kst = FixedOffset::east_opt(9 * 3600).expect("valid KST offset");
        let now = Utc::now().with_timezone(&kst);
        let today = now.date_naive();
        let Some(today_run_naive) = today.and_hms_opt(u32::from(hour), u32::from(minute), 0) else {
            return SchedulerJob {
                id: id.to_string(),
                name: name.to_string(),
                next_run: None,
            };
        };

        let Some(today_run) = today_run_naive.and_local_timezone(kst).single() else {
            return SchedulerJob {
                id: id.to_string(),
                name: name.to_string(),
                next_run: None,
            };
        };

        let next_run = if today_run > now {
            today_run
        } else {
            today_run + Duration::days(1)
        };

        Some(next_run.to_rfc3339())
    } else {
        None
    };

    SchedulerJob {
        id: id.to_string(),
        name: name.to_string(),
        next_run,
    }
}

fn not_found_or_conflict_to_app_error(error: anyhow::Error) -> AppError {
    if error.to_string().contains("not found") {
        AppError::NotFound(error.to_string())
    } else if error.to_string().contains("cannot be cancelled") {
        AppError::Conflict(error.to_string())
    } else {
        AppError::Internal(error.to_string())
    }
}
