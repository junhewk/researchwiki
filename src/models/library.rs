use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchMode {
    Semantic,
    Keyword,
    Hybrid,
    HybridRerank,
    Hyde,
    MultiQuery,
    Graph,
}

impl Default for SearchMode {
    fn default() -> Self {
        Self::Semantic
    }
}

#[derive(Debug, Serialize)]
pub struct LibraryStats {
    pub total_articles: i64,
    pub articles_with_embeddings: i64,
    pub total_chunks: i64,
    pub avg_chunks_per_article: f64,
    pub total_tokens_embedded: i64,
}

#[derive(Debug, Serialize)]
pub struct ChunkResponse {
    pub id: i64,
    pub chunk_index: i64,
    pub chunk_type: String,
    pub content: String,
    pub token_count: Option<i64>,
    pub source_page: Option<i64>,
    pub source_section: Option<String>,
    pub has_embedding: bool,
}

#[derive(Debug, Deserialize)]
pub struct ProcessArticleRequest {
    pub article_uid: String,
}

#[derive(Debug, Serialize)]
pub struct ProcessArticleResponse {
    pub article_uid: String,
    pub success: bool,
    pub chunks_created: i32,
    pub error: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct BackfillRequest {
    #[serde(default = "default_batch_size")]
    pub batch_size: i32,
}

#[derive(Debug, Serialize)]
pub struct BackfillResponse {
    pub processed: i32,
    pub failed: i32,
    pub errors: Vec<String>,
    pub remaining: i32,
}

fn default_batch_size() -> i32 {
    10
}

#[derive(Debug, Deserialize)]
pub struct SearchRequest {
    pub query: String,
    #[serde(default = "default_search_limit")]
    pub limit: i32,
    #[serde(default)]
    pub mode: SearchMode,
    pub min_score: Option<f64>,
    pub date_from: Option<String>,
    pub date_to: Option<String>,
    pub categories: Option<Vec<String>>,
    #[serde(default = "default_rrf_k")]
    pub rrf_k: i32,
}

#[derive(Debug, Serialize)]
pub struct SearchResultItem {
    pub article_uid: String,
    pub chunk_id: i64,
    pub content: String,
    pub similarity: f64,
    pub title: Option<String>,
    pub first_author: Option<String>,
    pub pub_date: Option<String>,
    pub url: Option<String>,
    pub chunk_type: String,
    pub source_page: Option<i64>,
    pub source_section: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct SearchResponse {
    pub query: String,
    pub results: Vec<SearchResultItem>,
    pub total_found: i32,
    pub search_time_ms: i64,
    pub mode: SearchMode,
}

#[derive(Debug, Deserialize)]
pub struct ContextRequest {
    pub query: String,
    #[serde(default = "default_context_limit")]
    pub limit: i32,
    #[serde(default = "default_max_tokens")]
    pub max_tokens: i32,
}

#[derive(Debug, Serialize)]
pub struct ContextResponse {
    pub query: String,
    pub context: String,
    pub sources: Vec<SourceCitation>,
    pub total_tokens: i32,
}

#[derive(Debug, Serialize)]
pub struct SourceCitation {
    pub article_uid: String,
    pub title: Option<String>,
    pub url: Option<String>,
    pub chunk_reference: String,
    pub similarity: f64,
}

fn default_search_limit() -> i32 {
    10
}
fn default_rrf_k() -> i32 {
    60
}
fn default_context_limit() -> i32 {
    5
}
fn default_max_tokens() -> i32 {
    4000
}
