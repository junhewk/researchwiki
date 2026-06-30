use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize)]
pub struct JobRunResponse {
    pub run_id: String,
    pub source: String,
    pub days_back: i32,
    pub status: String,
    pub requested_at: Option<String>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub candidates_found: i32,
    pub candidates_screened: i32,
    pub candidates_relevant: i32,
    pub candidates_fetched: i32,
    pub candidates_evaluated: i32,
    pub candidates_saved: i32,
    pub candidates_embedded: i32,
    pub candidates_skipped: i32,
    pub errors: i32,
    pub current_item: Option<String>,
    pub current_step: Option<String>,
    pub error_message: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct JobEventResponse {
    pub id: i64,
    pub event_type: String,
    pub payload_json: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Serialize)]
pub struct JobRunDetailResponse {
    #[serde(flatten)]
    pub run: JobRunResponse,
    pub events: Vec<JobEventResponse>,
}

#[derive(Debug, Deserialize)]
pub struct JobCreateRequest {
    pub source: String,
    #[serde(default = "default_days_back")]
    pub days_back: i32,
}

#[derive(Debug, Deserialize)]
pub struct JobListQuery {
    #[serde(default = "default_limit")]
    pub limit: u32,
}

fn default_days_back() -> i32 {
    2
}

fn default_limit() -> u32 {
    50
}
