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

/// Canonical research framing for one workspace. This is the object downstream
/// services should consume when they need to understand what the collection is
/// about, instead of each service loading a different subset of workspace fields.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceResearchContext {
    pub name: String,
    pub primary_question: String,
    pub gap_note: String,
    pub refined_question: String,
    pub seed_concepts: Vec<String>,
    pub override_queries: Vec<String>,
    pub topic_descriptor: String,
    pub lookback_days: i32,
}

impl WorkspaceResearchContext {
    pub fn from_workspace(workspace: &Workspace) -> Self {
        Self {
            name: workspace.name.clone(),
            primary_question: workspace.primary_question.clone(),
            gap_note: workspace.gap_note.clone(),
            refined_question: workspace.refined_question.clone(),
            seed_concepts: workspace.seed_concepts.clone(),
            override_queries: workspace.override_queries.clone(),
            topic_descriptor: workspace.topic_descriptor.clone(),
            lookback_days: workspace.lookback_days,
        }
    }

    /// Search terms used by gather: explicit override queries win, otherwise
    /// seed concepts are used. If both are empty, source-specific defaults apply.
    pub fn query_terms(&self) -> &[String] {
        if self.override_queries.is_empty() {
            &self.seed_concepts
        } else {
            &self.override_queries
        }
    }

    pub fn query_source_label(&self) -> &'static str {
        if !self.override_queries.is_empty() {
            "override queries"
        } else if !self.seed_concepts.is_empty() {
            "seed concepts"
        } else {
            "source defaults"
        }
    }

    pub fn query_preview(&self, limit: usize) -> String {
        let terms = self.query_terms();
        if terms.is_empty() {
            return "source defaults".to_string();
        }
        let limit = limit.max(1);
        let mut preview = terms
            .iter()
            .take(limit)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        if terms.len() > limit {
            preview.push_str(&format!(", +{} more", terms.len() - limit));
        }
        preview
    }

    /// Context used by screening, KG extraction, and wiki synthesis. Gap notes
    /// stay out of this context; they belong to Gap Bridge.
    pub fn collection_context(&self) -> String {
        let mut lines = Vec::new();
        push_context_line(&mut lines, "Workspace", &self.name);
        push_context_line(&mut lines, "Topic", &self.topic_descriptor);
        push_context_line(&mut lines, "Primary question", &self.primary_question);
        push_context_line(&mut lines, "Refined question", &self.refined_question);
        if !self.seed_concepts.is_empty() {
            lines.push(format!("Seed concepts: {}", self.seed_concepts.join(", ")));
        }
        if lines.is_empty() {
            "the current research collection focus".to_string()
        } else {
            lines.join("\n")
        }
    }

    pub fn seed_concepts_text(&self) -> String {
        if self.seed_concepts.is_empty() {
            "(none)".to_string()
        } else {
            self.seed_concepts.join(", ")
        }
    }
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

fn push_context_line(lines: &mut Vec<String>, label: &str, value: &str) {
    let trimmed = value.trim();
    if !trimmed.is_empty() {
        lines.push(format!("{label}: {trimmed}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context() -> WorkspaceResearchContext {
        WorkspaceResearchContext {
            name: "Diabetes chatbot evidence map".to_string(),
            primary_question: "Do conversational agents improve diabetes self-management?"
                .to_string(),
            gap_note: "Separate education from counseling.".to_string(),
            refined_question: "Do LLM counseling chatbots improve HbA1c safely?".to_string(),
            seed_concepts: vec!["type 2 diabetes".to_string(), "chatbot".to_string()],
            override_queries: Vec::new(),
            topic_descriptor: "diabetes chatbot self-management".to_string(),
            lookback_days: 365,
        }
    }

    #[test]
    fn seed_concepts_are_query_terms_without_overrides() {
        let context = context();

        assert_eq!(context.query_source_label(), "seed concepts");
        assert_eq!(
            context.query_terms(),
            &["type 2 diabetes".to_string(), "chatbot".to_string()]
        );
    }

    #[test]
    fn override_queries_win_over_seed_concepts() {
        let mut context = context();
        context.override_queries = vec!["diabetes chatbot HbA1c".to_string()];

        assert_eq!(context.query_source_label(), "override queries");
        assert_eq!(
            context.query_terms(),
            &["diabetes chatbot HbA1c".to_string()]
        );
    }

    #[test]
    fn empty_queries_fall_back_to_source_defaults() {
        let mut context = context();
        context.seed_concepts.clear();
        context.override_queries.clear();

        assert_eq!(context.query_source_label(), "source defaults");
        assert!(context.query_terms().is_empty());
        assert_eq!(context.query_preview(3), "source defaults");
    }

    #[test]
    fn collection_context_includes_research_framing_but_not_gap_note() {
        let text = context().collection_context();

        assert!(text.contains("Diabetes chatbot evidence map"));
        assert!(text.contains("diabetes chatbot self-management"));
        assert!(text.contains("Do conversational agents improve diabetes self-management?"));
        assert!(text.contains("Do LLM counseling chatbots improve HbA1c safely?"));
        assert!(text.contains("type 2 diabetes, chatbot"));
        assert!(!text.contains("Separate education from counseling."));
    }
}
