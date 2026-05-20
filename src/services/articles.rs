use std::sync::Arc;

use anyhow::Context;
use chrono::{Duration, NaiveDate, Utc};
use rusqlite::{params_from_iter, types::Value};
use tokio::task;

use crate::{
    error::{AppError, run_blocking},
    models::article::{
        ArticleListQuery, ArticleListResponse, ArticleResponse, ArticleStats, ArticleUpdate,
        DailyCount, DailyStatsResponse,
    },
};

const ARTICLE_COLUMNS: &str = r#"
    uid, title, url, category, first_author, authors, pub_date, journal,
    ai_tech, clinical_domain, ethics_framework, primary_issue, key_stakeholders,
    practical_impl, secondary_issues, key_argument, main_findings, normative_claims,
    limitations, theoretical_strengths, theoretical_weaknesses, empirical_strengths,
    empirical_weaknesses, byline_summary, why_it_matters, scholarly_rigor, novelty,
    relevance_score, practical_impact, interdisciplinary, critical_concerns,
    total_score, priority, reg_date, created_at, updated_at
"#;

#[derive(Clone)]
pub struct ArticleService {
    database_path: Arc<std::path::PathBuf>,
}

#[derive(Debug)]
pub struct ArticleProcessingContext {
    pub uid: String,
    pub category: Option<String>,
    pub title: Option<String>,
    pub byline_summary: Option<String>,
    pub full_text: Option<String>,
}

impl ArticleService {
    pub fn new(database_path: std::path::PathBuf) -> Self {
        Self {
            database_path: Arc::new(database_path),
        }
    }

    pub async fn list_articles(
        &self,
        query: ArticleListQuery,
        workspace_id: Option<i64>,
    ) -> Result<ArticleListResponse, AppError> {
        let database_path = self.database_path.clone();

        run_blocking(move || {
            let conn = crate::db::open_connection(&*database_path)?;
            let page = query.page.max(1);
            let page_size = query.page_size.clamp(1, 100);
            let (where_clause, base_params) = article_where_clause(&query, workspace_id);

            let count_sql = format!("SELECT COUNT(*) FROM haie_rev{where_clause}");
            let total: i64 =
                conn.query_row(&count_sql, params_from_iter(base_params.iter()), |row| {
                    row.get(0)
                })?;

            let mut items_params = base_params.clone();
            items_params.push(Value::Integer(i64::from(page_size)));
            items_params.push(Value::Integer(i64::from((page - 1) * page_size)));

            let sql = format!(
                "SELECT {ARTICLE_COLUMNS} FROM haie_rev{where_clause}
                 ORDER BY COALESCE(reg_date, '') DESC, COALESCE(total_score, -999) DESC
                 LIMIT ? OFFSET ?"
            );

            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map(params_from_iter(items_params.iter()), map_article_row)?;
            let items = rows.collect::<Result<Vec<_>, _>>()?;
            let pages = if total > 0 {
                ((total as f64) / f64::from(page_size)).ceil() as u32
            } else {
                1
            };

            Ok::<_, anyhow::Error>(ArticleListResponse {
                items,
                total,
                page,
                page_size,
                pages,
            })
        })
        .await
    }

    pub async fn get_article(&self, uid: &str) -> Result<ArticleResponse, AppError> {
        let database_path = self.database_path.clone();
        let uid = uid.to_string();

        task::spawn_blocking(move || {
            let conn = crate::db::open_connection(&*database_path)?;
            let sql = format!("SELECT {ARTICLE_COLUMNS} FROM haie_rev WHERE uid = ?1");

            let mut stmt = conn.prepare(&sql)?;
            let article = stmt
                .query_row([uid.as_str()], map_article_row)
                .map_err(|error| match error {
                    rusqlite::Error::QueryReturnedNoRows => {
                        anyhow::anyhow!("Article {uid} not found")
                    }
                    other => anyhow::Error::new(other),
                })?;

            Ok::<_, anyhow::Error>(article)
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

    pub async fn update_article(
        &self,
        uid: &str,
        update: ArticleUpdate,
    ) -> Result<ArticleResponse, AppError> {
        let database_path = self.database_path.clone();
        let uid = uid.to_string();

        task::spawn_blocking(move || {
            let conn = crate::db::open_connection(&*database_path)?;
            let (assignments, params) = build_article_update(&update.fields)?;

            if assignments.is_empty() {
                return Err(anyhow::anyhow!("No supported fields provided for update"));
            }

            let mut sql_params = params;
            sql_params.push(Value::Text(uid.clone()));

            let sql = format!(
                "UPDATE haie_rev SET {}, updated_at = datetime('now') WHERE uid = ?",
                assignments.join(", ")
            );
            let updated = conn.execute(&sql, params_from_iter(sql_params.iter()))?;
            if updated == 0 {
                return Err(anyhow::anyhow!("Article {uid} not found"));
            }

            let sql = format!("SELECT {ARTICLE_COLUMNS} FROM haie_rev WHERE uid = ?1");
            let mut stmt = conn.prepare(&sql)?;
            let article = stmt.query_row([uid.as_str()], map_article_row)?;

            Ok::<_, anyhow::Error>(article)
        })
        .await
        .map_err(|error| AppError::Internal(error.to_string()))?
        .map_err(|error| map_anyhow_not_found(error, "Article"))
    }

    pub async fn get_processing_context(
        &self,
        uid: &str,
    ) -> Result<ArticleProcessingContext, AppError> {
        let database_path = self.database_path.clone();
        let uid = uid.to_string();

        task::spawn_blocking(move || {
            let conn = crate::db::open_connection(&*database_path)?;
            let mut stmt = conn.prepare(
                "SELECT uid, category, title, byline_summary, full_text
                 FROM haie_rev
                 WHERE uid = ?1",
            )?;

            let article = stmt
                .query_row([uid.as_str()], |row| {
                    Ok(ArticleProcessingContext {
                        uid: row.get(0)?,
                        category: row.get(1)?,
                        title: row.get(2)?,
                        byline_summary: row.get(3)?,
                        full_text: row.get(4)?,
                    })
                })
                .map_err(|error| match error {
                    rusqlite::Error::QueryReturnedNoRows => {
                        anyhow::anyhow!("Article {uid} not found")
                    }
                    other => anyhow::Error::new(other),
                })?;

            Ok::<_, anyhow::Error>(article)
        })
        .await
        .map_err(|error| AppError::Internal(error.to_string()))?
        .map_err(|error| map_anyhow_not_found(error, "Article"))
    }

    pub async fn delete_article(&self, uid: &str) -> Result<(), AppError> {
        let database_path = self.database_path.clone();
        let uid = uid.to_string();

        task::spawn_blocking(move || {
            let conn = crate::db::open_connection(&*database_path)?;
            let deleted = conn.execute("DELETE FROM haie_rev WHERE uid = ?1", [uid.as_str()])?;
            if deleted == 0 {
                return Err(anyhow::anyhow!("Article {uid} not found"));
            }
            Ok::<_, anyhow::Error>(())
        })
        .await
        .map_err(|error| AppError::Internal(error.to_string()))?
        .map_err(|error| map_anyhow_not_found(error, "Article"))
    }

    pub async fn get_stats(&self, workspace_id: Option<i64>) -> Result<ArticleStats, AppError> {
        let database_path = self.database_path.clone();
        run_blocking(move || {
            let conn = crate::db::open_connection(&*database_path)?;
            let today = Utc::now().date_naive();
            let week_ago = today - Duration::days(7);

            let total_articles: i64 = {
                let mut sql = String::from("SELECT COUNT(uid) FROM haie_rev");
                let mut params: Vec<Value> = Vec::new();
                append_ws(&mut sql, &mut params, workspace_id, false);
                conn.query_row(&sql, params_from_iter(params.iter()), |row| row.get(0))?
            };
            let this_week: i64 = {
                let mut sql = String::from("SELECT COUNT(uid) FROM haie_rev WHERE reg_date >= ?");
                let mut params: Vec<Value> = vec![Value::Text(week_ago.to_string())];
                append_ws(&mut sql, &mut params, workspace_id, true);
                conn.query_row(&sql, params_from_iter(params.iter()), |row| row.get(0))?
            };
            let tier1_count: i64 = {
                let mut sql =
                    String::from("SELECT COUNT(uid) FROM haie_rev WHERE priority = 'Tier1'");
                let mut params: Vec<Value> = Vec::new();
                append_ws(&mut sql, &mut params, workspace_id, true);
                conn.query_row(&sql, params_from_iter(params.iter()), |row| row.get(0))?
            };
            let pending_review: i64 = {
                let mut sql = String::from(
                    "SELECT COUNT(uid) FROM haie_rev WHERE (priority IS NULL OR priority = '')",
                );
                let mut params: Vec<Value> = Vec::new();
                append_ws(&mut sql, &mut params, workspace_id, true);
                conn.query_row(&sql, params_from_iter(params.iter()), |row| row.get(0))?
            };

            Ok::<_, anyhow::Error>(ArticleStats {
                total_articles,
                this_week,
                tier1_count,
                pending_review,
            })
        })
        .await
    }

    pub async fn get_daily_stats(
        &self,
        days: u32,
        workspace_id: Option<i64>,
    ) -> Result<DailyStatsResponse, AppError> {
        let database_path = self.database_path.clone();
        run_blocking(move || {
            let conn = crate::db::open_connection(&*database_path)?;
            let days = days.clamp(1, 90);
            let today = Utc::now().date_naive();
            let start_date = today - Duration::days(i64::from(days.saturating_sub(1)));

            let mut sql = String::from(
                "SELECT reg_date, COUNT(uid) FROM haie_rev WHERE reg_date >= ?",
            );
            let mut params: Vec<Value> = vec![Value::Text(start_date.to_string())];
            append_ws(&mut sql, &mut params, workspace_id, true);
            sql.push_str(" GROUP BY reg_date ORDER BY reg_date");

            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map(params_from_iter(params.iter()), |row| {
                let reg_date: Option<String> = row.get(0)?;
                let count: i64 = row.get(1)?;
                Ok((reg_date, count))
            })?;

            let mut count_map = std::collections::BTreeMap::new();
            for row in rows {
                let (date_str, count) = row?;
                if let Some(date_str) = date_str {
                    if let Ok(date) = NaiveDate::parse_from_str(&date_str, "%Y-%m-%d") {
                        count_map.insert(date, count);
                    }
                }
            }

            let mut counts = Vec::new();
            let mut total = 0_i64;
            let mut current = start_date;
            while current <= today {
                let count = *count_map.get(&current).unwrap_or(&0);
                total += count;
                counts.push(DailyCount {
                    date: current.to_string(),
                    count,
                });
                current += Duration::days(1);
            }

            Ok::<_, anyhow::Error>(DailyStatsResponse {
                days: counts,
                total,
            })
        })
        .await
    }

    pub async fn get_recent_articles(
        &self,
        days: u32,
        limit: u32,
        min_score: Option<i32>,
        workspace_id: Option<i64>,
    ) -> Result<Vec<ArticleResponse>, AppError> {
        let database_path = self.database_path.clone();
        run_blocking(move || {
            let conn = crate::db::open_connection(&*database_path)?;
            let threshold = (Utc::now().date_naive()
                - Duration::days(i64::from(days.clamp(1, 30))))
            .to_string();

            let mut sql = format!("SELECT {ARTICLE_COLUMNS} FROM haie_rev WHERE reg_date >= ?");
            let mut params = vec![Value::Text(threshold)];
            append_ws(&mut sql, &mut params, workspace_id, true);
            if let Some(min_score) = min_score {
                sql.push_str(" AND total_score >= ?");
                params.push(Value::Integer(i64::from(min_score)));
            }
            sql.push_str(" ORDER BY COALESCE(total_score, -999) DESC LIMIT ?");
            params.push(Value::Integer(i64::from(limit.clamp(1, 50))));

            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map(params_from_iter(params.iter()), map_article_row)?;
            rows.collect::<Result<Vec<_>, _>>()
                .context("failed to query recent articles")
        })
        .await
    }

    pub async fn get_top_articles(
        &self,
        days: u32,
        limit: u32,
        workspace_id: Option<i64>,
    ) -> Result<Vec<ArticleResponse>, AppError> {
        let database_path = self.database_path.clone();
        run_blocking(move || {
            let conn = crate::db::open_connection(&*database_path)?;
            let threshold = (Utc::now().date_naive()
                - Duration::days(i64::from(days.clamp(1, 30))))
            .to_string();
            let mut sql = format!("SELECT {ARTICLE_COLUMNS} FROM haie_rev WHERE reg_date >= ?");
            let mut params = vec![Value::Text(threshold)];
            append_ws(&mut sql, &mut params, workspace_id, true);
            sql.push_str(" ORDER BY COALESCE(total_score, -999) DESC LIMIT ?");
            params.push(Value::Integer(i64::from(limit.clamp(1, 20))));

            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map(params_from_iter(params.iter()), map_article_row)?;
            rows.collect::<Result<Vec<_>, _>>()
                .context("failed to query top articles")
        })
        .await
    }

    pub async fn get_articles_by_uids(
        &self,
        uids: &[String],
    ) -> Result<Vec<ArticleResponse>, AppError> {
        let database_path = self.database_path.clone();
        let uids = uids.to_vec();

        run_blocking(move || {
            let conn = crate::db::open_connection(&*database_path)?;
            if uids.is_empty() {
                return Ok::<_, anyhow::Error>(Vec::new());
            }
            let placeholders = vec!["?"; uids.len()].join(", ");
            let sql =
                format!("SELECT {ARTICLE_COLUMNS} FROM haie_rev WHERE uid IN ({placeholders})");
            let params: Vec<Value> = uids.into_iter().map(Value::Text).collect();
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map(params_from_iter(params.iter()), map_article_row)?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(anyhow::Error::from)
        })
        .await
    }

    pub async fn save_fetched_abstract(
        &self,
        uid: &str,
        abstract_text: &str,
    ) -> Result<ArticleResponse, AppError> {
        let database_path = self.database_path.clone();
        let uid = uid.to_string();
        let abstract_text = abstract_text.to_string();

        task::spawn_blocking(move || {
            let conn = crate::db::open_connection(&*database_path)?;
            let updated = conn.execute(
                "UPDATE haie_rev
                 SET full_text = ?1,
                     content_type = 'abstract_only',
                     updated_at = datetime('now')
                 WHERE uid = ?2",
                [&abstract_text, uid.as_str()],
            )?;

            if updated == 0 {
                return Err(anyhow::anyhow!("Article {uid} not found"));
            }

            let sql = format!("SELECT {ARTICLE_COLUMNS} FROM haie_rev WHERE uid = ?1");
            let mut stmt = conn.prepare(&sql)?;
            let article = stmt.query_row([uid.as_str()], map_article_row)?;

            Ok::<_, anyhow::Error>(article)
        })
        .await
        .map_err(|error| AppError::Internal(error.to_string()))?
        .map_err(|error| map_anyhow_not_found(error, "Article"))
    }

    pub async fn apply_reevaluation(
        &self,
        uid: &str,
        evaluation: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<ArticleResponse, AppError> {
        let database_path = self.database_path.clone();
        let uid = uid.to_string();
        let evaluation = evaluation.clone();

        task::spawn_blocking(move || {
            let conn = crate::db::open_connection(&*database_path)?;

            let text_fields = [
                "ai_tech",
                "clinical_domain",
                "ethics_framework",
                "primary_issue",
                "key_stakeholders",
                "practical_impl",
                "secondary_issues",
                "key_argument",
                "main_findings",
                "normative_claims",
                "limitations",
                "theoretical_strengths",
                "theoretical_weaknesses",
                "empirical_strengths",
                "empirical_weaknesses",
                "byline_summary",
                "why_it_matters",
                "priority",
            ];
            let score_fields = [
                "scholarly_rigor",
                "novelty",
                "relevance_score",
                "practical_impact",
                "interdisciplinary",
                "critical_concerns",
                "total_score",
            ];

            let mut assignments = Vec::new();
            let mut params = Vec::new();

            for field in text_fields {
                match evaluation.get(field) {
                    Some(serde_json::Value::String(value)) if !value.is_empty() => {
                        assignments.push(format!("{field} = ?"));
                        params.push(Value::Text(value.clone()));
                    }
                    _ => {}
                }
            }

            for field in score_fields {
                if let Some(value) = evaluation.get(field).and_then(json_number_to_i64) {
                    assignments.push(format!("{field} = ?"));
                    params.push(Value::Integer(value));
                }
            }

            if assignments.is_empty() {
                return Err(anyhow::anyhow!(
                    "Re-evaluation produced no updatable fields"
                ));
            }

            params.push(Value::Text(uid.clone()));
            let sql = format!(
                "UPDATE haie_rev SET {}, updated_at = datetime('now') WHERE uid = ?",
                assignments.join(", ")
            );
            let updated = conn.execute(&sql, params_from_iter(params.iter()))?;

            if updated == 0 {
                return Err(anyhow::anyhow!("Article {uid} not found"));
            }

            let sql = format!("SELECT {ARTICLE_COLUMNS} FROM haie_rev WHERE uid = ?1");
            let mut stmt = conn.prepare(&sql)?;
            let article = stmt.query_row([uid.as_str()], map_article_row)?;

            Ok::<_, anyhow::Error>(article)
        })
        .await
        .map_err(|error| AppError::Internal(error.to_string()))?
        .map_err(|error| map_anyhow_not_found(error, "Article"))
    }
}

/// Appends a workspace scope to a query when an id is provided. `has_where`
/// chooses `AND` vs `WHERE` depending on whether the SQL already filters.
fn append_ws(sql: &mut String, params: &mut Vec<Value>, workspace_id: Option<i64>, has_where: bool) {
    if let Some(id) = workspace_id {
        sql.push_str(if has_where {
            " AND workspace_id = ?"
        } else {
            " WHERE workspace_id = ?"
        });
        params.push(Value::Integer(id));
    }
}

fn json_number_to_i64(value: &serde_json::Value) -> Option<i64> {
    match value {
        serde_json::Value::Number(number) => number
            .as_i64()
            .or_else(|| number.as_f64().map(|v| v as i64)),
        _ => None,
    }
}

fn build_article_update(
    fields: &serde_json::Map<String, serde_json::Value>,
) -> Result<(Vec<String>, Vec<Value>), anyhow::Error> {
    let mut assignments = Vec::new();
    let mut params = Vec::new();

    for (key, value) in fields {
        let column = match key.as_str() {
            "title"
            | "ai_tech"
            | "clinical_domain"
            | "ethics_framework"
            | "primary_issue"
            | "secondary_issues"
            | "key_stakeholders"
            | "practical_impl"
            | "key_argument"
            | "main_findings"
            | "normative_claims"
            | "limitations"
            | "theoretical_strengths"
            | "theoretical_weaknesses"
            | "empirical_strengths"
            | "empirical_weaknesses"
            | "byline_summary"
            | "why_it_matters"
            | "priority" => key.as_str(),
            "scholarly_rigor" | "novelty" | "relevance_score" | "practical_impact"
            | "interdisciplinary" | "critical_concerns" | "total_score" => key.as_str(),
            _ => continue,
        };

        assignments.push(format!("{column} = ?"));
        params.push(json_value_to_sql_value(key, value)?);
    }

    Ok((assignments, params))
}

fn json_value_to_sql_value(key: &str, value: &serde_json::Value) -> Result<Value, anyhow::Error> {
    if value.is_null() {
        return Ok(Value::Null);
    }

    match key {
        "scholarly_rigor" | "novelty" | "relevance_score" | "practical_impact"
        | "interdisciplinary" | "critical_concerns" | "total_score" => value
            .as_i64()
            .map(Value::Integer)
            .ok_or_else(|| anyhow::anyhow!("Field '{key}' must be an integer or null")),
        _ => value
            .as_str()
            .map(|text| Value::Text(text.to_string()))
            .ok_or_else(|| anyhow::anyhow!("Field '{key}' must be a string or null")),
    }
}

fn map_anyhow_not_found(error: anyhow::Error, entity_name: &str) -> AppError {
    let message = error.to_string();
    if message.contains("not found") {
        AppError::NotFound(message)
    } else if message.contains("No supported fields") {
        AppError::BadRequest(message)
    } else {
        AppError::Internal(format!("{entity_name} operation failed: {message}"))
    }
}

fn article_where_clause(
    query: &ArticleListQuery,
    workspace_id: Option<i64>,
) -> (String, Vec<Value>) {
    let mut conditions = Vec::new();
    let mut params = Vec::new();

    if let Some(id) = workspace_id {
        conditions.push("workspace_id = ?".to_string());
        params.push(Value::Integer(id));
    }
    if let Some(date_from) = query.date_from.as_ref().filter(|value| !value.is_empty()) {
        conditions.push("reg_date >= ?".to_string());
        params.push(Value::Text(date_from.clone()));
    }
    if let Some(date_to) = query.date_to.as_ref().filter(|value| !value.is_empty()) {
        conditions.push("reg_date <= ?".to_string());
        params.push(Value::Text(date_to.clone()));
    }
    if let Some(min_score) = query.min_score {
        conditions.push("total_score >= ?".to_string());
        params.push(Value::Integer(i64::from(min_score)));
    }
    if let Some(max_score) = query.max_score {
        conditions.push("total_score <= ?".to_string());
        params.push(Value::Integer(i64::from(max_score)));
    }
    if let Some(tier) = query.tier.as_ref().filter(|value| !value.is_empty()) {
        conditions.push("priority = ?".to_string());
        params.push(Value::Text(tier.clone()));
    }
    if let Some(category) = query.category.as_ref().filter(|value| !value.is_empty()) {
        conditions.push("LOWER(COALESCE(category, '')) = LOWER(?)".to_string());
        params.push(Value::Text(category.clone()));
    }
    if let Some(search) = query.search.as_ref().filter(|value| !value.is_empty()) {
        conditions.push(
            "(LOWER(COALESCE(title, '')) LIKE ? OR LOWER(COALESCE(key_argument, '')) LIKE ?)"
                .to_string(),
        );
        let pattern = format!("%{}%", search.to_lowercase());
        params.push(Value::Text(pattern.clone()));
        params.push(Value::Text(pattern));
    }

    if conditions.is_empty() {
        (String::new(), params)
    } else {
        (format!(" WHERE {}", conditions.join(" AND ")), params)
    }
}

fn map_article_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ArticleResponse> {
    Ok(ArticleResponse {
        uid: row.get(0)?,
        title: row.get(1)?,
        url: row.get(2)?,
        category: row.get(3)?,
        first_author: row.get(4)?,
        authors: row.get(5)?,
        pub_date: row.get(6)?,
        journal: row.get(7)?,
        ai_tech: row.get(8)?,
        clinical_domain: row.get(9)?,
        ethics_framework: row.get(10)?,
        primary_issue: row.get(11)?,
        key_stakeholders: row.get(12)?,
        practical_impl: row.get(13)?,
        secondary_issues: row.get(14)?,
        key_argument: row.get(15)?,
        main_findings: row.get(16)?,
        normative_claims: row.get(17)?,
        limitations: row.get(18)?,
        theoretical_strengths: row.get(19)?,
        theoretical_weaknesses: row.get(20)?,
        empirical_strengths: row.get(21)?,
        empirical_weaknesses: row.get(22)?,
        byline_summary: row.get(23)?,
        why_it_matters: row.get(24)?,
        scholarly_rigor: row.get(25)?,
        novelty: row.get(26)?,
        relevance_score: row.get(27)?,
        practical_impact: row.get(28)?,
        interdisciplinary: row.get(29)?,
        critical_concerns: row.get(30)?,
        total_score: row.get(31)?,
        priority: row.get(32)?,
        reg_date: row.get(33)?,
        created_at: row.get(34)?,
        updated_at: row.get(35)?,
    })
}
