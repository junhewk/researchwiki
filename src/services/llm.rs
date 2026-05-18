use std::{
    collections::BTreeMap,
    error::Error as StdError,
    sync::Arc,
    time::{Duration, Instant},
};

use reqwest::{Client, Response};
use serde_json::{Value, json};
use tokio::sync::Semaphore;
use tracing::warn;

use crate::{
    config::LlmConfig,
    error::AppError,
    services::{
        prompts::PromptService,
        traces::{TraceCreate, TraceService},
    },
};

#[derive(Clone, Copy)]
pub enum LlmOutputMode {
    Text,
    Json,
}

pub struct LlmExecutionResult {
    pub raw_text: String,
    pub json_output: Option<Value>,
    pub model: String,
    pub tokens_input: Option<i64>,
    pub tokens_output: Option<i64>,
    pub latency_ms: i64,
}

/// llama-server accepts temperature for the local Qwen runtime.
pub fn supports_temperature(model: &str) -> bool {
    !model.trim().is_empty()
}

#[derive(Clone)]
pub struct LlmService {
    client: Client,
    prompt_service: Arc<PromptService>,
    trace_service: Arc<TraceService>,
    config: LlmConfig,
    request_permits: Arc<Semaphore>,
}

impl LlmService {
    pub fn new(
        prompt_service: Arc<PromptService>,
        trace_service: Arc<TraceService>,
        config: LlmConfig,
    ) -> Self {
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(config.connect_timeout_seconds))
            .timeout(Duration::from_secs(config.request_timeout_seconds))
            .pool_max_idle_per_host(0)
            .tcp_keepalive(Duration::from_secs(30))
            .build()
            .expect("local LLM reqwest client should build");
        let max_concurrent_requests = config.max_concurrent_requests;

        Self {
            client,
            prompt_service,
            trace_service,
            config,
            request_permits: Arc::new(Semaphore::new(max_concurrent_requests)),
        }
    }

    pub async fn execute_prompt(
        &self,
        prompt_name: &str,
        variables: BTreeMap<String, String>,
        article_uid: Option<&str>,
        output_mode: LlmOutputMode,
    ) -> Result<LlmExecutionResult, AppError> {
        let prompt_config = self.prompt_service.get_prompt_config(prompt_name).await?;
        let prompt_version = self
            .prompt_service
            .get_prompt_version(prompt_name)
            .await
            .ok();
        let input_text = self
            .prompt_service
            .render_prompt(prompt_name, &variables)
            .await?;

        let model = self.config.model.clone();
        let temperature = prompt_config.temperature.unwrap_or(0.5);
        let instructions = append_example_style(
            prompt_config.system.unwrap_or_default(),
            prompt_config.example,
        );

        let started = Instant::now();
        let mut raw_text = None;
        let mut tokens_input = None;
        let mut tokens_output = None;
        let mut error_message = None;

        let result = async {
            let mut body = json!({
                "model": model,
                "messages": build_messages(&instructions, &input_text),
                "stream": false,
            });

            if supports_temperature(&model) {
                body["temperature"] = json!(temperature);
            }

            if matches!(output_mode, LlmOutputMode::Json) {
                body["response_format"] =
                    build_json_response_format(prompt_config.schema.as_ref())?;
            }

            if self.config.disable_thinking {
                body["chat_template_kwargs"] = json!({
                    "enable_thinking": false,
                });
            }

            let queue_started = Instant::now();
            let _permit = self
                .request_permits
                .clone()
                .acquire_owned()
                .await
                .map_err(|error| {
                    AppError::Internal(format!(
                        "failed to acquire local LLM request permit: {error}"
                    ))
                })?;
            let queue_wait = queue_started.elapsed();
            if queue_wait > Duration::from_secs(1) {
                warn!(
                    "local LLM request waited {} ms for concurrency permit",
                    queue_wait.as_millis()
                );
            }

            let endpoint = format!("{}/chat/completions", self.config.base_url);
            let response = self.send_with_retries(&endpoint, &body).await?;

            let status = response.status();
            let response_body = response.text().await.map_err(|error| {
                AppError::Internal(format!("Failed to read local LLM response: {error}"))
            })?;

            if !status.is_success() {
                let snippet = if response_body.len() > 500 {
                    format!("{}...", &response_body[..500])
                } else {
                    response_body
                };
                return Err(AppError::Internal(format!(
                    "Local LLM request failed with status {status}: {snippet}"
                )));
            }

            let payload: Value = serde_json::from_str(&response_body).map_err(|error| {
                AppError::Internal(format!("Failed to parse local LLM response JSON: {error}"))
            })?;

            let extracted_text = extract_output_text(&payload).ok_or_else(|| {
                AppError::Internal("Local LLM response did not contain output text".to_string())
            })?;

            tokens_input = payload
                .get("usage")
                .and_then(|usage| usage.get("input_tokens"))
                .or_else(|| {
                    payload
                        .get("usage")
                        .and_then(|usage| usage.get("prompt_tokens"))
                })
                .and_then(Value::as_i64);
            tokens_output = payload
                .get("usage")
                .and_then(|usage| usage.get("output_tokens"))
                .or_else(|| {
                    payload
                        .get("usage")
                        .and_then(|usage| usage.get("completion_tokens"))
                })
                .and_then(Value::as_i64);
            raw_text = Some(extracted_text.clone());

            let json_output = if matches!(output_mode, LlmOutputMode::Json) {
                parse_json_payload(&extracted_text)
            } else {
                None
            };

            Ok::<_, AppError>(LlmExecutionResult {
                raw_text: extracted_text,
                json_output,
                model: payload
                    .get("model")
                    .and_then(Value::as_str)
                    .unwrap_or(model.as_str())
                    .to_string(),
                tokens_input,
                tokens_output,
                latency_ms: started.elapsed().as_millis() as i64,
            })
        }
        .await;

        if let Err(error) = &result {
            error_message = Some(error.to_string());
        }

        if let Err(trace_error) = self
            .trace_service
            .record_trace(TraceCreate {
                prompt_name: prompt_name.to_string(),
                prompt_version,
                article_uid: article_uid.map(str::to_string),
                model: model.clone(),
                input_text: truncate_for_trace(&input_text),
                output_text: raw_text.clone(),
                tokens_input,
                tokens_output,
                latency_ms: Some(started.elapsed().as_millis() as i64),
                cost_usd: None,
                success: result.is_ok(),
                error_message,
            })
            .await
        {
            warn!(
                "failed to log prompt trace for {}: {}",
                prompt_name, trace_error
            );
        }

        result
    }

    async fn send_with_retries(&self, endpoint: &str, body: &Value) -> Result<Response, AppError> {
        let max_attempts = self.config.max_attempts.max(1);

        for attempt in 1..=max_attempts {
            let result = self
                .client
                .post(endpoint)
                .bearer_auth(&self.config.api_key)
                .json(body)
                .send()
                .await;

            match result {
                Ok(response) => return Ok(response),
                Err(error) if attempt < max_attempts => {
                    let detail = reqwest_error_with_sources(&error);
                    let delay = retry_delay(attempt);
                    warn!(
                        "local LLM request send failed on attempt {attempt}/{max_attempts}; retrying in {} ms: {detail}",
                        delay.as_millis()
                    );
                    tokio::time::sleep(delay).await;
                }
                Err(error) => {
                    let detail = reqwest_error_with_sources(&error);
                    return Err(AppError::Internal(format!(
                        "Local LLM request failed after {max_attempts} attempts: {detail}"
                    )));
                }
            }
        }

        Err(AppError::Internal(
            "Local LLM request failed before sending".to_string(),
        ))
    }
}

fn retry_delay(attempt: usize) -> Duration {
    match attempt {
        1 => Duration::from_millis(750),
        2 => Duration::from_secs(2),
        _ => Duration::from_secs(5),
    }
}

fn build_messages(instructions: &str, input_text: &str) -> Value {
    let mut messages = Vec::new();

    if !instructions.is_empty() {
        messages.push(json!({
            "role": "system",
            "content": instructions,
        }));
    }

    messages.push(json!({
        "role": "user",
        "content": input_text,
    }));

    Value::Array(messages)
}

fn reqwest_error_with_sources(error: &reqwest::Error) -> String {
    let mut message = error.to_string();
    let mut source = StdError::source(error);

    while let Some(error) = source {
        message.push_str(": ");
        message.push_str(&error.to_string());
        source = error.source();
    }

    message
}

fn build_json_response_format(schema: Option<&serde_yaml::Value>) -> Result<Value, AppError> {
    let mut response_format = json!({
        "type": "json_object",
    });

    if let Some(schema) = schema.filter(|value| !matches!(value, serde_yaml::Value::Null)) {
        response_format["schema"] = serde_json::to_value(schema).map_err(|error| {
            AppError::Internal(format!("Invalid JSON schema for prompt: {error}"))
        })?;
    }

    Ok(response_format)
}

fn append_example_style(system: String, example: Option<String>) -> String {
    match example {
        Some(example) if !example.trim().is_empty() => {
            if system.trim().is_empty() {
                format!("## Example Style:\n{example}")
            } else {
                format!("{system}\n\n## Example Style:\n{example}")
            }
        }
        _ => system,
    }
}

fn extract_output_text(payload: &Value) -> Option<String> {
    if let Some(text) = payload
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .and_then(|choice| choice.get("message"))
        .and_then(|message| message.get("content"))
        .and_then(Value::as_str)
    {
        if !text.trim().is_empty() {
            return Some(text.to_string());
        }
    }

    if let Some(text) = payload.get("output_text").and_then(Value::as_str) {
        if !text.trim().is_empty() {
            return Some(text.to_string());
        }
    }

    let mut text = String::new();
    let output_items = payload.get("output")?.as_array()?;

    for item in output_items {
        if item.get("type").and_then(Value::as_str) != Some("message") {
            continue;
        }
        let Some(content) = item.get("content").and_then(Value::as_array) else {
            continue;
        };
        for part in content {
            if part.get("type").and_then(Value::as_str) == Some("output_text") {
                if let Some(part_text) = part.get("text").and_then(Value::as_str) {
                    text.push_str(part_text);
                }
            }
        }
    }

    if text.is_empty() { None } else { Some(text) }
}

fn parse_json_payload(text: &str) -> Option<Value> {
    serde_json::from_str(text)
        .ok()
        .or_else(|| {
            extract_json_slice(text, '[', ']').and_then(|slice| serde_json::from_str(slice).ok())
        })
        .or_else(|| {
            extract_json_slice(text, '{', '}').and_then(|slice| serde_json::from_str(slice).ok())
        })
}

fn extract_json_slice(text: &str, open: char, close: char) -> Option<&str> {
    let start = text.find(open)?;
    let end = text.rfind(close)?;
    (end > start).then_some(&text[start..=end])
}

fn truncate_for_trace(text: &str) -> String {
    const LIMIT: usize = 2_000;
    if text.chars().count() > LIMIT {
        format!("{}...", text.chars().take(LIMIT).collect::<String>())
    } else {
        text.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::truncate_for_trace;

    #[test]
    fn trace_truncation_preserves_utf8_boundaries() {
        let text = format!("{}•{}", "a".repeat(1_999), "b".repeat(10));

        let truncated = truncate_for_trace(&text);

        assert!(truncated.ends_with("..."));
        assert!(truncated.contains('•'));
    }
}
