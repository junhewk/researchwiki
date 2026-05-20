use serde::{Deserialize, Serialize};

/// A research workspace: a single topic/collection framing that scopes the
/// articles, knowledge graph, gather queries, and prompt overrides.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Workspace {
    pub id: i64,
    pub name: String,
    pub slug: String,
    pub db_filename: String,
    pub primary_question: String,
    pub gap_note: String,
    pub refined_question: String,
    pub seed_concepts: Vec<String>,
    pub override_queries: Vec<String>,
    pub topic_descriptor: String,
    pub lookback_days: i32,
    pub is_active: bool,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct WorkspaceSummary {
    pub id: i64,
    pub name: String,
    pub slug: String,
    pub is_active: bool,
}

#[derive(Clone, Debug, Deserialize)]
pub struct WorkspaceCreate {
    pub name: String,
    #[serde(default)]
    pub primary_question: String,
    #[serde(default)]
    pub gap_note: String,
    #[serde(default)]
    pub topic_descriptor: String,
    #[serde(default)]
    pub seed_concepts: Vec<String>,
    #[serde(default)]
    pub override_queries: Vec<String>,
    #[serde(default = "default_lookback_days")]
    pub lookback_days: i32,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct WorkspaceUpdate {
    pub name: Option<String>,
    pub primary_question: Option<String>,
    pub gap_note: Option<String>,
    pub refined_question: Option<String>,
    pub topic_descriptor: Option<String>,
    pub seed_concepts: Option<Vec<String>>,
    pub override_queries: Option<Vec<String>>,
    pub lookback_days: Option<i32>,
}

fn default_lookback_days() -> i32 {
    180
}
