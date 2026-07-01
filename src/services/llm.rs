use std::{
    collections::BTreeMap,
    error::Error as StdError,
    sync::Arc,
    time::{Duration, Instant},
};

use reqwest::{Client, Response, StatusCode, header};
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
        let provider = self.config.effective_provider();

        let result = async {
            if provider.uses_native_anthropic_api() {
                let result = self
                    .execute_anthropic(
                        &model,
                        &instructions,
                        &input_text,
                        output_mode,
                        prompt_config.schema.as_ref(),
                        temperature,
                        started,
                    )
                    .await?;
                tokens_input = result.tokens_input;
                tokens_output = result.tokens_output;
                raw_text = Some(result.raw_text.clone());
                return Ok(result);
            }

            let mut body = json!({
                "model": model,
                "messages": build_messages(&instructions, &input_text),
                "stream": false,
            });

            if supports_temperature(&model) {
                body["temperature"] = json!(temperature);
            }

            if matches!(output_mode, LlmOutputMode::Json) {
                body["response_format"] = build_json_response_format(
                    prompt_config.schema.as_ref(),
                    uses_deepseek_chat_api(&self.config),
                )?;
            }

            if self.config.disable_thinking {
                if uses_deepseek_chat_api(&self.config) {
                    body["thinking"] = json!({ "type": "disabled" });
                } else {
                    body["chat_template_kwargs"] = json!({
                        "enable_thinking": false,
                    });
                }
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

    async fn execute_anthropic(
        &self,
        model: &str,
        instructions: &str,
        input_text: &str,
        output_mode: LlmOutputMode,
        schema: Option<&serde_yaml::Value>,
        temperature: f64,
        started: Instant,
    ) -> Result<LlmExecutionResult, AppError> {
        let mut body = json!({
            "model": model,
            "messages": [
                {
                    "role": "user",
                    "content": input_text,
                }
            ],
            "max_tokens": 8192,
            "temperature": temperature,
        });

        if !instructions.trim().is_empty() {
            body["system"] = json!(instructions);
        }

        if matches!(output_mode, LlmOutputMode::Json) {
            body["tools"] = json!([build_anthropic_json_tool(schema)?]);
            body["tool_choice"] = json!({
                "type": "tool",
                "name": "emit_json",
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
                    "failed to acquire Anthropic LLM request permit: {error}"
                ))
            })?;
        let queue_wait = queue_started.elapsed();
        if queue_wait > Duration::from_secs(1) {
            warn!(
                "Anthropic LLM request waited {} ms for concurrency permit",
                queue_wait.as_millis()
            );
        }

        let endpoint = format!("{}/messages", self.config.base_url);
        let response = self.send_anthropic_with_retries(&endpoint, &body).await?;
        let status = response.status();
        let response_body = response.text().await.map_err(|error| {
            AppError::Internal(format!("Failed to read Anthropic LLM response: {error}"))
        })?;

        if !status.is_success() {
            let snippet = if response_body.len() > 500 {
                format!("{}...", &response_body[..500])
            } else {
                response_body
            };
            return Err(AppError::Internal(format!(
                "Anthropic LLM request failed with status {status}: {snippet}"
            )));
        }

        let payload: Value = serde_json::from_str(&response_body).map_err(|error| {
            AppError::Internal(format!(
                "Failed to parse Anthropic LLM response JSON: {error}"
            ))
        })?;

        let json_output = if matches!(output_mode, LlmOutputMode::Json) {
            Some(extract_anthropic_tool_input(&payload).ok_or_else(|| {
                AppError::Internal(
                    "Anthropic LLM response did not contain the requested JSON tool output"
                        .to_string(),
                )
            })?)
        } else {
            None
        };

        let raw_text = if let Some(json_output) = json_output.as_ref() {
            serde_json::to_string(json_output).map_err(|error| {
                AppError::Internal(format!(
                    "Failed to serialize Anthropic JSON output: {error}"
                ))
            })?
        } else {
            extract_anthropic_text(&payload).ok_or_else(|| {
                AppError::Internal("Anthropic LLM response did not contain output text".to_string())
            })?
        };

        Ok(LlmExecutionResult {
            raw_text,
            json_output,
            model: payload
                .get("model")
                .and_then(Value::as_str)
                .unwrap_or(model)
                .to_string(),
            tokens_input: payload
                .get("usage")
                .and_then(|usage| usage.get("input_tokens"))
                .and_then(Value::as_i64),
            tokens_output: payload
                .get("usage")
                .and_then(|usage| usage.get("output_tokens"))
                .and_then(Value::as_i64),
            latency_ms: started.elapsed().as_millis() as i64,
        })
    }

    pub fn max_concurrent_requests(&self) -> usize {
        self.config.max_concurrent_requests.max(1)
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
                Ok(response)
                    if should_retry_response(response.status()) && attempt < max_attempts =>
                {
                    let status = response.status();
                    let delay = response_retry_delay(attempt, &response);
                    warn!(
                        "local LLM request returned HTTP {status} on attempt {attempt}/{max_attempts}; retrying in {} ms",
                        delay.as_millis()
                    );
                    tokio::time::sleep(delay).await;
                }
                Ok(response) => return Ok(response),
                Err(error) if is_https_plain_http_mismatch(endpoint, &error) => {
                    let detail = reqwest_error_with_sources(&error);
                    return Err(AppError::Internal(format!(
                        "Local LLM endpoint appears to be plain HTTP but base URL uses HTTPS. Change LLM base URL to {}. Original error: {detail}",
                        http_endpoint_suggestion(endpoint)
                    )));
                }
                Err(error) if error.is_timeout() => {
                    let detail = reqwest_error_with_sources(&error);
                    return Err(AppError::Internal(format!(
                        "Local LLM request timed out after {} seconds; not retrying the same prompt. Reduce the evaluation input, lower pipeline concurrency, or increase the local LLM timeout. Original error: {detail}",
                        self.config.request_timeout_seconds
                    )));
                }
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

    async fn send_anthropic_with_retries(
        &self,
        endpoint: &str,
        body: &Value,
    ) -> Result<Response, AppError> {
        let max_attempts = self.config.max_attempts.max(1);

        for attempt in 1..=max_attempts {
            let mut request = self
                .client
                .post(endpoint)
                .header("anthropic-version", "2023-06-01")
                .json(body);
            if !self.config.api_key.is_empty() {
                request = request.header("x-api-key", &self.config.api_key);
            }

            let result = request.send().await;

            match result {
                Ok(response)
                    if should_retry_response(response.status()) && attempt < max_attempts =>
                {
                    let status = response.status();
                    let delay = response_retry_delay(attempt, &response);
                    warn!(
                        "Anthropic LLM request returned HTTP {status} on attempt {attempt}/{max_attempts}; retrying in {} ms",
                        delay.as_millis()
                    );
                    tokio::time::sleep(delay).await;
                }
                Ok(response) => return Ok(response),
                Err(error) if error.is_timeout() => {
                    let detail = reqwest_error_with_sources(&error);
                    return Err(AppError::Internal(format!(
                        "Anthropic LLM request timed out after {} seconds; not retrying the same prompt. Original error: {detail}",
                        self.config.request_timeout_seconds
                    )));
                }
                Err(error) if attempt < max_attempts => {
                    let detail = reqwest_error_with_sources(&error);
                    let delay = retry_delay(attempt);
                    warn!(
                        "Anthropic LLM request send failed on attempt {attempt}/{max_attempts}; retrying in {} ms: {detail}",
                        delay.as_millis()
                    );
                    tokio::time::sleep(delay).await;
                }
                Err(error) => {
                    let detail = reqwest_error_with_sources(&error);
                    return Err(AppError::Internal(format!(
                        "Anthropic LLM request failed after {max_attempts} attempts: {detail}"
                    )));
                }
            }
        }

        Err(AppError::Internal(
            "Anthropic LLM request failed before sending".to_string(),
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

fn should_retry_response(status: StatusCode) -> bool {
    status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error()
}

fn response_retry_delay(attempt: usize, response: &Response) -> Duration {
    parse_retry_after(response.headers().get(header::RETRY_AFTER)).unwrap_or_else(
        || match attempt {
            1 => Duration::from_secs(10),
            2 => Duration::from_secs(30),
            _ => Duration::from_secs(60),
        },
    )
}

fn parse_retry_after(value: Option<&header::HeaderValue>) -> Option<Duration> {
    value
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok())
        .map(Duration::from_secs)
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

fn is_https_plain_http_mismatch(endpoint: &str, error: &reqwest::Error) -> bool {
    if !endpoint.starts_with("https://") {
        return false;
    }

    let detail = reqwest_error_with_sources(error);
    detail.contains("InvalidContentType") || detail.contains("received corrupt message")
}

fn http_endpoint_suggestion(endpoint: &str) -> String {
    endpoint
        .strip_prefix("https://")
        .map(|rest| format!("http://{rest}"))
        .unwrap_or_else(|| endpoint.to_string())
        .trim_end_matches("/chat/completions")
        .to_string()
}

fn uses_deepseek_chat_api(config: &LlmConfig) -> bool {
    config.base_url.contains("api.deepseek.com")
        || config.model.to_ascii_lowercase().starts_with("deepseek-")
}

fn build_json_response_format(
    schema: Option<&serde_yaml::Value>,
    deepseek_compatible: bool,
) -> Result<Value, AppError> {
    let mut response_format = json!({
        "type": "json_object",
    });

    if !deepseek_compatible {
        if let Some(schema) = schema.filter(|value| !matches!(value, serde_yaml::Value::Null)) {
            // llama-server's OpenAI-compatible chat endpoint accepts schema-constrained JSON
            // as {"type":"json_object","schema":...}; OpenAI's nested json_schema wrapper
            // is not portable across llama.cpp versions.
            response_format["schema"] = serde_json::to_value(schema).map_err(|error| {
                AppError::Internal(format!("Invalid JSON schema for prompt: {error}"))
            })?;
        }
    }

    Ok(response_format)
}

fn build_anthropic_json_tool(schema: Option<&serde_yaml::Value>) -> Result<Value, AppError> {
    let input_schema =
        if let Some(schema) = schema.filter(|value| !matches!(value, serde_yaml::Value::Null)) {
            serde_json::to_value(schema).map_err(|error| {
                AppError::Internal(format!("Invalid JSON schema for prompt: {error}"))
            })?
        } else {
            json!({
                "type": "object",
                "additionalProperties": true,
            })
        };

    if input_schema.get("type").and_then(Value::as_str) != Some("object") {
        return Err(AppError::Internal(
            "Anthropic JSON tool output requires an object JSON schema".to_string(),
        ));
    }

    Ok(json!({
        "name": "emit_json",
        "description": "Emit only the structured JSON output requested by the prompt.",
        "input_schema": input_schema,
    }))
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

fn extract_anthropic_text(payload: &Value) -> Option<String> {
    let mut text = String::new();
    let content = payload.get("content")?.as_array()?;

    for part in content {
        if part.get("type").and_then(Value::as_str) == Some("text")
            && let Some(part_text) = part.get("text").and_then(Value::as_str)
        {
            text.push_str(part_text);
        }
    }

    if text.trim().is_empty() {
        None
    } else {
        Some(text)
    }
}

fn extract_anthropic_tool_input(payload: &Value) -> Option<Value> {
    let content = payload.get("content")?.as_array()?;

    content.iter().find_map(|part| {
        (part.get("type").and_then(Value::as_str) == Some("tool_use")
            && part.get("name").and_then(Value::as_str) == Some("emit_json"))
        .then(|| part.get("input").cloned())
        .flatten()
    })
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

#[cfg(test)]
#[test]
fn llama_server_json_schema_uses_schema_field_under_json_object() {
    let schema = serde_yaml::to_value(serde_json::json!({
        "type": "object",
        "properties": {
            "score": { "type": "integer" }
        },
        "required": ["score"]
    }))
    .unwrap();

    let response_format = build_json_response_format(Some(&schema), false).unwrap();

    assert_eq!(
        response_format.get("type"),
        Some(&serde_json::json!("json_object"))
    );
    assert!(response_format.get("schema").is_some());
    assert!(response_format.get("json_schema").is_none());
}

#[cfg(test)]
#[test]
fn deepseek_json_format_omits_local_schema_extension() {
    let schema = serde_yaml::to_value(serde_json::json!({
        "type": "object",
        "properties": {
            "score": { "type": "integer" }
        }
    }))
    .unwrap();

    let response_format = build_json_response_format(Some(&schema), true).unwrap();

    assert_eq!(
        response_format,
        serde_json::json!({ "type": "json_object" })
    );
}

#[cfg(test)]
#[test]
fn anthropic_json_tool_uses_prompt_schema() {
    let schema = serde_yaml::to_value(serde_json::json!({
        "type": "object",
        "properties": {
            "score": { "type": "integer" }
        },
        "required": ["score"]
    }))
    .unwrap();

    let tool = build_anthropic_json_tool(Some(&schema)).unwrap();

    assert_eq!(tool.get("name"), Some(&serde_json::json!("emit_json")));
    assert_eq!(
        tool.pointer("/input_schema/properties/score/type"),
        Some(&serde_json::json!("integer"))
    );
}

#[cfg(test)]
#[test]
fn anthropic_json_tool_rejects_non_object_schema() {
    let schema = serde_yaml::to_value(serde_json::json!({
        "type": "array",
        "items": { "type": "string" }
    }))
    .unwrap();

    let err = build_anthropic_json_tool(Some(&schema)).unwrap_err();

    assert!(err.to_string().contains("object JSON schema"));
}

#[cfg(test)]
#[test]
fn anthropic_response_extractors_handle_text_and_tool_use() {
    let text_payload = serde_json::json!({
        "content": [
            { "type": "text", "text": "hello" },
            { "type": "text", "text": " world" }
        ]
    });
    assert_eq!(
        extract_anthropic_text(&text_payload),
        Some("hello world".to_string())
    );

    let tool_payload = serde_json::json!({
        "content": [
            {
                "type": "tool_use",
                "name": "emit_json",
                "input": { "score": 7 }
            }
        ]
    });
    assert_eq!(
        extract_anthropic_tool_input(&tool_payload),
        Some(serde_json::json!({ "score": 7 }))
    );
}

#[cfg(test)]
#[test]
fn retry_after_parses_seconds() {
    let value = header::HeaderValue::from_static("42");

    assert_eq!(
        parse_retry_after(Some(&value)),
        Some(std::time::Duration::from_secs(42))
    );
}
