use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
pub struct ArticleListQuery {
    #[serde(default = "default_page")]
    pub page: u32,
    #[serde(default = "default_page_size")]
    pub page_size: u32,
    pub date_from: Option<String>,
    pub date_to: Option<String>,
    pub category: Option<String>,
    pub search: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ArticleUpdate {
    #[serde(flatten)]
    pub fields: serde_json::Map<String, serde_json::Value>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ArticleResponse {
    pub uid: String,
    pub title: Option<String>,
    pub url: Option<String>,
    pub category: Option<String>,
    pub first_author: Option<String>,
    pub authors: Option<String>,
    pub pub_date: Option<String>,
    pub journal: Option<String>,
    pub ai_tech: Option<String>,
    pub clinical_domain: Option<String>,
    pub ethics_framework: Option<String>,
    pub primary_issue: Option<String>,
    pub key_stakeholders: Option<String>,
    pub practical_impl: Option<String>,
    pub secondary_issues: Option<String>,
    pub key_argument: Option<String>,
    pub main_findings: Option<String>,
    pub normative_claims: Option<String>,
    pub limitations: Option<String>,
    pub theoretical_strengths: Option<String>,
    pub theoretical_weaknesses: Option<String>,
    pub empirical_strengths: Option<String>,
    pub empirical_weaknesses: Option<String>,
    pub byline_summary: Option<String>,
    pub why_it_matters: Option<String>,
    pub evaluated_at: Option<String>,
    pub content_type: Option<String>,
    pub pdf_path: Option<String>,
    pub reg_date: Option<String>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ArticleListResponse {
    pub items: Vec<ArticleResponse>,
    pub total: i64,
    pub page: u32,
    pub page_size: u32,
    pub pages: u32,
}

#[derive(Debug, Serialize)]
pub struct ArticleStats {
    pub total_articles: i64,
    pub this_week: i64,
    pub evaluated_count: i64,
    pub pending_evaluation: i64,
}

#[derive(Debug, Deserialize)]
pub struct DaysQuery {
    #[serde(default = "default_days")]
    pub days: u32,
}

#[derive(Debug, Deserialize)]
pub struct RecentArticlesQuery {
    #[serde(default = "default_recent_days")]
    pub days: u32,
    #[serde(default = "default_recent_limit")]
    pub limit: u32,
}

#[derive(Debug, Deserialize)]
pub struct TopArticlesQuery {
    #[serde(default = "default_recent_days")]
    pub days: u32,
    #[serde(default = "default_top_limit")]
    pub limit: u32,
}

#[derive(Debug, Serialize)]
pub struct DailyCount {
    pub date: String,
    pub count: i64,
}

#[derive(Debug, Serialize)]
pub struct DailyStatsResponse {
    pub days: Vec<DailyCount>,
    pub total: i64,
}

fn default_page() -> u32 {
    1
}

fn default_page_size() -> u32 {
    20
}

fn default_days() -> u32 {
    30
}

fn default_recent_days() -> u32 {
    7
}

fn default_recent_limit() -> u32 {
    10
}

fn default_top_limit() -> u32 {
    7
}
