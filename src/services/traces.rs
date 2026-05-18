use std::{path::PathBuf, sync::Arc};

use anyhow::Context;
use rusqlite::{params_from_iter, types::Value};
use tokio::task;

use crate::{
    error::{AppError, run_blocking, run_blocking_db},
    models::trace::{TraceListQuery, TraceListResponse, TraceResponse, TraceSummary},
};

#[derive(Debug)]
pub struct TraceCreate {
    pub prompt_name: String,
    pub prompt_version: Option<i64>,
    pub article_uid: Option<String>,
    pub model: String,
    pub input_text: String,
    pub output_text: Option<String>,
    pub tokens_input: Option<i64>,
    pub tokens_output: Option<i64>,
    pub latency_ms: Option<i64>,
    pub cost_usd: Option<f64>,
    pub success: bool,
    pub error_message: Option<String>,
}

#[derive(Clone)]
pub struct TraceService {
    database_path: Arc<PathBuf>,
}

impl TraceService {
    pub fn new(database_path: PathBuf) -> Self {
        Self {
            database_path: Arc::new(database_path),
        }
    }

    pub async fn list_traces(&self, query: TraceListQuery) -> Result<TraceListResponse, AppError> {
        let database_path = self.database_path.clone();

        run_blocking(move || {
            let conn = crate::db::open_connection(&*database_path)?;
            let page = query.page.max(1);
            let page_size = query.page_size.clamp(1, 100);
            let (where_clause, base_params) = trace_where_clause(&query);

            let count_sql = format!("SELECT COUNT(*) FROM prompt_traces{where_clause}");
            let total: i64 =
                conn.query_row(&count_sql, params_from_iter(base_params.iter()), |row| {
                    row.get(0)
                })?;

            let mut params = base_params.clone();
            params.push(Value::Integer(i64::from(page_size)));
            params.push(Value::Integer(i64::from((page - 1) * page_size)));

            let sql = format!(
                "
                SELECT id, prompt_name, prompt_version, article_uid, model, input_text, output_text,
                       tokens_input, tokens_output, tokens_total, latency_ms, cost_usd, success,
                       error_message, created_at
                FROM prompt_traces
                {where_clause}
                ORDER BY COALESCE(created_at, '') DESC, id DESC
                LIMIT ? OFFSET ?
                "
            );

            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map(params_from_iter(params.iter()), map_trace_row)?;
            let items = rows.collect::<Result<Vec<_>, _>>()?;
            let pages = if total > 0 {
                ((total as f64) / f64::from(page_size)).ceil() as u32
            } else {
                1
            };

            Ok(TraceListResponse {
                items,
                total,
                page,
                page_size,
                pages,
            })
        })
        .await
    }

    pub async fn get_trace(&self, trace_id: i64) -> Result<TraceResponse, AppError> {
        let database_path = self.database_path.clone();

        task::spawn_blocking(move || {
            let conn = crate::db::open_connection(&*database_path)?;
            let mut stmt = conn.prepare(
                "
                SELECT id, prompt_name, prompt_version, article_uid, model, input_text, output_text,
                       tokens_input, tokens_output, tokens_total, latency_ms, cost_usd, success,
                       error_message, created_at
                FROM prompt_traces
                WHERE id = ?1
                ",
            )?;
            let trace = stmt
                .query_row([trace_id], map_trace_row)
                .map_err(|error| match error {
                    rusqlite::Error::QueryReturnedNoRows => {
                        anyhow::anyhow!("Trace {trace_id} not found")
                    }
                    other => anyhow::Error::new(other),
                })?;

            Ok::<_, anyhow::Error>(trace)
        })
        .await
        .map_err(|error| AppError::Internal(error.to_string()))?
        .map_err(|error| {
            if error.to_string().contains("not found") {
                AppError::NotFound(error.to_string())
            } else {
                AppError::Internal(error.to_string())
            }
        })
    }

    pub async fn get_summary(&self) -> Result<Vec<TraceSummary>, AppError> {
        let database_path = self.database_path.clone();

        run_blocking(move || {
            let conn = crate::db::open_connection(&*database_path)?;
            let mut stmt = conn.prepare(
                "
                SELECT prompt_name,
                       COUNT(*) AS total,
                       COALESCE(SUM(CASE WHEN success = 1 THEN 1 ELSE 0 END), 0) AS successful,
                       AVG(latency_ms) AS avg_latency,
                       SUM(tokens_total) AS total_tokens,
                       SUM(cost_usd) AS total_cost
                FROM prompt_traces
                GROUP BY prompt_name
                ORDER BY total DESC, prompt_name ASC
                ",
            )?;
            let rows = stmt.query_map([], |row| {
                let total: i64 = row.get(1)?;
                let successful: i64 = row.get::<_, Option<i64>>(2)?.unwrap_or(0);
                let total_cost_cents = row.get::<_, Option<f64>>(5)?;
                Ok(TraceSummary {
                    prompt_name: row.get(0)?,
                    total_executions: total,
                    successful_executions: successful,
                    failed_executions: total - successful,
                    avg_latency_ms: row.get(3)?,
                    total_tokens: row.get(4)?,
                    total_cost_usd: total_cost_cents.map(|value| value / 100.0),
                })
            })?;

            rows.collect::<Result<Vec<_>, _>>()
                .context("failed to load trace summary")
        })
        .await
    }

    pub async fn record_trace(&self, trace: TraceCreate) -> Result<(), AppError> {
        let database_path = self.database_path.clone();

        run_blocking_db(move || {
            let conn = crate::db::open_connection(&*database_path)?;
            conn.execute(
                "
                INSERT INTO prompt_traces (
                    prompt_name, prompt_version, article_uid, model, input_text, output_text,
                    tokens_input, tokens_output, tokens_total, latency_ms, cost_usd, success,
                    error_message
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
                ",
                rusqlite::params![
                    trace.prompt_name,
                    trace.prompt_version,
                    trace.article_uid,
                    trace.model,
                    trace.input_text,
                    trace.output_text,
                    trace.tokens_input,
                    trace.tokens_output,
                    match (trace.tokens_input, trace.tokens_output) {
                        (Some(input), Some(output)) => Some(input + output),
                        (Some(input), None) => Some(input),
                        (None, Some(output)) => Some(output),
                        (None, None) => None,
                    },
                    trace.latency_ms,
                    trace.cost_usd.map(|value| value * 100.0),
                    trace.success,
                    trace.error_message,
                ],
            )?;

            Ok(())
        })
        .await
    }
}

fn trace_where_clause(query: &TraceListQuery) -> (String, Vec<Value>) {
    let mut conditions = Vec::new();
    let mut params = Vec::new();

    if let Some(prompt_name) = query.prompt_name.as_ref().filter(|value| !value.is_empty()) {
        conditions.push("prompt_name = ?".to_string());
        params.push(Value::Text(prompt_name.clone()));
    }
    if let Some(article_uid) = query.article_uid.as_ref().filter(|value| !value.is_empty()) {
        conditions.push("article_uid = ?".to_string());
        params.push(Value::Text(article_uid.clone()));
    }
    if let Some(model) = query.model.as_ref().filter(|value| !value.is_empty()) {
        conditions.push("model = ?".to_string());
        params.push(Value::Text(model.clone()));
    }
    if let Some(success) = query.success {
        conditions.push("success = ?".to_string());
        params.push(Value::Integer(if success { 1 } else { 0 }));
    }

    if conditions.is_empty() {
        (String::new(), params)
    } else {
        (format!(" WHERE {}", conditions.join(" AND ")), params)
    }
}

fn map_trace_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<TraceResponse> {
    Ok(TraceResponse {
        id: row.get(0)?,
        prompt_name: row.get(1)?,
        prompt_version: row.get(2)?,
        article_uid: row.get(3)?,
        model: row.get(4)?,
        input_text: row.get(5)?,
        output_text: row.get(6)?,
        tokens_input: row.get(7)?,
        tokens_output: row.get(8)?,
        tokens_total: row.get(9)?,
        latency_ms: row.get(10)?,
        cost_usd: row.get::<_, Option<f64>>(11)?.map(|value| value / 100.0),
        success: row.get::<_, Option<bool>>(12)?.unwrap_or(false),
        error_message: row.get(13)?,
        created_at: row.get(14)?,
    })
}
