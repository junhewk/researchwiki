use std::{collections::HashSet, path::PathBuf, sync::Arc};

use rusqlite::{params_from_iter, types::Value};

use crate::error::{AppError, run_blocking};

const STOP_WORDS: &[&str] = &[
    "the", "a", "an", "and", "or", "but", "in", "on", "at", "to", "for", "of", "with", "by",
    "from", "is", "are", "was", "were", "be", "been", "being", "have", "has", "had", "do", "does",
    "did", "will", "would", "could", "should", "may", "might", "can", "shall", "not", "no", "this",
    "that", "these", "those", "it", "its", "they", "them", "their", "what", "which", "who", "whom",
    "how", "when", "where", "why", "if", "then", "than", "so", "as",
];

pub struct EntityContext {
    pub expanded_query: String,
    pub article_uids: Vec<String>,
}

pub async fn get_entity_context(
    database_path: Arc<PathBuf>,
    query: &str,
    max_entities: usize,
) -> Result<EntityContext, AppError> {
    let keywords: Vec<String> = query
        .split_whitespace()
        .map(|w| w.to_lowercase())
        .filter(|w| w.len() >= 2 && !STOP_WORDS.contains(&w.as_str()))
        .collect();

    if keywords.is_empty() {
        return Ok(EntityContext {
            expanded_query: query.to_string(),
            article_uids: Vec::new(),
        });
    }

    let db_path = database_path.clone();
    let max_ent = max_entities;
    let query_owned = query.to_string();

    run_blocking(move || {
        let query = &query_owned;
        let conn = crate::db::open_connection(&*db_path)?;

        // Find entities matching keywords with a single batched query.
        let patterns: Vec<String> = keywords.iter().map(|kw| format!("%{kw}%")).collect();
        let n = patterns.len();

        // Build: WHERE LOWER(canonical_name) LIKE ?1 OR ... OR LOWER(COALESCE(aliases_json,'')) LIKE ?1 OR ...
        let name_clauses: Vec<String> = (1..=n)
            .map(|i| format!("LOWER(canonical_name) LIKE ?{i}"))
            .collect();
        let alias_clauses: Vec<String> = (1..=n)
            .map(|i| format!("LOWER(COALESCE(aliases_json, '')) LIKE ?{i}"))
            .collect();
        let where_clause = name_clauses
            .iter()
            .chain(alias_clauses.iter())
            .cloned()
            .collect::<Vec<_>>()
            .join(" OR ");

        let limit_param_idx = n + 1;
        let sql = format!(
            "SELECT DISTINCT id, canonical_name FROM kg_entities
             WHERE {where_clause}
             ORDER BY mention_count DESC
             LIMIT ?{limit_param_idx}"
        );

        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = patterns
            .iter()
            .map(|p| Box::new(p.clone()) as Box<dyn rusqlite::types::ToSql>)
            .collect();
        params.push(Box::new(max_ent as i64));

        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|p| p.as_ref()).collect();

        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(param_refs.as_slice(), |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?;

        let mut entity_ids = Vec::new();
        let mut entity_names = Vec::new();
        for row in rows {
            let (id, name) = row?;
            entity_ids.push(id);
            entity_names.push(name);
        }

        // Get article UIDs from entity mentions.
        let article_uids = if entity_ids.is_empty() {
            Vec::new()
        } else {
            let placeholders = vec!["?"; entity_ids.len()].join(", ");
            let sql = format!(
                "SELECT DISTINCT article_uid FROM kg_article_entities
                 WHERE entity_id IN ({placeholders})"
            );
            let params: Vec<Value> = entity_ids.iter().copied().map(Value::Integer).collect();
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map(params_from_iter(params.iter()), |row| {
                row.get::<_, String>(0)
            })?;
            rows.collect::<Result<Vec<_>, _>>()?
        };

        let expanded_query = if entity_names.is_empty() {
            query.to_string()
        } else {
            format!("{query} {}", entity_names.join(" "))
        };

        Ok(EntityContext {
            expanded_query,
            article_uids,
        })
    })
    .await
}

/// Boost scores for chunks from articles mentioning KG entities.
pub fn boost_graph_results(
    results: &mut Vec<(i64, f64, String)>,
    related_uids: &HashSet<String>,
    boost_factor: f64,
) {
    for (_, score, article_uid) in results.iter_mut() {
        if related_uids.contains(article_uid) {
            *score *= boost_factor;
        }
    }
}
