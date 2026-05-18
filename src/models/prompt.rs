use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct PromptFileConfig {
    pub model: Option<String>,
    pub temperature: Option<f64>,
    pub system: Option<String>,
    pub user: Option<String>,
    pub schema: Option<serde_yaml::Value>,
    pub structured_output: Option<String>,
    pub example: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct PromptResponse {
    pub name: String,
    pub content: String,
    pub model: Option<String>,
    pub temperature: Option<f64>,
    pub current_version: i64,
    pub execution_count: i64,
    pub last_executed: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize)]
pub struct PromptListResponse {
    pub prompts: Vec<PromptResponse>,
}

#[derive(Debug, Deserialize)]
pub struct PromptCreate {
    pub content: String,
    pub description: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct PromptTestRequest {
    pub sample_input: BTreeMap<String, String>,
}

#[derive(Debug, Serialize)]
pub struct PromptVersionResponse {
    pub id: i64,
    pub prompt_name: String,
    pub version: i64,
    pub content: String,
    pub model: Option<String>,
    pub temperature: Option<f64>,
    pub description: Option<String>,
    pub changed_by: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
pub struct ModelConfigEntry {
    pub prompt_name: String,
    pub label: String,
    pub model: String,
    pub temperature: f64,
}

#[derive(Debug, Serialize)]
pub struct ModelConfigCategory {
    pub category: String,
    pub label: String,
    pub configs: Vec<ModelConfigEntry>,
}

#[derive(Debug, Serialize)]
pub struct ModelConfigsResponse {
    pub categories: Vec<ModelConfigCategory>,
}

#[derive(Debug, Deserialize)]
pub struct ModelConfigUpdate {
    pub prompt_name: String,
    pub model: String,
    pub temperature: f64,
}

#[derive(Debug, Deserialize)]
pub struct ModelConfigsUpdateRequest {
    pub updates: Vec<ModelConfigUpdate>,
}

#[derive(Debug, Serialize)]
pub struct PromptReloadResponse {
    pub status: String,
    pub prompts_loaded: usize,
}

#[derive(Debug, Serialize)]
pub struct PromptTestResponse {
    pub output: serde_json::Value,
    pub tokens_used: Option<i64>,
    pub latency_ms: Option<i64>,
    pub model: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct SchemaFieldResponse {
    pub name: String,
    pub r#type: String,
    pub description: String,
    pub required: bool,
    pub default: Option<serde_json::Value>,
    pub options: Option<Vec<String>>,
    pub min: Option<f64>,
    pub max: Option<f64>,
}

#[derive(Clone, Debug, Serialize)]
pub struct SchemaResponse {
    pub name: String,
    pub description: String,
    pub fields: Vec<SchemaFieldResponse>,
}

#[derive(Debug, Serialize)]
pub struct SchemaListResponse {
    pub schemas: Vec<String>,
}
