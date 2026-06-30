use std::{collections::HashSet, sync::Arc};

use anyhow::{Context, anyhow};
use chrono::{Duration, Local};
use rusqlite::{OptionalExtension, params};
use tokio::task;
use uuid::Uuid;

use crate::{
    error::{AppError, run_blocking, run_blocking_db},
    models::{
        job::{JobCreateRequest, JobEventResponse, JobRunDetailResponse, JobRunResponse},
        settings::{SchedulerJob, SchedulerSettings, SchedulerStatusResponse},
        workspace::WorkspaceResearchContext,
    },
    services::{
        evaluator::ArticleEvaluator,
        fetcher::ContentFetcher,
        knowledge_graph::KnowledgeGraphService,
        library::LibraryService,
        llm::LlmService,
        pipeline::{
            ArticleCandidate, FetchedArticleContent, GATHER_SOURCE_IDS, PipelineService,
            SaveCounters, is_gather_source, source_label,
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

const CANDIDATE_LISTED: &str = "listed";
const CANDIDATE_DEDUPED: &str = "deduped";
const CANDIDATE_SKIPPED_DUPLICATE: &str = "skipped_duplicate";
const CANDIDATE_SKIPPED_CROSS_SOURCE: &str = "skipped_cross_source";
const CANDIDATE_RELEVANT: &str = "relevant";
const CANDIDATE_SCREENED_OUT: &str = "screened_out";
const CANDIDATE_PROCESSING: &str = "processing";
const CANDIDATE_SAVED: &str = "saved";
const CANDIDATE_SKIPPED_SAVE: &str = "skipped_save";
const CANDIDATE_FAILED: &str = "failed";

#[derive(Debug)]
struct CandidateCheckpoint {
    candidate: ArticleCandidate,
    status: String,
}

impl JobService {
    #[allow(clippy::too_many_arguments)] // service graph: each dependency is distinct
    pub fn new(
        database_path: std::path::PathBuf,
        llm_service: Arc<LlmService>,
        settings_service: Arc<SettingsService>,
        http_client: reqwest::Client,
        library_service: Arc<LibraryService>,
        kg_service: Arc<KnowledgeGraphService>,
        workspace_service: Arc<WorkspaceService>,
        contact_email: Option<String>,
        semantic_scholar_api_key: Option<String>,
        pdf_dir: std::path::PathBuf,
    ) -> Self {
        let pipeline_service = PipelineService::new(
            database_path.clone(),
            contact_email.clone(),
            semantic_scholar_api_key,
        );
        let screener = ArticleScreener::new(llm_service.clone());
        let fetcher = ContentFetcher::new(http_client.clone(), contact_email, pdf_dir);
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
                 SET status = 'interrupted',
                     error_message = 'backend stopped while this job was running'
                 WHERE status = 'running'",
                [],
            )?;
            Ok::<_, rusqlite::Error>(updated as u64)
        })
        .await
    }

    pub async fn list_interrupted_jobs(
        &self,
        workspace_id: i64,
    ) -> Result<Vec<JobRunResponse>, AppError> {
        let database_path = self.database_path.clone();
        run_blocking(move || {
            let conn = crate::db::open_connection(&*database_path)?;
            let mut stmt = conn.prepare(&format!(
                "SELECT {JOB_RUN_COLUMNS}
                 FROM job_runs
                 WHERE workspace_id = ?1 AND status = 'interrupted'
                 ORDER BY requested_at DESC",
            ))?;
            let rows = stmt.query_map(params![workspace_id], map_job_row)?;
            rows.collect::<Result<Vec<_>, _>>()
                .context("failed to list interrupted job runs")
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
                params![
                    run_id_for_insert,
                    source_for_insert,
                    days_back,
                    workspace_id
                ],
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
            false,
        );

        // Reset the per-workspace cadence clock: any gather (manual or auto)
        // counts as "last gathered now".
        if workspace_id > 0 {
            let _ = self
                .workspace_service
                .touch_last_gathered(workspace_id)
                .await;
        }

        Ok(queued_job)
    }

    pub async fn resume_job(
        &self,
        run_id: &str,
        workspace_id: i64,
    ) -> Result<JobRunResponse, AppError> {
        let run = self.get_job(run_id).await?.run;
        if run.status != "interrupted" {
            return Err(AppError::Conflict(format!(
                "Run {} is {} and cannot be resumed",
                run.run_id, run.status
            )));
        }
        if run.source != "all" && !is_gather_source(&run.source) {
            return Err(AppError::BadRequest(format!(
                "Run {} has invalid source {}",
                run.run_id, run.source
            )));
        }
        if let Some((other_run_id, running_source)) = self.find_conflict(&run.source).await? {
            return Err(AppError::Conflict(format!(
                "pipeline already running for {running_source} (run_id: {other_run_id})"
            )));
        }

        let database_path = self.database_path.clone();
        let run_id_owned = run.run_id.clone();
        let resumed = run_blocking(move || {
            let conn = crate::db::open_connection(&*database_path)?;
            conn.execute(
                "UPDATE job_runs
                 SET status = 'queued',
                     finished_at = NULL,
                     error_message = NULL
                 WHERE id = ?1 AND status = 'interrupted'",
                [run_id_owned.as_str()],
            )?;
            conn.execute(
                "INSERT INTO job_events (run_id, event_type, payload_json)
                 VALUES (?1, 'resume_requested', ?2)",
                params![
                    run_id_owned.as_str(),
                    serde_json::json!({ "status": "queued" }).to_string()
                ],
            )?;
            conn.query_row(
                &format!(
                    "SELECT {JOB_RUN_COLUMNS}
                     FROM job_runs
                     WHERE id = ?1"
                ),
                [run_id_owned.as_str()],
                map_job_row,
            )
            .context("failed to fetch resumed job")
        })
        .await?;

        self.spawn_worker(
            resumed.run_id.clone(),
            resumed.source.clone(),
            resumed.days_back,
            workspace_id,
            true,
        );

        Ok(resumed)
    }

    pub async fn mark_interrupted_failed(&self, run_id: &str) -> Result<JobRunResponse, AppError> {
        let database_path = self.database_path.clone();
        let run_id = run_id.to_string();
        task::spawn_blocking(move || {
            let conn = crate::db::open_connection(&*database_path)?;
            let updated = conn.execute(
                "UPDATE job_runs
                 SET status = 'failed',
                     finished_at = COALESCE(finished_at, datetime('now')),
                     current_item = NULL,
                     current_step = 'interrupted',
                     error_message = COALESCE(error_message, 'interrupted run was marked failed')
                 WHERE id = ?1 AND status = 'interrupted'",
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
                            "Run {run_id} is {status} and cannot be marked failed"
                        ));
                    }
                }
            }

            conn.execute(
                "INSERT INTO job_events (run_id, event_type, payload_json)
                 VALUES (?1, 'marked_failed', ?2)",
                params![
                    run_id.as_str(),
                    serde_json::json!({ "status": "failed" }).to_string()
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
            .context("failed to fetch marked failed job")
        })
        .await
        .map_err(|error| AppError::Internal(error.to_string()))?
        .map_err(not_found_or_conflict_to_app_error)
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

    fn spawn_worker(
        &self,
        run_id: String,
        source: String,
        days_back: i32,
        workspace_id: i64,
        resume: bool,
    ) {
        let service = self.clone();
        tokio::spawn(async move {
            if let Err(error) = service
                .run_job(run_id.clone(), source, days_back, workspace_id, resume)
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
        resume: bool,
    ) -> Result<(), AppError> {
        if self.is_cancelled(&run_id).await? {
            return Ok(());
        }

        let resume_snapshot = if resume {
            Some(self.get_job(&run_id).await?.run)
        } else {
            None
        };

        self.mark_running(&run_id).await?;
        self.append_event(
            &run_id,
            if resume { "resumed" } else { "started" },
            serde_json::json!({ "source": source, "days_back": days_back }),
        )
        .await?;

        // Per-workspace research context, loaded from the registry (meta DB).
        let context = self
            .workspace_service
            .research_context(workspace_id)
            .await
            .unwrap_or_default();

        let sources = if source == "all" {
            GATHER_SOURCE_IDS.to_vec()
        } else {
            vec![source.as_str()]
        };

        let completed_sources = if resume {
            self.completed_sources(&run_id).await?
        } else {
            HashSet::new()
        };
        let mut counters = resume_snapshot
            .as_ref()
            .map(counters_from_job)
            .unwrap_or_default();
        let mut last_error = resume_snapshot.and_then(|run| run.error_message);
        // The same paper often arrives from several sources under different
        // uids (arXiv + Crossref + OpenAlex). Track DOI/title keys across the
        // whole run so only the first sighting is screened and processed.
        let mut seen_dois = std::collections::HashSet::<String>::new();
        let mut seen_titles = std::collections::HashSet::<String>::new();

        for (source_index, current_source) in sources.iter().enumerate() {
            if self.is_cancelled(&run_id).await? {
                return Ok(());
            }

            if resume && completed_sources.contains(*current_source) {
                self.add_seen_candidates_for_source(
                    &run_id,
                    current_source,
                    &mut seen_dois,
                    &mut seen_titles,
                )
                .await?;
                continue;
            }

            let should_continue = self
                .process_source(
                    &run_id,
                    &source,
                    current_source,
                    source_index,
                    days_back,
                    workspace_id,
                    &context,
                    &mut counters,
                    &mut last_error,
                    &mut seen_dois,
                    &mut seen_titles,
                    resume,
                )
                .await?;
            if !should_continue {
                return Ok(());
            }
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
                    let event_type = if response.total_entities > 0 {
                        "wiki_compile_started"
                    } else {
                        "wiki_compile_skipped"
                    };
                    self.append_event(
                        &run_id,
                        event_type,
                        serde_json::json!({
                            "status": response.status,
                            "message": response.message,
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

    #[allow(clippy::too_many_arguments)]
    async fn process_source(
        &self,
        run_id: &str,
        requested_source: &str,
        current_source: &str,
        source_index: usize,
        days_back: i32,
        workspace_id: i64,
        context: &WorkspaceResearchContext,
        counters: &mut JobCounters,
        last_error: &mut Option<String>,
        seen_dois: &mut HashSet<String>,
        seen_titles: &mut HashSet<String>,
        resume: bool,
    ) -> Result<bool, AppError> {
        self.add_seen_candidates_for_source(run_id, current_source, seen_dois, seen_titles)
            .await?;

        let mut checkpoints = self.load_source_candidates(run_id, current_source).await?;
        if checkpoints.is_empty() {
            self.update_progress(
                run_id,
                &format!("listing:{current_source}"),
                Some(current_source),
                *counters,
            )
            .await?;

            let candidates = match self
                .pipeline_service
                .list_source(current_source, days_back, context)
                .await
            {
                Ok(candidates) => candidates,
                Err(error) => {
                    counters.errors += 1;
                    let message = format!("{current_source} listing failed: {error}");
                    *last_error = Some(message.clone());
                    self.append_event(
                        run_id,
                        "source_failed",
                        serde_json::json!({
                            "source": current_source,
                            "step": "listing",
                            "error": message,
                        }),
                    )
                    .await?;
                    if requested_source == "all" {
                        return Ok(true);
                    }
                    self.finish_run(run_id, "failed", *counters, last_error.as_deref())
                        .await?;
                    return Ok(false);
                }
            };

            let found = candidates.len() as i32;
            counters.found += found;
            self.persist_listed_candidates(run_id, current_source, source_index, &candidates)
                .await?;
            self.update_progress(
                run_id,
                &format!("listing:{current_source}"),
                Some(current_source),
                *counters,
            )
            .await?;
            self.append_event(
                run_id,
                "listed",
                serde_json::json!({
                    "source": current_source,
                    "candidates_found": found,
                }),
            )
            .await?;
            checkpoints = self.load_source_candidates(run_id, current_source).await?;
        } else if resume {
            self.append_event(
                run_id,
                "source_resume_checkpoint",
                serde_json::json!({
                    "source": current_source,
                    "candidates_loaded": checkpoints.len(),
                }),
            )
            .await?;
        }

        if self.is_cancelled(run_id).await? {
            return Ok(false);
        }

        let listed = checkpoints
            .iter()
            .filter(|checkpoint| checkpoint.status == CANDIDATE_LISTED)
            .map(|checkpoint| checkpoint.candidate.clone())
            .collect::<Vec<_>>();
        let mut dedup_skipped = 0_i32;
        let mut cross_source_skipped = 0_i32;
        if !listed.is_empty() {
            let uids = listed.iter().map(ArticleCandidate::uid).collect::<Vec<_>>();
            let existing = self
                .pipeline_service
                .check_duplicates_batch(&uids)
                .await
                .unwrap_or_default();
            let known = self
                .pipeline_service
                .check_known_duplicates(&listed, workspace_id)
                .await
                .unwrap_or_default();

            for candidate in listed {
                let uid = candidate.uid();
                if existing.contains(&uid) || known.contains(&uid) {
                    dedup_skipped += 1;
                    self.set_candidate_status(run_id, &uid, CANDIDATE_SKIPPED_DUPLICATE, None)
                        .await?;
                    continue;
                }
                if candidate_seen(&candidate, seen_dois, seen_titles) {
                    cross_source_skipped += 1;
                    self.set_candidate_status(run_id, &uid, CANDIDATE_SKIPPED_CROSS_SOURCE, None)
                        .await?;
                    continue;
                }
                remember_candidate(&candidate, seen_dois, seen_titles);
                self.set_candidate_status(run_id, &uid, CANDIDATE_DEDUPED, None)
                    .await?;
            }
            counters.skipped += dedup_skipped + cross_source_skipped;
            self.update_progress(
                run_id,
                &format!("dedupe:{current_source}"),
                Some(current_source),
                *counters,
            )
            .await?;
        }

        if self.is_cancelled(run_id).await? {
            return Ok(false);
        }

        checkpoints = self.load_source_candidates(run_id, current_source).await?;
        let to_screen = checkpoints
            .iter()
            .filter(|checkpoint| checkpoint.status == CANDIDATE_DEDUPED)
            .map(|checkpoint| checkpoint.candidate.clone())
            .collect::<Vec<_>>();
        if !to_screen.is_empty() {
            self.update_progress(
                run_id,
                &format!("screening:{current_source}"),
                Some(current_source),
                *counters,
            )
            .await?;
            let relevant = self.screener.filter_relevant(&to_screen, context).await;
            let relevant_uids = relevant
                .iter()
                .map(ArticleCandidate::uid)
                .collect::<HashSet<_>>();
            for candidate in &to_screen {
                let uid = candidate.uid();
                let status = if relevant_uids.contains(&uid) {
                    CANDIDATE_RELEVANT
                } else {
                    CANDIDATE_SCREENED_OUT
                };
                self.set_candidate_status(run_id, &uid, status, None)
                    .await?;
            }
            counters.screened += to_screen.len() as i32;
            counters.relevant += relevant.len() as i32;
            self.update_progress(
                run_id,
                &format!("screening:{current_source}"),
                Some(current_source),
                *counters,
            )
            .await?;
            self.append_event(
                run_id,
                "screened",
                serde_json::json!({
                    "source": current_source,
                    "screened": to_screen.len(),
                    "relevant": relevant.len(),
                    "dedup_skipped": dedup_skipped,
                    "cross_source_skipped": cross_source_skipped,
                }),
            )
            .await?;
        }

        if self.is_cancelled(run_id).await? {
            return Ok(false);
        }

        checkpoints = self.load_source_candidates(run_id, current_source).await?;
        let to_process = checkpoints
            .iter()
            .filter(|checkpoint| {
                matches!(
                    checkpoint.status.as_str(),
                    CANDIDATE_RELEVANT | CANDIDATE_PROCESSING
                )
            })
            .map(|checkpoint| checkpoint.candidate.clone())
            .collect::<Vec<_>>();

        for candidate in &to_process {
            if self.is_cancelled(run_id).await? {
                return Ok(false);
            }

            let uid = candidate.uid();
            let title_snippet: String = candidate.title.chars().take(50).collect();
            self.set_candidate_status(run_id, &uid, CANDIDATE_PROCESSING, None)
                .await?;
            self.update_progress(
                run_id,
                &format!("processing:{current_source}"),
                Some(&title_snippet),
                *counters,
            )
            .await?;

            let save_result = self
                .process_single_article(candidate, counters, last_error, workspace_id, context)
                .await;

            match save_result {
                Ok(status) => {
                    self.set_candidate_status(run_id, &uid, status, None)
                        .await?;
                }
                Err(error) => {
                    counters.errors += 1;
                    let message = format!("article {uid} failed: {error}");
                    tracing::warn!("{message}");
                    *last_error = Some(message.clone());
                    self.set_candidate_status(run_id, &uid, CANDIDATE_FAILED, Some(&message))
                        .await?;
                }
            }
            self.update_progress(
                run_id,
                &format!("processing:{current_source}"),
                Some(&title_snippet),
                *counters,
            )
            .await?;
        }

        self.append_event(
            run_id,
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

        Ok(true)
    }

    async fn process_single_article(
        &self,
        candidate: &ArticleCandidate,
        counters: &mut JobCounters,
        last_error: &mut Option<String>,
        workspace_id: i64,
        context: &WorkspaceResearchContext,
    ) -> Result<&'static str, AppError> {
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
            let status = candidate_status_from_save_result(save_result);
            apply_save_counters(counters, save_result);
            return Ok(status);
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

        // 3. Save the article with the fetched full text (and stored-PDF path)
        // regardless of whether the evaluation succeeded — the text is what the
        // embedding and KG stages read from the database afterwards.
        let fetched_payload = FetchedArticleContent {
            full_text: content
                .content
                .as_text()
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(str::to_string),
            content_type: Some(content.content_type.as_str().to_string()),
            pdf_path: content
                .pdf_path
                .as_ref()
                .map(|path| path.to_string_lossy().into_owned()),
            pdf_sha256: content.pdf_sha256.clone(),
            pdf_bytes: content.pdf_bytes,
            pdf_source_url: content.pdf_source_url.clone(),
            pdf_fetch_method: content.pdf_fetch_method.clone(),
            text_extraction_status: content.text_extraction_status.clone(),
            text_extraction_error: content.text_extraction_error.clone(),
        };
        let save_result = self
            .pipeline_service
            .save_processed_candidate(
                candidate,
                evaluation.as_ref(),
                Some(fetched_payload),
                workspace_id,
            )
            .await
            .map_err(|error| AppError::Internal(error.to_string()))?;
        let status = candidate_status_from_save_result(save_result);
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
                if let Err(error) = self
                    .kg_service
                    .insert_articles_with_context(vec![uid.clone()], context.clone())
                    .await
                {
                    tracing::warn!("KG extraction failed for {uid}: {error}");
                }
            }
        }

        Ok(status)
    }

    /// Re-runs MarkItDown over an article's stored PDF and, on success, swaps
    /// the extracted markdown into `full_text` and refreshes the embedding/KG
    /// stages. Returns `false` when extraction still produces nothing.
    pub async fn re_extract_article(&self, uid: &str, workspace_id: i64) -> Result<bool, AppError> {
        let database_path = self.database_path.clone();
        let uid_owned = uid.to_string();
        let pdf_path: Option<String> = run_blocking_db(move || {
            let conn = crate::db::open_connection(&*database_path)?;
            conn.query_row(
                "SELECT pdf_path FROM haie_rev WHERE uid = ?1",
                [uid_owned.as_str()],
                |row| row.get(0),
            )
            .optional()
            .map(Option::flatten)
        })
        .await?;

        let Some(pdf_path) = pdf_path.filter(|path| !path.trim().is_empty()) else {
            return Err(AppError::BadRequest(format!(
                "article {uid} has no stored PDF to re-extract"
            )));
        };

        let markdown = self
            .fetcher
            .re_extract_stored_pdf(std::path::Path::new(&pdf_path))
            .await?
            .map(|text| text.trim().to_string())
            .filter(|text| !text.is_empty());
        let Some(markdown) = markdown else {
            return Ok(false);
        };

        let database_path = self.database_path.clone();
        let uid_owned = uid.to_string();
        run_blocking_db(move || {
            let conn = crate::db::open_connection(&*database_path)?;
            conn.execute(
                "UPDATE haie_rev
                 SET full_text = ?1,
                     content_type = 'pdf',
                     text_extraction_status = 'extracted',
                     text_extracted_at = datetime('now'),
                     text_extraction_error = NULL,
                     has_embeddings = 0,
                     has_kg_entities = 0,
                     updated_at = datetime('now')
                 WHERE uid = ?2",
                params![markdown, uid_owned],
            )?;
            Ok::<_, rusqlite::Error>(())
        })
        .await?;

        let context = self
            .workspace_service
            .research_context(workspace_id)
            .await
            .unwrap_or_default();
        let (library_enabled, kg_enabled) = self
            .settings_service
            .get_feature_flags()
            .await
            .unwrap_or((true, true));
        if library_enabled && let Err(error) = self.library_service.process_article(uid).await {
            tracing::warn!("re-embedding after re-extraction failed for {uid}: {error}");
        }
        if kg_enabled
            && let Err(error) = self
                .kg_service
                .insert_articles_with_context(vec![uid.to_string()], context)
                .await
        {
            tracing::warn!("KG refresh after re-extraction failed for {uid}: {error}");
        }

        Ok(true)
    }

    async fn completed_sources(&self, run_id: &str) -> Result<HashSet<String>, AppError> {
        let database_path = self.database_path.clone();
        let run_id = run_id.to_string();
        run_blocking(move || {
            let conn = crate::db::open_connection(&*database_path)?;
            let mut stmt = conn.prepare(
                "SELECT payload_json
                 FROM job_events
                 WHERE run_id = ?1 AND event_type = 'source_completed'",
            )?;
            let rows = stmt.query_map([run_id.as_str()], |row| row.get::<_, Option<String>>(0))?;
            let mut sources = HashSet::new();
            for row in rows {
                let Some(payload) = row? else {
                    continue;
                };
                if let Some(source) = serde_json::from_str::<serde_json::Value>(&payload)
                    .ok()
                    .and_then(|value| {
                        value
                            .get("source")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_string)
                    })
                {
                    sources.insert(source);
                }
            }
            Ok::<_, anyhow::Error>(sources)
        })
        .await
    }

    async fn persist_listed_candidates(
        &self,
        run_id: &str,
        source: &str,
        source_index: usize,
        candidates: &[ArticleCandidate],
    ) -> Result<(), AppError> {
        let database_path = self.database_path.clone();
        let run_id = run_id.to_string();
        let source = source.to_string();
        let source_index = source_index as i64;
        let rows = candidates
            .iter()
            .enumerate()
            .map(|(idx, candidate)| {
                Ok::<_, AppError>((
                    candidate.uid(),
                    idx as i64,
                    serde_json::to_string(candidate)
                        .map_err(|error| AppError::Internal(error.to_string()))?,
                ))
            })
            .collect::<Result<Vec<_>, _>>()?;

        run_blocking_db(move || {
            let mut conn = crate::db::open_connection(&*database_path)?;
            let tx = conn.transaction()?;
            {
                let mut stmt = tx.prepare(
                    "INSERT OR IGNORE INTO job_candidates
                        (run_id, uid, source, source_index, candidate_index, candidate_json, status)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'listed')",
                )?;
                for (uid, candidate_index, candidate_json) in rows {
                    stmt.execute(params![
                        run_id.as_str(),
                        uid,
                        source.as_str(),
                        source_index,
                        candidate_index,
                        candidate_json
                    ])?;
                }
            }
            tx.commit()?;
            Ok::<_, rusqlite::Error>(())
        })
        .await
    }

    async fn load_source_candidates(
        &self,
        run_id: &str,
        source: &str,
    ) -> Result<Vec<CandidateCheckpoint>, AppError> {
        let database_path = self.database_path.clone();
        let run_id = run_id.to_string();
        let source = source.to_string();
        run_blocking(move || {
            let conn = crate::db::open_connection(&*database_path)?;
            let mut stmt = conn.prepare(
                "SELECT candidate_json, status
                 FROM job_candidates
                 WHERE run_id = ?1 AND source = ?2
                 ORDER BY candidate_index ASC",
            )?;
            let rows = stmt.query_map(params![run_id.as_str(), source.as_str()], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            let mut checkpoints = Vec::new();
            for row in rows {
                let (candidate_json, status) = row?;
                let candidate = serde_json::from_str::<ArticleCandidate>(&candidate_json)
                    .with_context(|| "failed to parse checkpointed article candidate")?;
                checkpoints.push(CandidateCheckpoint { candidate, status });
            }
            Ok::<_, anyhow::Error>(checkpoints)
        })
        .await
    }

    async fn set_candidate_status(
        &self,
        run_id: &str,
        uid: &str,
        status: &str,
        error_message: Option<&str>,
    ) -> Result<(), AppError> {
        let database_path = self.database_path.clone();
        let run_id = run_id.to_string();
        let uid = uid.to_string();
        let status = status.to_string();
        let error_message = error_message.map(str::to_string);
        run_blocking_db(move || {
            let conn = crate::db::open_connection(&*database_path)?;
            conn.execute(
                "UPDATE job_candidates
                 SET status = ?3,
                     error_message = ?4,
                     updated_at = datetime('now')
                 WHERE run_id = ?1 AND uid = ?2",
                params![run_id, uid, status, error_message],
            )?;
            Ok::<_, rusqlite::Error>(())
        })
        .await
    }

    async fn add_seen_candidates_for_source(
        &self,
        run_id: &str,
        source: &str,
        seen_dois: &mut HashSet<String>,
        seen_titles: &mut HashSet<String>,
    ) -> Result<(), AppError> {
        for checkpoint in self.load_source_candidates(run_id, source).await? {
            if candidate_claims_seen_key(&checkpoint.status) {
                remember_candidate(&checkpoint.candidate, seen_dois, seen_titles);
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

fn counters_from_job(run: &JobRunResponse) -> JobCounters {
    JobCounters {
        found: run.candidates_found,
        screened: run.candidates_screened,
        relevant: run.candidates_relevant,
        fetched: run.candidates_fetched,
        evaluated: run.candidates_evaluated,
        saved: run.candidates_saved,
        embedded: run.candidates_embedded,
        skipped: run.candidates_skipped,
        errors: run.errors,
    }
}

fn candidate_status_from_save_result(result: SaveCounters) -> &'static str {
    if result.saved > 0 {
        CANDIDATE_SAVED
    } else {
        CANDIDATE_SKIPPED_SAVE
    }
}

fn candidate_claims_seen_key(status: &str) -> bool {
    !matches!(
        status,
        CANDIDATE_LISTED | CANDIDATE_SKIPPED_DUPLICATE | CANDIDATE_SKIPPED_CROSS_SOURCE
    )
}

fn candidate_seen(
    candidate: &ArticleCandidate,
    seen_dois: &HashSet<String>,
    seen_titles: &HashSet<String>,
) -> bool {
    let doi_seen = candidate
        .doi
        .as_deref()
        .map(crate::services::pipeline::normalize_doi)
        .filter(|value| !value.is_empty())
        .is_some_and(|key| seen_dois.contains(&key));
    let title_key = crate::services::pipeline::normalized_duplicate_title(&candidate.title);
    let title_seen = !title_key.is_empty() && seen_titles.contains(&title_key);
    doi_seen || title_seen
}

fn remember_candidate(
    candidate: &ArticleCandidate,
    seen_dois: &mut HashSet<String>,
    seen_titles: &mut HashSet<String>,
) {
    if let Some(doi_key) = candidate
        .doi
        .as_deref()
        .map(crate::services::pipeline::normalize_doi)
        .filter(|value| !value.is_empty())
    {
        seen_dois.insert(doi_key);
    }
    let title_key = crate::services::pipeline::normalized_duplicate_title(&candidate.title);
    if !title_key.is_empty() {
        seen_titles.insert(title_key);
    }
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
        let now = Local::now();
        let today = now.date_naive();
        let Some(today_run_naive) = today.and_hms_opt(u32::from(hour), u32::from(minute), 0) else {
            return SchedulerJob {
                id: id.to_string(),
                name: name.to_string(),
                next_run: None,
            };
        };

        let Some(today_run) = today_run_naive.and_local_timezone(Local).single() else {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(source_id: &str, title: &str, doi: Option<&str>) -> ArticleCandidate {
        ArticleCandidate {
            source: "pubmed".to_string(),
            source_id: source_id.to_string(),
            title: title.to_string(),
            summary: Some("summary".to_string()),
            first_author: "First".to_string(),
            authors: None,
            pub_date: Some("2026".to_string()),
            journal: Some("Journal".to_string()),
            doi: doi.map(str::to_string),
            url: format!("https://example.test/{source_id}"),
        }
    }

    #[test]
    fn checkpoint_candidate_json_round_trips() {
        let original = candidate("123", "Shared decision making training", Some("10.1/test"));

        let json = serde_json::to_string(&original).expect("serialize candidate");
        let restored: ArticleCandidate =
            serde_json::from_str(&json).expect("deserialize candidate");

        assert_eq!(restored.uid(), original.uid());
        assert_eq!(restored.title, original.title);
        assert_eq!(restored.doi, original.doi);
    }

    #[test]
    fn final_candidate_status_follows_save_result() {
        assert_eq!(
            candidate_status_from_save_result(SaveCounters {
                saved: 1,
                skipped: 0,
                errors: 0,
            }),
            CANDIDATE_SAVED
        );
        assert_eq!(
            candidate_status_from_save_result(SaveCounters {
                saved: 0,
                skipped: 1,
                errors: 0,
            }),
            CANDIDATE_SKIPPED_SAVE
        );
    }

    #[test]
    fn seen_keys_use_normalized_doi_and_title() {
        let first = candidate(
            "a",
            "AI coaching for SDM",
            Some("https://doi.org/10.1000/ABC"),
        );
        let same_doi = candidate("b", "Different title", Some("10.1000/abc"));
        let same_title = candidate("c", "AI coaching for SDM", None);
        let novel = candidate("d", "Virtual patient simulation", None);

        let mut seen_dois = HashSet::new();
        let mut seen_titles = HashSet::new();
        remember_candidate(&first, &mut seen_dois, &mut seen_titles);

        assert!(candidate_seen(&same_doi, &seen_dois, &seen_titles));
        assert!(candidate_seen(&same_title, &seen_dois, &seen_titles));
        assert!(!candidate_seen(&novel, &seen_dois, &seen_titles));
    }

    #[test]
    fn only_deduped_or_later_candidates_claim_seen_keys() {
        assert!(!candidate_claims_seen_key(CANDIDATE_LISTED));
        assert!(!candidate_claims_seen_key(CANDIDATE_SKIPPED_DUPLICATE));
        assert!(!candidate_claims_seen_key(CANDIDATE_SKIPPED_CROSS_SOURCE));
        assert!(candidate_claims_seen_key(CANDIDATE_DEDUPED));
        assert!(candidate_claims_seen_key(CANDIDATE_SCREENED_OUT));
        assert!(candidate_claims_seen_key(CANDIDATE_PROCESSING));
        assert!(candidate_claims_seen_key(CANDIDATE_SAVED));
    }
}
