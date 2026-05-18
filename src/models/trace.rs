use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
pub struct TraceListQuery {
    #[serde(default = "default_page")]
    pub page: u32,
    #[serde(default = "default_page_size")]
    pub page_size: u32,
    pub prompt_name: Option<String>,
    pub article_uid: Option<String>,
    pub model: Option<String>,
    pub success: Option<bool>,
}

#[derive(Debug, Serialize)]
pub struct TraceResponse {
    pub id: i64,
    pub prompt_name: String,
    pub prompt_version: Option<i64>,
    pub article_uid: Option<String>,
    pub model: Option<String>,
    pub input_text: Option<String>,
    pub output_text: Option<String>,
    pub tokens_input: Option<i64>,
    pub tokens_output: Option<i64>,
    pub tokens_total: Option<i64>,
    pub latency_ms: Option<i64>,
    pub cost_usd: Option<f64>,
    pub success: bool,
    pub error_message: Option<String>,
    pub created_at: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct TraceListResponse {
    pub items: Vec<TraceResponse>,
    pub total: i64,
    pub page: u32,
    pub page_size: u32,
    pub pages: u32,
}

#[derive(Debug, Serialize)]
pub struct TraceSummary {
    pub prompt_name: String,
    pub total_executions: i64,
    pub successful_executions: i64,
    pub failed_executions: i64,
    pub avg_latency_ms: Option<f64>,
    pub total_tokens: Option<i64>,
    pub total_cost_usd: Option<f64>,
}

fn default_page() -> u32 {
    1
}

fn default_page_size() -> u32 {
    20
}
