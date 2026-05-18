use std::{collections::BTreeMap, sync::Arc};

use tracing::warn;

use crate::{
    error::AppError,
    services::llm::{LlmOutputMode, LlmService},
};

#[derive(Clone)]
pub struct MultiQueryExpander {
    llm_service: Arc<LlmService>,
}

impl MultiQueryExpander {
    pub fn new(llm_service: Arc<LlmService>) -> Self {
        Self { llm_service }
    }

    /// Generate query variants for multi-query search.
    /// Returns `[original_query, variant1, variant2, variant3]`.
    /// Falls back to just `[original_query]` on error.
    pub async fn expand(&self, query: &str) -> Vec<String> {
        match self.expand_inner(query).await {
            Ok(variants) if !variants.is_empty() => {
                let mut result = vec![query.to_string()];
                result.extend(variants.into_iter().take(3));
                result
            }
            Ok(_) => vec![query.to_string()],
            Err(error) => {
                warn!("multi-query expansion failed: {error}");
                vec![query.to_string()]
            }
        }
    }

    async fn expand_inner(&self, query: &str) -> Result<Vec<String>, AppError> {
        let mut variables = BTreeMap::new();
        variables.insert("query".to_string(), query.to_string());

        let result = self
            .llm_service
            .execute_prompt(
                "multi_query_expansion",
                variables,
                None,
                LlmOutputMode::Text,
            )
            .await?;

        let variants: Vec<String> = result
            .raw_text
            .lines()
            .map(|line| line.trim().to_string())
            .filter(|line| line.len() > 5)
            .collect();

        Ok(variants)
    }
}
