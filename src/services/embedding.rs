use std::sync::Arc;

use reqwest::Client;
use serde_json::{Value as JsonValue, json};

use crate::{error::AppError, models::settings::AiProvider, services::settings::SettingsService};

const EMBEDDING_MODEL: &str = "text-embedding-3-small";

#[derive(Clone)]
pub struct EmbeddingService {
    client: Client,
    settings_service: Arc<SettingsService>,
}

impl EmbeddingService {
    pub fn new(client: Client, settings_service: Arc<SettingsService>) -> Self {
        Self {
            client,
            settings_service,
        }
    }

    pub async fn embed_texts(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, AppError> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        let api_key = self
            .settings_service
            .get_api_key(AiProvider::Openai)
            .await?
            .ok_or_else(|| AppError::BadRequest("OpenAI API key is not configured".to_string()))?;

        let response = self
            .client
            .post("https://api.openai.com/v1/embeddings")
            .bearer_auth(api_key)
            .json(&json!({
                "model": EMBEDDING_MODEL,
                "input": texts,
            }))
            .send()
            .await
            .map_err(|error| {
                AppError::Internal(format!("OpenAI embeddings request failed: {error}"))
            })?;

        let status = response.status();
        let body = response.text().await.map_err(|error| {
            AppError::Internal(format!("Failed to read embeddings response: {error}"))
        })?;
        if !status.is_success() {
            let snippet = if body.len() > 500 {
                format!("{}...", &body[..500])
            } else {
                body
            };
            return Err(AppError::Internal(format!(
                "OpenAI embeddings request failed with status {status}: {snippet}"
            )));
        }

        let payload: JsonValue = serde_json::from_str(&body).map_err(|error| {
            AppError::Internal(format!("Failed to parse embeddings response: {error}"))
        })?;
        let Some(data) = payload.get("data").and_then(JsonValue::as_array) else {
            return Err(AppError::Internal(
                "OpenAI embeddings response missing data".to_string(),
            ));
        };

        let mut embeddings = vec![Vec::new(); texts.len()];
        for item in data {
            let index = item
                .get("index")
                .and_then(JsonValue::as_u64)
                .ok_or_else(|| AppError::Internal("Embedding item missing index".to_string()))?
                as usize;
            let vector = item
                .get("embedding")
                .and_then(JsonValue::as_array)
                .ok_or_else(|| AppError::Internal("Embedding item missing vector".to_string()))?
                .iter()
                .map(|value| {
                    value.as_f64().map(|number| number as f32).ok_or_else(|| {
                        AppError::Internal("Embedding value was not numeric".to_string())
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            if index >= embeddings.len() {
                return Err(AppError::Internal(
                    "Embedding response returned an out-of-range index".to_string(),
                ));
            }
            embeddings[index] = vector;
        }

        Ok(embeddings)
    }

    pub async fn embed_single(&self, text: &str) -> Result<Vec<f32>, AppError> {
        let results = self.embed_texts(&[text.to_string()]).await?;
        results
            .into_iter()
            .next()
            .ok_or_else(|| AppError::Internal("Embedding response was empty".to_string()))
    }
}
