use std::{collections::BTreeMap, sync::Arc};

use serde_json::Value;
use tokio::sync::Semaphore;
use tracing::warn;

use crate::{
    error::AppError,
    models::workspace::WorkspaceResearchContext,
    services::{
        llm::{LlmOutputMode, LlmService},
        pipeline::ArticleCandidate,
    },
};

const DEFAULT_CONCURRENCY: usize = 5;

#[derive(Debug)]
pub struct ScreeningResult {
    pub is_relevant: bool,
    pub confidence: Option<f64>,
    pub reasoning: Option<String>,
}

#[derive(Clone)]
pub struct ArticleScreener {
    llm_service: Arc<LlmService>,
}

impl ArticleScreener {
    pub fn new(llm_service: Arc<LlmService>) -> Self {
        Self { llm_service }
    }

    pub async fn screen(
        &self,
        candidate: &ArticleCandidate,
        context: &WorkspaceResearchContext,
    ) -> ScreeningResult {
        match self.screen_inner(candidate, context).await {
            Ok(result) => result,
            Err(error) => {
                warn!(
                    "screening failed for {}: {error}, defaulting to relevant",
                    candidate.uid()
                );
                ScreeningResult {
                    is_relevant: true,
                    confidence: None,
                    reasoning: Some(format!("screening error: {error}")),
                }
            }
        }
    }

    pub async fn screen_batch(
        &self,
        candidates: &[ArticleCandidate],
        concurrency: usize,
        context: &WorkspaceResearchContext,
    ) -> Vec<ScreeningResult> {
        let semaphore = Arc::new(Semaphore::new(concurrency.max(1)));
        let futures: Vec<_> = candidates
            .iter()
            .map(|candidate| {
                let screener = self.clone();
                let semaphore = semaphore.clone();
                let candidate = candidate.clone();
                let context = context.clone();
                async move {
                    let Ok(_permit) = semaphore.acquire().await else {
                        return ScreeningResult {
                            is_relevant: true,
                            confidence: None,
                            reasoning: Some("semaphore closed".to_string()),
                        };
                    };
                    screener.screen(&candidate, &context).await
                }
            })
            .collect();
        futures::future::join_all(futures).await
    }

    pub async fn filter_relevant(
        &self,
        candidates: &[ArticleCandidate],
        context: &WorkspaceResearchContext,
    ) -> Vec<ArticleCandidate> {
        let concurrency = DEFAULT_CONCURRENCY.min(self.llm_service.max_concurrent_requests());
        let results = self.screen_batch(candidates, concurrency, context).await;
        candidates
            .iter()
            .zip(results)
            .filter(|(_, result)| result.is_relevant)
            .map(|(candidate, _)| candidate.clone())
            .collect()
    }

    async fn screen_inner(
        &self,
        candidate: &ArticleCandidate,
        context: &WorkspaceResearchContext,
    ) -> Result<ScreeningResult, AppError> {
        let summary = candidate
            .summary
            .as_deref()
            .unwrap_or("")
            .chars()
            .take(500)
            .collect::<String>();

        let mut variables = BTreeMap::new();
        variables.insert("title".to_string(), candidate.title.clone());
        variables.insert("summary".to_string(), summary);
        variables.insert(
            "collection_context".to_string(),
            context.collection_context(),
        );
        variables.insert(
            "topic_descriptor".to_string(),
            if context.topic_descriptor.trim().is_empty() {
                "the current research collection focus".to_string()
            } else {
                context.topic_descriptor.clone()
            },
        );
        variables.insert(
            "primary_question".to_string(),
            empty_placeholder(&context.primary_question),
        );
        variables.insert("seed_concepts".to_string(), context.seed_concepts_text());
        variables.insert(
            "refined_question".to_string(),
            empty_placeholder(&context.refined_question),
        );

        let result = self
            .llm_service
            .execute_prompt(
                "relevancy_filter",
                variables,
                Some(&candidate.uid()),
                LlmOutputMode::Json,
            )
            .await?;

        let (is_relevant, confidence, reasoning) = match result.json_output {
            Some(ref json) => parse_screening_json(json),
            None => parse_screening_text(&result.raw_text),
        };

        Ok(ScreeningResult {
            is_relevant,
            confidence,
            reasoning,
        })
    }
}

fn empty_placeholder(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        "(not set)".to_string()
    } else {
        trimmed.to_string()
    }
}

fn parse_screening_json(json: &Value) -> (bool, Option<f64>, Option<String>) {
    let decision = json
        .get("decision")
        .and_then(Value::as_str)
        .unwrap_or("yes");
    let is_relevant = !decision.eq_ignore_ascii_case("no");
    let confidence = json.get("confidence").and_then(Value::as_f64);
    let reasoning = json
        .get("reasoning")
        .and_then(Value::as_str)
        .map(str::to_string);
    (is_relevant, confidence, reasoning)
}

fn parse_screening_text(text: &str) -> (bool, Option<f64>, Option<String>) {
    let lower = text.to_lowercase();
    let is_relevant = !lower.contains("\"no\"") && !lower.starts_with("no");
    (is_relevant, None, Some(text.to_string()))
}
