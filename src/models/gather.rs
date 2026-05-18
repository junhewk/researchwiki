use serde::{Deserialize, Serialize};

use crate::models::job::JobRunResponse;

#[derive(Debug, Deserialize)]
pub struct GatherDaysQuery {
    #[serde(default = "default_days_back")]
    pub days_back: i32,
}

#[derive(Debug, Serialize)]
pub struct TriggerResponse {
    pub status: String,
    pub source: String,
    pub run_id: String,
    pub message: String,
}

#[derive(Debug, Serialize)]
pub struct PipelineStatusResponse {
    pub run_id: String,
    pub source: String,
    pub status: String,
    pub started_at: String,
    pub completed_at: Option<String>,
    pub candidates_found: i32,
    pub candidates_screened: i32,
    pub candidates_relevant: i32,
    pub candidates_fetched: i32,
    pub candidates_evaluated: i32,
    pub candidates_saved: i32,
    pub candidates_skipped: i32,
    pub errors: i32,
    pub current_item: Option<String>,
    pub current_step: Option<String>,
    pub error_message: Option<String>,
}

impl From<JobRunResponse> for PipelineStatusResponse {
    fn from(run: JobRunResponse) -> Self {
        Self {
            run_id: run.run_id,
            source: run.source,
            status: run.status,
            started_at: run.started_at.or(run.requested_at).unwrap_or_default(),
            completed_at: run.completed_at,
            candidates_found: run.candidates_found,
            candidates_screened: run.candidates_screened,
            candidates_relevant: run.candidates_relevant,
            candidates_fetched: run.candidates_fetched,
            candidates_evaluated: run.candidates_evaluated,
            candidates_saved: run.candidates_saved,
            candidates_skipped: run.candidates_skipped,
            errors: run.errors,
            current_item: run.current_item,
            current_step: run.current_step,
            error_message: run.error_message,
        }
    }
}

fn default_days_back() -> i32 {
    2
}
