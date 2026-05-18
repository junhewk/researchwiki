use std::{collections::BTreeMap, sync::Arc};

use serde_json::{Map, Value};
use tracing::warn;

use crate::{
    error::AppError,
    services::{
        fetcher::{ContentData, ContentType, FetchedContent},
        llm::{LlmOutputMode, LlmService},
        pipeline::ArticleCandidate,
    },
};

pub(crate) fn ensure_evaluation_scores(evaluation: &mut Map<String, Value>) {
    if !evaluation.contains_key("total_score") {
        let total = [
            "scholarly_rigor",
            "novelty",
            "relevance_score",
            "practical_impact",
            "interdisciplinary",
            "critical_concerns",
        ]
        .into_iter()
        .filter_map(|field| match evaluation.get(field) {
            Some(Value::Number(number)) => number
                .as_i64()
                .or_else(|| number.as_f64().map(|v| v as i64)),
            _ => None,
        })
        .sum::<i64>();

        evaluation.insert("total_score".to_string(), Value::from(total));
    }

    if !evaluation.contains_key("priority") {
        if let Some(total_score) = evaluation
            .get("total_score")
            .and_then(|value| value.as_i64().or_else(|| value.as_f64().map(|v| v as i64)))
        {
            let priority = if total_score >= 18 {
                "Tier1"
            } else if total_score >= 9 {
                "Tier2"
            } else {
                "Tier3"
            };
            evaluation.insert("priority".to_string(), Value::String(priority.to_string()));
        }
    }
}

const MAX_TEXT_LENGTH: usize = 50_000;

#[derive(Clone)]
pub struct ArticleEvaluator {
    llm_service: Arc<LlmService>,
}

impl ArticleEvaluator {
    pub fn new(llm_service: Arc<LlmService>) -> Self {
        Self { llm_service }
    }

    pub async fn evaluate(
        &self,
        content: &FetchedContent,
        candidate: &ArticleCandidate,
    ) -> Result<Option<Map<String, Value>>, AppError> {
        match (&content.content_type, &content.content) {
            (ContentType::Pdf, ContentData::Binary(_bytes)) => {
                warn!(
                    "PDF binary evaluation for {} is running without remote file upload; using summary/title fallback",
                    candidate.uid()
                );
                self.evaluate_with_text(
                    candidate.summary.as_deref().unwrap_or(&candidate.title),
                    candidate,
                )
                .await
            }
            (_, ContentData::Text(text)) => self.evaluate_with_text(text, candidate).await,
            (_, ContentData::Binary(_)) => {
                warn!(
                    "unexpected binary content for non-PDF type, using summary for {}",
                    candidate.uid()
                );
                self.evaluate_with_text(
                    candidate.summary.as_deref().unwrap_or(&candidate.title),
                    candidate,
                )
                .await
            }
        }
    }

    async fn evaluate_with_text(
        &self,
        text: &str,
        candidate: &ArticleCandidate,
    ) -> Result<Option<Map<String, Value>>, AppError> {
        let article_text: String = text.chars().take(MAX_TEXT_LENGTH).collect();
        if article_text.trim().is_empty() {
            return Ok(None);
        }

        let mut variables = BTreeMap::new();
        variables.insert("article_text".to_string(), article_text);

        let result = self
            .llm_service
            .execute_prompt(
                "full_evaluation",
                variables,
                Some(&candidate.uid()),
                LlmOutputMode::Json,
            )
            .await?;

        let Some(json) = result.json_output else {
            warn!(
                "evaluation returned no JSON for {}, raw: {}",
                candidate.uid(),
                &result.raw_text[..result.raw_text.len().min(200)]
            );
            return Ok(None);
        };

        let mut fields = match json {
            Value::Object(map) => flatten_nested_json(map),
            _ => return Ok(None),
        };

        // Preserve candidate metadata that the LLM may not provide.
        if !fields.contains_key("first_author")
            || fields
                .get("first_author")
                .and_then(Value::as_str)
                .map_or(true, str::is_empty)
        {
            fields.insert(
                "first_author".to_string(),
                Value::String(candidate.first_author.clone()),
            );
        }
        if let Some(authors) = &candidate.authors {
            fields
                .entry("authors".to_string())
                .or_insert_with(|| Value::String(authors.clone()));
        }
        if let Some(pub_date) = &candidate.pub_date {
            fields
                .entry("pub_date".to_string())
                .or_insert_with(|| Value::String(pub_date.clone()));
        }
        if let Some(journal) = &candidate.journal {
            fields
                .entry("journal".to_string())
                .or_insert_with(|| Value::String(journal.clone()));
        }

        ensure_evaluation_scores(&mut fields);
        Ok(Some(fields))
    }
}

/// Flatten nested JSON sections that LLMs sometimes produce.
/// e.g. `{"Metadata": {"title": "..."}, "Scoring": {"novelty": 3}}` → flat map.
fn flatten_nested_json(map: Map<String, Value>) -> Map<String, Value> {
    const SECTION_KEYS: &[&str] = &[
        "metadata",
        "classification",
        "content_analysis",
        "content",
        "scoring",
    ];

    let mut flat = Map::new();
    for (key, value) in map {
        if SECTION_KEYS.contains(&key.to_lowercase().as_str()) {
            if let Value::Object(inner) = value {
                for (inner_key, inner_value) in inner {
                    flat.insert(inner_key, inner_value);
                }
            } else {
                flat.insert(key, value);
            }
        } else {
            flat.insert(key, value);
        }
    }
    flat
}
