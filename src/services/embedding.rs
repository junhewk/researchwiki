use reqwest::{Client, RequestBuilder};
use serde_json::{Value as JsonValue, json};

use crate::{config::EmbeddingConfig, error::AppError};

#[derive(Clone)]
pub struct EmbeddingService {
    client: Client,
    config: EmbeddingConfig,
    expected_dimensions: u32,
}

impl EmbeddingService {
    pub fn new(client: Client, config: EmbeddingConfig, expected_dimensions: u32) -> Self {
        Self {
            client,
            config,
            expected_dimensions,
        }
    }

    pub async fn embed_texts(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, AppError> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        if !self.config.is_configured() {
            return Err(AppError::BadRequest(
                "Embedding endpoint is not configured (Settings → Embedding endpoint)".to_string(),
            ));
        }

        let endpoint = format!("{}/embeddings", self.config.base_url);
        let response = self
            .with_optional_auth(self.client.post(&endpoint))
            .json(&json!({
                "model": self.config.model,
                "input": texts,
            }))
            .send()
            .await
            .map_err(|error| {
                AppError::Internal(format!("Embeddings request to {endpoint} failed: {error}"))
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
                "Embeddings request failed with status {status}: {snippet}"
            )));
        }

        let payload: JsonValue = serde_json::from_str(&body).map_err(|error| {
            AppError::Internal(format!("Failed to parse embeddings response: {error}"))
        })?;
        let Some(data) = payload.get("data").and_then(JsonValue::as_array) else {
            return Err(AppError::Internal(
                "Embeddings response missing data".to_string(),
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
            validate_embedding_dimensions(&vector, self.expected_dimensions)?;
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

    fn with_optional_auth(&self, req: RequestBuilder) -> RequestBuilder {
        // Local embedding servers (llama-server, infinity) usually accept
        // no auth header. Only attach Bearer when we actually have a key.
        if self.config.api_key.is_empty() {
            req
        } else {
            req.bearer_auth(&self.config.api_key)
        }
    }
}

fn validate_embedding_dimensions(vector: &[f32], expected_dimensions: u32) -> Result<(), AppError> {
    if vector.len() != expected_dimensions as usize {
        return Err(AppError::Internal(format!(
            "Embedding model returned {} dimensions, but the current vector table expects {}. Change Settings -> Embeddings to match the selected embedding model, then restart.",
            vector.len(),
            expected_dimensions
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::validate_embedding_dimensions;

    #[test]
    fn validates_embedding_vector_length() {
        assert!(validate_embedding_dimensions(&[0.0, 1.0, 2.0], 3).is_ok());

        let err = validate_embedding_dimensions(&[0.0, 1.0], 3).unwrap_err();
        assert!(err.to_string().contains("expects 3"));
    }
}
