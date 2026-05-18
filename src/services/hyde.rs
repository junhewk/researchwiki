use std::{collections::BTreeMap, sync::Arc};

use tracing::warn;

use crate::{
    error::AppError,
    services::llm::{LlmOutputMode, LlmService},
};

#[derive(Clone)]
pub struct HyDEExpander {
    llm_service: Arc<LlmService>,
}

impl HyDEExpander {
    pub fn new(llm_service: Arc<LlmService>) -> Self {
        Self { llm_service }
    }

    /// Generate a hypothetical document passage for the query.
    /// Returns combined `"{query}\n\n{hypothetical_passage}"` for embedding.
    /// Falls back to original query on error.
    pub async fn expand(&self, query: &str) -> String {
        match self.expand_inner(query).await {
            Ok(passage) if !passage.trim().is_empty() => {
                format!("{query}\n\n{passage}")
            }
            Ok(_) => query.to_string(),
            Err(error) => {
                warn!("HyDE expansion failed, using original query: {error}");
                query.to_string()
            }
        }
    }

    async fn expand_inner(&self, query: &str) -> Result<String, AppError> {
        let mut variables = BTreeMap::new();
        variables.insert("query".to_string(), query.to_string());

        let result = self
            .llm_service
            .execute_prompt("hyde_expansion", variables, None, LlmOutputMode::Text)
            .await?;

        Ok(result.raw_text)
    }
}
