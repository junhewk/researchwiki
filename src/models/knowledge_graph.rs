use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Debug, Deserialize)]
pub struct KGQueryRequest {
    pub query: String,
    #[serde(default = "default_query_mode")]
    pub mode: String,
}

#[derive(Debug, Serialize)]
pub struct KGSearchEntity {
    pub name: String,
    pub entity_type: String,
    pub description: Option<String>,
    pub mention_count: i64,
    pub similarity: Option<f64>,
    pub aliases: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub synthesis_summary: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct KGSearchRelationship {
    pub source: String,
    pub target: String,
    pub relationship_type: String,
    pub weight: f64,
    pub article_count: i64,
}

#[derive(Debug, Serialize)]
pub struct KGSearchSource {
    pub article_uid: String,
    pub title: String,
    pub url: Option<String>,
    pub chunk_content: String,
    pub similarity: f64,
    pub chunk_reference: String,
}

#[derive(Debug, Serialize)]
pub struct KGQueryResponse {
    pub success: bool,
    pub mode: String,
    pub query: String,
    pub entities: Vec<KGSearchEntity>,
    pub relationships: Vec<KGSearchRelationship>,
    pub context: Option<String>,
    pub sources: Vec<KGSearchSource>,
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct KGStatsResponse {
    pub nodes: i64,
    pub edges: i64,
    pub entity_types: std::collections::BTreeMap<String, i64>,
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct KGGraphNode {
    pub id: String,
    pub labels: Vec<String>,
    pub properties: Map<String, Value>,
}

#[derive(Debug, Serialize)]
pub struct KGGraphEdge {
    pub source: String,
    pub target: String,
    pub properties: Map<String, Value>,
}

#[derive(Debug, Serialize)]
pub struct KGGraphDataResponse {
    pub nodes: Vec<KGGraphNode>,
    pub edges: Vec<KGGraphEdge>,
    pub error: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct KGGraphDataQuery {
    #[serde(default = "default_graph_limit")]
    pub limit: u32,
    #[serde(default)]
    pub min_degree: u32,
    pub entity_types: Option<String>,
    pub layout: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct KGEntityRequest {
    pub entity: String,
}

#[derive(Debug, Serialize)]
pub struct KGEntityNeighbor {
    pub entity: String,
    pub entity_type: String,
    pub relationship: String,
    pub weight: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence_summary: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct KGEntityResponse {
    pub entity: String,
    pub found: bool,
    pub entity_type: Option<String>,
    pub description: Option<String>,
    pub mention_count: Option<i64>,
    pub aliases: Vec<String>,
    pub neighbors: Vec<KGEntityNeighbor>,
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub synthesis_summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub synthesis_content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub synthesis_stale: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub synthesis_key_aspects: Option<Vec<String>>,
}

#[derive(Debug, Serialize, Clone, Default)]
pub struct KGBackfillStatusResponse {
    pub running: bool,
    pub processed: i64,
    pub inserted: i64,
    pub failed: i64,
    pub total: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_article_uid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_article_title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_article_index: Option<i64>,
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct KGBackfillStartResponse {
    pub status: String,
    pub message: String,
    pub total_articles: i64,
}

#[derive(Debug, Serialize, Clone, Default)]
pub struct KGBackfillOverview {
    pub kg_total_articles: i64,
    pub kg_completed_articles: i64,
    pub kg_remaining_articles: i64,
    pub wiki_total_entities: i64,
    pub wiki_compiled_entities: i64,
    pub wiki_pending_entities: i64,
}

#[derive(Debug, Deserialize)]
pub struct KGFullBackfillRequest {
    #[serde(default = "default_synthesis_batch")]
    pub kg_batch_size: u32,
    #[serde(default = "default_synthesis_batch")]
    pub wiki_batch_size: u32,
}

#[derive(Debug, Serialize)]
pub struct KGFullBackfillStartResponse {
    pub status: String,
    pub message: String,
}

#[derive(Debug, Serialize, Clone, Default)]
pub struct KGFullBackfillStatus {
    pub running: bool,
    pub stop_requested: bool,
    pub phase: String,
    pub kg_batches: i64,
    pub kg_processed: i64,
    pub kg_inserted: i64,
    pub kg_failed: i64,
    pub wiki_batches: i64,
    pub wiki_processed: i64,
    pub wiki_compiled: i64,
    pub wiki_failed: i64,
    pub message: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct KGInsertRequest {
    pub uids: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct KGInsertResult {
    pub uid: String,
    pub success: bool,
    pub entities: i64,
    pub relationships: i64,
    pub chunks: i64,
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct KGInsertResponse {
    pub total: usize,
    pub inserted: usize,
    pub failed: usize,
    pub results: Vec<KGInsertResult>,
}

#[derive(Debug, Serialize)]
pub struct KGArticleEntityItem {
    pub entity_id: i64,
    pub entity: String,
    pub entity_type: String,
    pub mention_text: Option<String>,
    pub context: Option<String>,
    pub chunk_index: i64,
}

#[derive(Debug, Serialize)]
pub struct KGArticleEntitiesResponse {
    pub uid: String,
    pub entities: Vec<KGArticleEntityItem>,
    pub count: usize,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ChunkExtraction {
    #[serde(default)]
    pub entities: Vec<ExtractedEntity>,
    #[serde(default)]
    pub relationships: Vec<ExtractedRelationship>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ExtractedEntity {
    pub name: String,
    pub entity_type: String,
    pub description: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ExtractedRelationship {
    pub source: String,
    pub target: String,
    pub relationship: String,
    pub description: String,
}

#[derive(Debug, Deserialize)]
pub struct EntityVerificationResult {
    #[serde(default)]
    pub same_entity: bool,
    #[serde(default)]
    pub confidence: f64,
    #[serde(default)]
    pub reasoning: String,
}

fn default_query_mode() -> String {
    "hybrid".to_string()
}

fn default_graph_limit() -> u32 {
    200
}

// --- Entity Synthesis (Auto-Wiki) Models ---

#[derive(Debug, Serialize)]
pub struct KGEntitySynthesis {
    pub entity_id: i64,
    pub entity_name: String,
    pub entity_type: String,
    pub summary: String,
    pub synthesis: String,
    pub key_aspects: Vec<String>,
    pub related_entities: Vec<KGSynthesisRelatedEntity>,
    pub source_article_count: i64,
    pub compiled_at: Option<String>,
    pub stale: bool,
    pub version: i64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct KGSynthesisRelatedEntity {
    pub name: String,
    pub relationship_type: String,
    pub entity_type: String,
}

#[derive(Debug, Serialize)]
pub struct KGEntitySynthesisSummary {
    pub entity_id: i64,
    pub entity_name: String,
    pub entity_type: String,
    pub summary: String,
    pub source_article_count: i64,
    pub stale: bool,
    pub compiled_at: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct KGSynthesisCompileRequest {
    #[serde(default = "default_synthesis_batch")]
    pub batch_size: u32,
    #[serde(default)]
    pub force_all: bool,
    pub entity_ids: Option<Vec<i64>>,
}

#[derive(Debug, Serialize, Clone, Default)]
pub struct KGSynthesisCompileStatus {
    pub running: bool,
    pub processed: i64,
    pub compiled: i64,
    pub failed: i64,
    pub total: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_entity_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_entity_index: Option<i64>,
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct KGSynthesisCompileStartResponse {
    pub status: String,
    pub message: String,
    pub total_entities: i64,
}

#[derive(Debug, Serialize)]
pub struct KGSynthesisListResponse {
    pub syntheses: Vec<KGEntitySynthesisSummary>,
    pub total: i64,
    pub stale_count: i64,
}

#[derive(Debug, Deserialize)]
pub struct KGSynthesisListQuery {
    #[serde(default = "default_synthesis_list_limit")]
    pub limit: u32,
    #[serde(default)]
    pub offset: u32,
    #[serde(default)]
    pub stale_only: bool,
    pub entity_type: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct KGSynthesisSearchRequest {
    pub query: String,
    #[serde(default = "default_synthesis_search_limit")]
    pub limit: u32,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct KGGapAnalysisResult {
    pub entity_name: String,
    pub issue_type: String,
    pub suggestion: String,
    pub confidence: f64,
}

#[derive(Debug, Serialize)]
pub struct KGGapAnalysisResponse {
    pub issues: Vec<KGGapAnalysisResult>,
    pub entities_reviewed: i64,
}

// LLM output deserialization
#[derive(Debug, Deserialize)]
pub struct SynthesisGenerationOutput {
    pub summary: String,
    pub synthesis: String,
    #[serde(default)]
    pub key_aspects: Vec<String>,
    #[serde(default)]
    pub related_entities: Vec<KGSynthesisRelatedEntity>,
}

#[derive(Debug, Deserialize)]
pub struct RelationshipEvidenceOutput {
    pub evidence_summary: String,
}

fn default_synthesis_batch() -> u32 {
    20
}
fn default_synthesis_list_limit() -> u32 {
    50
}
fn default_synthesis_search_limit() -> u32 {
    20
}
