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
    let raw_total = clamp_score(evaluation, "scholarly_rigor", 0, 5)
        + clamp_score(evaluation, "novelty", 0, 5)
        + clamp_score(evaluation, "relevance_score", 0, 5)
        + clamp_score(evaluation, "practical_impact", 0, 5)
        + clamp_score(evaluation, "interdisciplinary", 0, 4)
        + clamp_score(evaluation, "critical_concerns", -5, 0);
    let normalized_total = ((raw_total.max(0) * 100) + 12) / 24;
    let normalized_total = normalized_total.clamp(0, 100);
    let priority = priority_for_score(normalized_total);

    evaluation.insert("total_score".to_string(), Value::from(normalized_total));
    evaluation.insert("priority".to_string(), Value::String(priority.to_string()));
}

fn clamp_score(evaluation: &mut Map<String, Value>, field: &str, min: i64, max: i64) -> i64 {
    let value = evaluation
        .get(field)
        .and_then(score_value_as_i64)
        .unwrap_or(0)
        .clamp(min, max);
    evaluation.insert(field.to_string(), Value::from(value));
    value
}

fn score_value_as_i64(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_f64().map(|value| value as i64))
        .or_else(|| value.as_str().and_then(parse_score_string))
}

fn parse_score_string(value: &str) -> Option<i64> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }

    trimmed.parse::<i64>().ok().or_else(|| {
        trimmed
            .split_once('/')
            .and_then(|(score, _)| score.trim().parse::<i64>().ok())
    })
}

fn priority_for_score(total_score: i64) -> &'static str {
    if total_score >= 75 {
        "Tier1"
    } else if total_score >= 40 {
        "Tier2"
    } else {
        "Tier3"
    }
}

const EVAL_TEXT_HEAD_CHARS: usize = 10_000;
const EVAL_TEXT_TAIL_CHARS: usize = 4_000;
const EVAL_TEXT_MAX_CHARS: usize = EVAL_TEXT_HEAD_CHARS + EVAL_TEXT_TAIL_CHARS;

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
        let article_text = compact_evaluation_text(text);
        if article_text.trim().is_empty() {
            return Ok(None);
        }
        if text.chars().count() > EVAL_TEXT_MAX_CHARS {
            warn!(
                "compacted evaluation text for {} from {} to {} chars",
                candidate.uid(),
                text.chars().count(),
                article_text.chars().count()
            );
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

fn compact_evaluation_text(text: &str) -> String {
    let char_count = text.chars().count();
    if char_count <= EVAL_TEXT_MAX_CHARS {
        return text.to_string();
    }

    let head = text.chars().take(EVAL_TEXT_HEAD_CHARS).collect::<String>();
    let tail_start = char_count.saturating_sub(EVAL_TEXT_TAIL_CHARS);
    let tail = text.chars().skip(tail_start).collect::<String>();
    format!(
        "{head}\n\n[... middle of article omitted to keep local LLM evaluation bounded ...]\n\n{tail}"
    )
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn evaluation_scores_are_normalized_to_100_point_scale() {
        let mut evaluation = Map::from_iter([
            ("scholarly_rigor".to_string(), json!(5)),
            ("novelty".to_string(), json!(4)),
            ("relevance_score".to_string(), json!(5)),
            ("practical_impact".to_string(), json!(4)),
            ("interdisciplinary".to_string(), json!(4)),
            ("critical_concerns".to_string(), json!(0)),
        ]);

        ensure_evaluation_scores(&mut evaluation);

        assert_eq!(evaluation.get("total_score"), Some(&json!(92)));
        assert_eq!(evaluation.get("priority"), Some(&json!("Tier1")));
    }

    #[test]
    fn evaluation_scores_are_clamped_before_tiering() {
        let mut evaluation = Map::from_iter([
            ("scholarly_rigor".to_string(), json!(99)),
            ("novelty".to_string(), json!(-2)),
            ("relevance_score".to_string(), json!(5)),
            ("practical_impact".to_string(), json!(5)),
            ("interdisciplinary".to_string(), json!(4)),
            ("critical_concerns".to_string(), json!(-99)),
            ("total_score".to_string(), json!(999)),
            ("priority".to_string(), json!("Tier1")),
        ]);

        ensure_evaluation_scores(&mut evaluation);

        assert_eq!(evaluation.get("scholarly_rigor"), Some(&json!(5)));
        assert_eq!(evaluation.get("novelty"), Some(&json!(0)));
        assert_eq!(evaluation.get("critical_concerns"), Some(&json!(-5)));
        assert_eq!(evaluation.get("total_score"), Some(&json!(58)));
        assert_eq!(evaluation.get("priority"), Some(&json!("Tier2")));
    }

    #[test]
    fn evaluation_scores_accept_numeric_strings() {
        let mut evaluation = Map::from_iter([
            ("scholarly_rigor".to_string(), json!("5")),
            ("novelty".to_string(), json!("4/5")),
            ("relevance_score".to_string(), json!(5)),
            ("practical_impact".to_string(), json!("3")),
            ("interdisciplinary".to_string(), json!("2/4")),
            ("critical_concerns".to_string(), json!("-1")),
        ]);

        ensure_evaluation_scores(&mut evaluation);

        assert_eq!(evaluation.get("scholarly_rigor"), Some(&json!(5)));
        assert_eq!(evaluation.get("novelty"), Some(&json!(4)));
        assert_eq!(evaluation.get("critical_concerns"), Some(&json!(-1)));
        assert_eq!(evaluation.get("total_score"), Some(&json!(75)));
        assert_eq!(evaluation.get("priority"), Some(&json!("Tier1")));
    }

    #[test]
    fn compact_evaluation_text_keeps_head_and_tail() {
        let text = format!(
            "{}{}{}",
            "A".repeat(EVAL_TEXT_HEAD_CHARS + 100),
            "MIDDLE",
            "Z".repeat(EVAL_TEXT_TAIL_CHARS + 100)
        );

        let compacted = compact_evaluation_text(&text);

        assert!(compacted.starts_with("AAA"));
        assert!(compacted.contains("middle of article omitted"));
        assert!(compacted.ends_with("ZZZ"));
        assert!(compacted.len() < text.len());
    }
}
