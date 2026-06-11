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
            (ContentType::Xml | ContentType::Html, ContentData::Text(text)) => {
                // Strip markup before evaluation; fall back to the raw payload
                // if extraction yields nothing.
                let extracted = crate::services::text_extractor::extract_from_content(
                    text,
                    content.content_type.as_str(),
                );
                let evaluation_text = if extracted.full_text.trim().is_empty() {
                    text.as_str()
                } else {
                    extracted.full_text.as_str()
                };
                self.evaluate_with_text(evaluation_text, candidate).await
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

        overlay_candidate_metadata(&mut fields, candidate);

        Ok(Some(fields))
    }
}

fn overlay_candidate_metadata(fields: &mut Map<String, Value>, candidate: &ArticleCandidate) {
    fields.insert("title".to_string(), Value::String(candidate.title.clone()));
    fields.insert(
        "first_author".to_string(),
        Value::String(candidate.first_author.clone()),
    );
    fields.insert("url".to_string(), Value::String(candidate.url.clone()));

    if let Some(authors) = &candidate.authors {
        fields.insert("authors".to_string(), Value::String(authors.clone()));
    }
    if let Some(pub_date) = &candidate.pub_date {
        fields.insert("pub_date".to_string(), Value::String(pub_date.clone()));
    }
    if let Some(journal) = &candidate.journal {
        fields.insert("journal".to_string(), Value::String(journal.clone()));
    }
    if let Some(doi) = &candidate.doi {
        fields.insert("doi".to_string(), Value::String(doi.clone()));
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

    #[test]
    fn candidate_metadata_overrides_llm_metadata() {
        let candidate = ArticleCandidate {
            source: "semantic_scholar".to_string(),
            source_id: "abc".to_string(),
            title: "Source title".to_string(),
            summary: Some("Source abstract".to_string()),
            first_author: "Source Author".to_string(),
            authors: Some("Source Author, Second Author".to_string()),
            pub_date: Some("2026-05-20".to_string()),
            journal: Some("Source Journal".to_string()),
            doi: Some("10.1000/source".to_string()),
            url: "https://example.com/source".to_string(),
        };
        let mut fields = Map::from_iter([
            ("title".to_string(), json!("LLM hallucinated title")),
            ("first_author".to_string(), json!("LLM Author")),
            ("pub_date".to_string(), json!("1900-01-01")),
            ("journal".to_string(), json!("LLM Journal")),
            ("doi".to_string(), json!("10.1000/llm")),
            ("url".to_string(), json!("https://example.com/llm")),
        ]);

        overlay_candidate_metadata(&mut fields, &candidate);

        assert_eq!(fields.get("title"), Some(&json!("Source title")));
        assert_eq!(fields.get("first_author"), Some(&json!("Source Author")));
        assert_eq!(fields.get("pub_date"), Some(&json!("2026-05-20")));
        assert_eq!(fields.get("journal"), Some(&json!("Source Journal")));
        assert_eq!(fields.get("doi"), Some(&json!("10.1000/source")));
        assert_eq!(
            fields.get("url"),
            Some(&json!("https://example.com/source"))
        );
    }
}
