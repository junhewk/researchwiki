use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

use chrono::Local;
use rusqlite::{
    Connection, OptionalExtension, params, params_from_iter,
    types::{Value, ValueRef},
};
use serde_json::{Map, Value as JsonValue, json};
use tracing::{error, info, warn};
use zerocopy::IntoBytes;

use crate::{
    error::{AppError, run_blocking},
    models::knowledge_graph::{
        ChunkExtraction, EntityVerificationResult, ExtractedEntity, KGArticleEntitiesResponse,
        KGArticleEntityItem, KGBackfillStartResponse, KGBackfillStatusResponse, KGEntityNeighbor,
        KGEntityResponse, KGEntitySynthesis, KGEntitySynthesisSummary, KGFullBackfillStartResponse,
        KGFullBackfillStatus, KGGapAnalysisResponse, KGGapAnalysisResult, KGGraphDataQuery,
        KGGraphDataResponse, KGGraphEdge, KGGraphNode, KGInsertResponse, KGInsertResult,
        KGQueryRequest, KGQueryResponse, KGSearchEntity, KGSearchRelationship, KGSearchSource,
        KGStatsResponse, KGSynthesisCompileStartResponse, KGSynthesisCompileStatus,
        KGSynthesisListQuery, KGSynthesisListResponse, KGSynthesisRelatedEntity,
        RelationshipEvidenceOutput, SynthesisGenerationOutput,
    },
    models::workspace::WorkspaceResearchContext,
    services::{
        embedding::EmbeddingService,
        fts::build_fts_query,
        llm::{LlmOutputMode, LlmService},
        workspace::WorkspaceService,
    },
};

/// Subquery (one positional `?` = workspace_id) selecting entity ids that are
/// mentioned by at least one article in the given workspace. Entities are
/// globally deduplicated, so workspace membership is resolved via the article
/// join rather than a column on `kg_entities`.
const WS_ENTITY_SCOPE: &str = "(SELECT kae.entity_id FROM kg_article_entities kae \
     JOIN haie_rev h ON h.uid = kae.article_uid WHERE h.workspace_id = ?)";

/// Budget for the text handed to chunked entity extraction. Sized so the
/// bounded text still fits inside [`KG_MAX_CHUNKS`] 450-word chunks
/// (~24k chars ≈ 4k words ≈ 11 chunks) — raising it past that silently drops
/// the tail again.
const KG_TEXT_MAX_CHARS: usize = 24_000;
/// Portion of [`KG_TEXT_MAX_CHARS`] reserved for the document tail when the
/// text is longer than the budget, so conclusions/limitations still reach the
/// extractor instead of only the front matter.
const KG_TEXT_TAIL_CHARS: usize = 8_000;
const KG_CHUNK_WORDS: usize = 450;
const KG_CHUNK_OVERLAP_WORDS: usize = 60;
const KG_MAX_CHUNKS: usize = 12;
const HIGH_CONFIDENCE_THRESHOLD: f32 = 0.90;
const AMBIGUOUS_MIN_THRESHOLD: f32 = 0.80;
const ENTITY_VERIFICATION_CONFIDENCE_THRESHOLD: f64 = 0.82;
const WIKI_MIN_SOURCE_ARTICLES: i64 = 3;

const WIKI_EXCLUDED_ENTITY_NAMES: &[&str] = &[
    "2024",
    "2025",
    "2026",
    "article",
    "articles",
    "author",
    "authors",
    "cc by",
    "cc by 4.0",
    "copyright",
    "creative commons",
    "creative commons attribution license",
    "creative commons attribution license cc-by 4.0",
    "creative commons license",
    "epub",
    "journal",
    "open access",
    "paper",
    "papers",
    "pmc",
    "pmc-last-change",
    "pmc-live",
    "pmc-release",
    "publication",
    "publications",
    "pubmed",
    "research article",
    "journal article",
    "jmir publications inc.",
    "nature publishing group",
    "scientific reports",
    "springer",
    "source",
    "sources",
];

const WIKI_ENTITY_FILTER_SQL: &str = r#"
    UPPER(COALESCE(e.entity_type, '')) NOT IN ('PERSON')
    AND LOWER(TRIM(e.canonical_name)) NOT IN (
        '2024',
        '2025',
        '2026',
        'article',
        'articles',
        'author',
        'authors',
        'cc by',
        'cc by 4.0',
        'copyright',
        'creative commons',
        'creative commons attribution license',
        'creative commons attribution license cc-by 4.0',
        'creative commons license',
        'epub',
        'journal',
        'open access',
        'paper',
        'papers',
        'pmc',
        'pmc-last-change',
        'pmc-live',
        'pmc-release',
        'publication',
        'publications',
        'pubmed',
        'research article',
        'journal article',
        'jmir publications inc.',
        'nature publishing group',
        'scientific reports',
        'springer',
        'source',
        'sources'
    )
    AND LOWER(TRIM(e.canonical_name)) NOT GLOB '[12][0-9][0-9][0-9]'
    AND LOWER(TRIM(e.canonical_name)) NOT GLOB 'pmc[0-9]*'
    AND LOWER(TRIM(e.canonical_name)) NOT GLOB 'pubmed[0-9]*'
    AND LOWER(TRIM(e.canonical_name)) NOT GLOB 'pmid[0-9]*'
    AND LOWER(TRIM(e.canonical_name)) NOT GLOB 'volume [0-9]*'
    AND LOWER(TRIM(e.canonical_name)) NOT GLOB 'issue [0-9]*'
    AND NOT (
        LOWER(TRIM(e.canonical_name)) GLOB '[0-3][0-9] * [12][0-9][0-9][0-9]'
        AND (
            LOWER(TRIM(e.canonical_name)) LIKE '%january%'
            OR LOWER(TRIM(e.canonical_name)) LIKE '%february%'
            OR LOWER(TRIM(e.canonical_name)) LIKE '%march%'
            OR LOWER(TRIM(e.canonical_name)) LIKE '%april%'
            OR LOWER(TRIM(e.canonical_name)) LIKE '%may%'
            OR LOWER(TRIM(e.canonical_name)) LIKE '%june%'
            OR LOWER(TRIM(e.canonical_name)) LIKE '%july%'
            OR LOWER(TRIM(e.canonical_name)) LIKE '%august%'
            OR LOWER(TRIM(e.canonical_name)) LIKE '%september%'
            OR LOWER(TRIM(e.canonical_name)) LIKE '%october%'
            OR LOWER(TRIM(e.canonical_name)) LIKE '%november%'
            OR LOWER(TRIM(e.canonical_name)) LIKE '%december%'
        )
    )
    AND LOWER(TRIM(e.canonical_name)) NOT LIKE 'doi:%'
    AND LOWER(TRIM(e.canonical_name)) NOT LIKE 'license:%'
"#;

#[derive(Clone)]
pub struct KnowledgeGraphService {
    database_path: Arc<PathBuf>,
    wiki_export_dir: Arc<PathBuf>,
    workspace_id: i64,
    workspace_service: Arc<WorkspaceService>,
    llm_service: Arc<LlmService>,
    embedding_service: Arc<EmbeddingService>,
    backfill_state: Arc<Mutex<KGBackfillStatusResponse>>,
    synthesis_compile_state: Arc<Mutex<KGSynthesisCompileStatus>>,
    full_backfill_state: Arc<Mutex<KGFullBackfillStatus>>,
}

#[derive(Debug)]
struct MatchedEntity {
    id: i64,
    name: String,
    entity_type: String,
    description: Option<String>,
    mention_count: i64,
    aliases: Vec<String>,
    similarity: Option<f64>,
    synthesis_summary: Option<String>,
}

#[derive(Clone, Debug)]
struct ArticleInput {
    uid: String,
    title: Option<String>,
    full_text: Option<String>,
    content_type: Option<String>,
    byline_summary: Option<String>,
}

impl ArticleInput {
    fn kg_text(&self) -> String {
        // full_text may hold raw PMC XML or scraped HTML; extract plain text
        // before chunking so the LLM never sees markup.
        let extracted = self
            .full_text
            .as_deref()
            .map(|text| {
                crate::services::text_extractor::extract_from_content(
                    text,
                    self.content_type.as_deref().unwrap_or("text"),
                )
                .full_text
            })
            .filter(|text| !text.trim().is_empty());
        prepare_kg_text(
            self.title.as_deref(),
            extracted.as_deref().or(self.byline_summary.as_deref()),
        )
    }
}

#[derive(Clone, Debug)]
struct StoredEntity {
    id: i64,
    canonical_name: String,
    entity_type: String,
    description: Option<String>,
    mention_count: i64,
    aliases: Vec<String>,
}

#[derive(Clone, Debug)]
struct CachedEmbedding {
    entity_id: i64,
    vector: Vec<f32>,
    norm: f32,
}

#[derive(Clone, Debug)]
struct ResolutionCandidate {
    entity: StoredEntity,
    similarity: f32,
}

#[derive(Default)]
struct EntityEngram {
    by_id: HashMap<i64, StoredEntity>,
    by_name: HashMap<String, i64>,
    by_alias: HashMap<String, i64>,
    embeddings_by_type: HashMap<String, Vec<CachedEmbedding>>,
}

#[derive(Debug)]
struct InsertOutcome {
    entities: i64,
    relationships: i64,
    chunks: i64,
    /// Chunks whose extraction failed even after a retry. The article is left
    /// unmarked so the next backfill resumes the missing chunks.
    failed_chunks: i64,
}

impl EntityEngram {
    fn from_entities(entities: Vec<StoredEntity>) -> Self {
        let mut engram = Self::default();
        for entity in entities {
            engram.insert_metadata(entity);
        }
        engram
    }

    fn find_by_name(&self, name: &str) -> Option<i64> {
        self.by_name.get(&normalize_name(name)).copied()
    }

    fn find_by_alias(&self, name: &str) -> Option<i64> {
        self.by_alias.get(&normalize_name(name)).copied()
    }

    fn insert_metadata(&mut self, entity: StoredEntity) {
        let entity_id = entity.id;
        self.by_name
            .insert(normalize_name(&entity.canonical_name), entity_id);
        for alias in &entity.aliases {
            self.by_alias.insert(normalize_name(alias), entity_id);
        }
        self.by_id.insert(entity_id, entity);
    }

    fn register_alias(&mut self, entity_id: i64, alias: &str) {
        let normalized = normalize_name(alias);
        if normalized.is_empty() {
            return;
        }

        if let Some(entity) = self.by_id.get_mut(&entity_id) {
            if !entity
                .aliases
                .iter()
                .any(|existing| normalize_name(existing) == normalized)
            {
                entity.aliases.push(alias.trim().to_string());
            }
        }
        self.by_alias.insert(normalized, entity_id);
    }

    fn add_entity_with_embedding(&mut self, entity: StoredEntity, embedding: Vec<f32>) {
        let entity_type = normalize_entity_type(&entity.entity_type);
        let entity_id = entity.id;
        self.insert_metadata(entity);
        self.embeddings_by_type
            .entry(entity_type)
            .or_default()
            .push(CachedEmbedding {
                entity_id,
                norm: vector_norm(&embedding),
                vector: embedding,
            });
    }

    async fn ensure_type_embeddings(
        &mut self,
        service: &KnowledgeGraphService,
        entity_type: &str,
    ) -> Result<(), AppError> {
        let key = normalize_entity_type(entity_type);
        if self.embeddings_by_type.contains_key(&key) {
            return Ok(());
        }

        let embeddings = service.load_embeddings_for_type(&key).await?;
        self.embeddings_by_type.insert(key, embeddings);
        Ok(())
    }

    async fn find_similar(
        &mut self,
        service: &KnowledgeGraphService,
        embedding: &[f32],
        threshold: f32,
        entity_type: &str,
    ) -> Result<Option<ResolutionCandidate>, AppError> {
        self.ensure_type_embeddings(service, entity_type).await?;
        let key = normalize_entity_type(entity_type);
        let query_norm = vector_norm(embedding);
        let mut best: Option<ResolutionCandidate> = None;

        if let Some(candidates) = self.embeddings_by_type.get(&key) {
            for candidate in candidates {
                let similarity =
                    cosine_similarity(embedding, query_norm, &candidate.vector, candidate.norm);
                if similarity < threshold {
                    continue;
                }
                let Some(entity) = self.by_id.get(&candidate.entity_id) else {
                    continue;
                };
                let should_replace = best
                    .as_ref()
                    .map(|current| similarity > current.similarity)
                    .unwrap_or(true);
                if should_replace {
                    best = Some(ResolutionCandidate {
                        entity: entity.clone(),
                        similarity,
                    });
                }
            }
        }

        Ok(best)
    }

    async fn find_candidates(
        &mut self,
        service: &KnowledgeGraphService,
        embedding: &[f32],
        entity_type: &str,
        limit: usize,
    ) -> Result<Vec<ResolutionCandidate>, AppError> {
        self.ensure_type_embeddings(service, entity_type).await?;
        let key = normalize_entity_type(entity_type);
        let query_norm = vector_norm(embedding);
        let mut matches = Vec::new();

        if let Some(candidates) = self.embeddings_by_type.get(&key) {
            for candidate in candidates {
                let similarity =
                    cosine_similarity(embedding, query_norm, &candidate.vector, candidate.norm);
                if !(AMBIGUOUS_MIN_THRESHOLD..HIGH_CONFIDENCE_THRESHOLD).contains(&similarity) {
                    continue;
                }
                let Some(entity) = self.by_id.get(&candidate.entity_id) else {
                    continue;
                };
                matches.push(ResolutionCandidate {
                    entity: entity.clone(),
                    similarity,
                });
            }
        }

        matches.sort_by(|left, right| {
            right
                .similarity
                .partial_cmp(&left.similarity)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| right.entity.mention_count.cmp(&left.entity.mention_count))
                .then_with(|| left.entity.canonical_name.cmp(&right.entity.canonical_name))
        });
        matches.truncate(limit);
        Ok(matches)
    }
}

impl KnowledgeGraphService {
    pub fn new(
        database_path: PathBuf,
        wiki_export_dir: PathBuf,
        workspace_id: i64,
        workspace_service: Arc<WorkspaceService>,
        llm_service: Arc<LlmService>,
        embedding_service: Arc<EmbeddingService>,
    ) -> Self {
        Self {
            database_path: Arc::new(database_path),
            wiki_export_dir: Arc::new(wiki_export_dir),
            workspace_id,
            workspace_service,
            llm_service,
            embedding_service,
            backfill_state: Arc::new(Mutex::new(KGBackfillStatusResponse::default())),
            synthesis_compile_state: Arc::new(Mutex::new(KGSynthesisCompileStatus::default())),
            full_backfill_state: Arc::new(Mutex::new(KGFullBackfillStatus::default())),
        }
    }

    async fn research_context(&self) -> WorkspaceResearchContext {
        self.workspace_service
            .research_context(self.workspace_id)
            .await
            .unwrap_or_default()
    }

    pub async fn query(&self, request: KGQueryRequest) -> Result<KGQueryResponse, AppError> {
        let database_path = self.database_path.clone();

        run_blocking(move || {
            let conn = crate::db::open_connection(&*database_path)?;
            let query = request.query.trim().to_string();
            let matched = query_entities(&conn, &query, 12)?;
            let entity_ids = matched.iter().map(|entity| entity.id).collect::<Vec<_>>();

            let entities = matched
                .into_iter()
                .map(|entity| KGSearchEntity {
                    name: entity.name,
                    entity_type: entity.entity_type,
                    description: entity.description,
                    mention_count: entity.mention_count,
                    similarity: entity.similarity,
                    aliases: entity.aliases,
                    synthesis_summary: entity.synthesis_summary,
                })
                .collect::<Vec<_>>();

            let relationships = query_relationships(&conn, &entity_ids, 40)?;
            let sources = query_sources(&conn, &entity_ids, 8)?;
            let context = build_context(&sources);

            Ok(KGQueryResponse {
                success: true,
                mode: request.mode,
                query,
                entities,
                relationships,
                context,
                sources,
                error: None,
            })
        })
        .await
    }

    pub async fn get_stats(&self, workspace_id: i64) -> Result<KGStatsResponse, AppError> {
        let database_path = self.database_path.clone();

        run_blocking(move || {
            let conn = crate::db::open_connection(&*database_path)?;
            let nodes = conn.query_row(
                &format!("SELECT COUNT(*) FROM kg_entities WHERE id IN {WS_ENTITY_SCOPE}"),
                [workspace_id],
                |row| row.get::<_, i64>(0),
            )?;
            let edges = conn.query_row(
                &format!(
                    "SELECT COUNT(*) FROM kg_relationships
                     WHERE source_entity_id IN {WS_ENTITY_SCOPE}
                       AND target_entity_id IN {WS_ENTITY_SCOPE}"
                ),
                [workspace_id, workspace_id],
                |row| row.get::<_, i64>(0),
            )?;

            let mut stmt = conn.prepare(&format!(
                "SELECT entity_type, COUNT(*) AS count
                 FROM kg_entities
                 WHERE id IN {WS_ENTITY_SCOPE}
                 GROUP BY entity_type
                 ORDER BY entity_type"
            ))?;
            let rows = stmt.query_map([workspace_id], |row| {
                let entity_type: Option<String> = row.get(0)?;
                let count: i64 = row.get(1)?;
                Ok((entity_type.unwrap_or_else(|| "UNKNOWN".to_string()), count))
            })?;

            let mut entity_types = std::collections::BTreeMap::new();
            for row in rows {
                let (entity_type, count) = row?;
                entity_types.insert(entity_type, count);
            }

            Ok(KGStatsResponse {
                nodes,
                edges,
                entity_types,
                error: None,
            })
        })
        .await
    }

    pub async fn get_graph_data(
        &self,
        query: KGGraphDataQuery,
        workspace_id: i64,
    ) -> Result<KGGraphDataResponse, AppError> {
        let database_path = self.database_path.clone();

        run_blocking(move || {
            let conn = crate::db::open_connection(&*database_path)?;
            let limit = i64::from(query.limit.clamp(1, 2000));
            let min_degree = i64::from(query.min_degree.min(100));
            let entity_types = parse_entity_types(query.entity_types);

            // Workspace filter first so its `?` aligns with the leading param.
            let mut params = vec![Value::Integer(workspace_id), Value::Integer(min_degree)];
            let mut filters = vec![
                format!("e.id IN {WS_ENTITY_SCOPE}"),
                "COALESCE(d.degree, 0) >= ?".to_string(),
            ];

            if !entity_types.is_empty() {
                let placeholders = (0..entity_types.len())
                    .map(|_| "?".to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                filters.push(format!("UPPER(e.entity_type) IN ({placeholders})"));
                params.extend(entity_types.iter().cloned().map(Value::Text));
            }

            params.push(Value::Integer(limit));

            let where_clause = filters.join(" AND ");
            let sql = format!(
                "
                WITH degrees AS (
                    SELECT entity_id, COUNT(*) AS degree
                    FROM (
                        SELECT source_entity_id AS entity_id FROM kg_relationships
                        UNION ALL
                        SELECT target_entity_id AS entity_id FROM kg_relationships
                    )
                    GROUP BY entity_id
                )
                SELECT e.id, e.canonical_name, e.entity_type, e.description, e.mention_count,
                       COALESCE(d.degree, 0) AS degree
                FROM kg_entities e
                LEFT JOIN degrees d ON d.entity_id = e.id
                WHERE {where_clause}
                ORDER BY COALESCE(d.degree, 0) DESC, e.mention_count DESC, e.canonical_name ASC
                LIMIT ?
                "
            );

            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map(params_from_iter(params.iter()), |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?
                        .unwrap_or_else(|| "UNKNOWN".to_string()),
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<i64>>(4)?.unwrap_or(0),
                    row.get::<_, i64>(5)?,
                ))
            })?;

            let mut node_ids = Vec::new();
            let mut node_names = Vec::new();
            let mut nodes = Vec::new();

            for row in rows {
                let (id, name, entity_type, description, mention_count, degree) = row?;
                node_ids.push(id);
                node_names.push(name.clone());
                nodes.push(KGGraphNode {
                    id: name.clone(),
                    labels: vec![entity_type.clone()],
                    properties: json_object([
                        ("entity_type", JsonValue::String(entity_type)),
                        (
                            "description",
                            description
                                .map(JsonValue::String)
                                .unwrap_or(JsonValue::Null),
                        ),
                        ("mention_count", JsonValue::Number(mention_count.into())),
                        ("degree", JsonValue::Number(degree.into())),
                    ]),
                });
            }

            let edges = query_graph_edges(&conn, &node_ids, &node_names, (limit * 6).max(100))?;

            Ok(KGGraphDataResponse {
                nodes,
                edges,
                error: None,
            })
        })
        .await
    }

    pub async fn get_entity(&self, entity: &str) -> Result<KGEntityResponse, AppError> {
        let database_path = self.database_path.clone();
        let entity = entity.trim().to_string();

        run_blocking(move || {
            let conn = crate::db::open_connection(&*database_path)?;
            let entity_like = format!("%{}%", entity.to_lowercase());

            let row = conn
                .query_row(
                    "
                    SELECT id, canonical_name, entity_type, description, mention_count, aliases_json
                    FROM kg_entities
                    WHERE lower(canonical_name) = lower(?1)
                       OR lower(canonical_name) LIKE ?2
                       OR lower(COALESCE(aliases_json, '')) LIKE ?2
                    ORDER BY
                        CASE
                            WHEN lower(canonical_name) = lower(?1) THEN 0
                            WHEN lower(canonical_name) LIKE ?2 THEN 1
                            ELSE 2
                        END,
                        mention_count DESC,
                        canonical_name ASC
                    LIMIT 1
                    ",
                    [&entity as &dyn rusqlite::ToSql, &entity_like],
                    |row| {
                        Ok((
                            row.get::<_, i64>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, Option<String>>(2)?
                                .unwrap_or_else(|| "UNKNOWN".to_string()),
                            row.get::<_, Option<String>>(3)?,
                            row.get::<_, Option<i64>>(4)?.unwrap_or(0),
                            parse_string_list(row.get::<_, Option<String>>(5)?),
                        ))
                    },
                )
                .optional()?;

            let Some((entity_id, canonical_name, entity_type, description, mention_count, aliases)) =
                row
            else {
                return Ok(KGEntityResponse {
                    entity,
                    found: false,
                    entity_type: None,
                    description: None,
                    mention_count: None,
                    aliases: Vec::new(),
                    neighbors: Vec::new(),
                    error: None,
                    synthesis_summary: None,
                    synthesis_content: None,
                    synthesis_stale: None,
                    synthesis_key_aspects: None,
                });
            };

            let mut stmt = conn.prepare(
                "
                SELECT neighbor.canonical_name, neighbor.entity_type, rel.relationship_type, rel.weight,
                       rel.evidence_summary
                FROM kg_relationships rel
                JOIN kg_entities neighbor
                  ON neighbor.id = CASE
                        WHEN rel.source_entity_id = ?1 THEN rel.target_entity_id
                        ELSE rel.source_entity_id
                     END
                WHERE rel.source_entity_id = ?1 OR rel.target_entity_id = ?1
                ORDER BY rel.weight DESC, neighbor.mention_count DESC, neighbor.canonical_name ASC
                LIMIT 50
                ",
            )?;
            let neighbors = stmt
                .query_map([entity_id], |row| {
                    Ok(KGEntityNeighbor {
                        entity: row.get(0)?,
                        entity_type: row
                            .get::<_, Option<String>>(1)?
                            .unwrap_or_else(|| "UNKNOWN".to_string()),
                        relationship: row.get(2)?,
                        weight: row.get::<_, Option<f64>>(3)?.unwrap_or(1.0),
                        evidence_summary: row.get(4)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;

            // Fetch synthesis data if available
            let synthesis_row = conn
                .query_row(
                    "SELECT summary, synthesis, stale, key_aspects_json
                     FROM kg_entity_syntheses WHERE entity_id = ?",
                    [entity_id],
                    |row| {
                        Ok((
                            row.get::<_, Option<String>>(0)?,
                            row.get::<_, Option<String>>(1)?,
                            row.get::<_, Option<i64>>(2)?,
                            row.get::<_, Option<String>>(3)?,
                        ))
                    },
                )
                .optional()?;

            let (synthesis_summary, synthesis_content, synthesis_stale, synthesis_key_aspects) =
                if let Some((summary, synthesis, stale, key_aspects_json)) = synthesis_row {
                    let key_aspects = key_aspects_json
                        .and_then(|json| serde_json::from_str::<Vec<String>>(&json).ok());
                    (summary, synthesis, stale.map(|v| v != 0), key_aspects)
                } else {
                    (None, None, None, None)
                };

            Ok(KGEntityResponse {
                entity: canonical_name,
                found: true,
                entity_type: Some(entity_type),
                description,
                mention_count: Some(mention_count),
                aliases,
                neighbors,
                error: None,
                synthesis_summary,
                synthesis_content,
                synthesis_stale,
                synthesis_key_aspects,
            })
        })
        .await
    }

    pub async fn insert_articles(&self, uids: Vec<String>) -> Result<KGInsertResponse, AppError> {
        let context = self.research_context().await;
        self.insert_articles_with_context(uids, context).await
    }

    pub async fn insert_articles_with_context(
        &self,
        uids: Vec<String>,
        context: WorkspaceResearchContext,
    ) -> Result<KGInsertResponse, AppError> {
        if uids.is_empty() {
            return Ok(KGInsertResponse {
                total: 0,
                inserted: 0,
                failed: 0,
                results: Vec::new(),
            });
        }

        let articles = self.load_articles_by_uids(&uids).await?;
        let mut articles_by_uid = HashMap::new();
        for article in articles {
            articles_by_uid.insert(article.uid.clone(), article);
        }

        let mut engram = self.load_entity_engram().await?;
        let mut results = Vec::new();
        let mut inserted = 0usize;
        let mut failed = 0usize;

        for uid in uids {
            let Some(article) = articles_by_uid.remove(&uid) else {
                results.push(KGInsertResult {
                    uid,
                    success: false,
                    entities: 0,
                    relationships: 0,
                    chunks: 0,
                    error: Some("Article not found".to_string()),
                });
                failed += 1;
                continue;
            };

            let text = article.kg_text();
            if text.trim().is_empty() {
                results.push(KGInsertResult {
                    uid: article.uid,
                    success: false,
                    entities: 0,
                    relationships: 0,
                    chunks: 0,
                    error: Some("No text content".to_string()),
                });
                failed += 1;
                continue;
            }

            match self
                .insert_article_text(&article.uid, &text, &mut engram, &context)
                .await
            {
                Ok(outcome) => {
                    let complete = outcome.failed_chunks == 0;
                    results.push(KGInsertResult {
                        uid: article.uid,
                        success: complete,
                        entities: outcome.entities,
                        relationships: outcome.relationships,
                        chunks: outcome.chunks,
                        error: (!complete).then(|| {
                            format!(
                                "{} of {} chunks failed extraction; the article will be retried by the next backfill",
                                outcome.failed_chunks, outcome.chunks
                            )
                        }),
                    });
                    if complete {
                        inserted += 1;
                    } else {
                        failed += 1;
                    }
                }
                Err(error) => {
                    results.push(KGInsertResult {
                        uid: article.uid,
                        success: false,
                        entities: 0,
                        relationships: 0,
                        chunks: 0,
                        error: Some(error.to_string()),
                    });
                    failed += 1;
                }
            }
        }

        Ok(KGInsertResponse {
            total: results.len(),
            inserted,
            failed,
            results,
        })
    }

    pub async fn get_article_entities(
        &self,
        uid: &str,
    ) -> Result<KGArticleEntitiesResponse, AppError> {
        let database_path = self.database_path.clone();
        let uid = uid.to_string();

        run_blocking(move || {
            let conn = crate::db::open_connection(&*database_path)?;
            let mut stmt = conn.prepare(
                "
                SELECT kae.entity_id, e.canonical_name, e.entity_type, kae.mention_text, kae.context,
                       kae.chunk_index
                FROM kg_article_entities kae
                JOIN kg_entities e ON e.id = kae.entity_id
                WHERE kae.article_uid = ?1
                ORDER BY kae.chunk_index ASC, e.canonical_name ASC
                ",
            )?;
            let entities = stmt
                .query_map([uid.as_str()], |row| {
                    Ok(KGArticleEntityItem {
                        entity_id: row.get(0)?,
                        entity: row.get(1)?,
                        entity_type: row
                            .get::<_, Option<String>>(2)?
                            .unwrap_or_else(|| "UNKNOWN".to_string()),
                        mention_text: row.get(3)?,
                        context: row.get(4)?,
                        chunk_index: row.get::<_, Option<i64>>(5)?.unwrap_or(0),
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;

            Ok(KGArticleEntitiesResponse {
                uid,
                count: entities.len(),
                entities,
            })
        })
        .await
    }

    pub fn get_backfill_status(&self) -> Result<KGBackfillStatusResponse, AppError> {
        let state = self.backfill_state.lock().map_err(|_| {
            AppError::Internal("knowledge graph backfill state is poisoned".to_string())
        })?;
        Ok(state.clone())
    }

    pub async fn start_backfill(
        &self,
        batch_size: u32,
        offset: u32,
    ) -> Result<KGBackfillStartResponse, AppError> {
        let total_eligible = self.count_backfill_candidates().await?;
        let articles_remaining = (total_eligible - i64::from(offset)).max(0);
        let actual_batch = articles_remaining.min(i64::from(batch_size.max(1)));

        self.update_backfill(|state| {
            if state.running {
                return Err(AppError::Conflict(
                    "Backfill already in progress".to_string(),
                ));
            }
            *state = KGBackfillStatusResponse {
                running: true,
                processed: 0,
                inserted: 0,
                failed: 0,
                total: actual_batch,
                current_article_uid: None,
                current_article_title: None,
                current_article_index: None,
                error: None,
            };
            Ok(())
        })?;

        let service = self.clone();
        tokio::spawn(async move {
            let finisher = service.clone();
            let handle = tokio::spawn(async move {
                service.run_backfill(batch_size.max(1), offset).await;
            });

            if let Err(error) = handle.await {
                let message = if error.is_panic() {
                    "knowledge graph backfill worker panicked".to_string()
                } else {
                    format!("knowledge graph backfill worker stopped unexpectedly: {error}")
                };
                error!("{message}");
                let _ = finisher.finish_backfill(Some(message));
            }
        });

        Ok(KGBackfillStartResponse {
            status: "started".to_string(),
            message: format!(
                "Backfill started for up to {actual_batch} articles (offset {offset})"
            ),
            total_articles: actual_batch,
        })
    }

    async fn run_backfill(self, batch_size: u32, offset: u32) {
        let articles = match self.load_backfill_articles(batch_size, offset).await {
            Ok(articles) => articles,
            Err(error) => {
                error!("knowledge graph backfill failed to load articles: {error}");
                let _ = self.finish_backfill(Some(error.to_string()));
                return;
            }
        };

        if let Err(error) = self.update_backfill(|state| {
            state.total = articles.len() as i64;
            Ok(())
        }) {
            error!("knowledge graph backfill state update failed: {error}");
            return;
        }

        let mut engram = match self.load_entity_engram().await {
            Ok(engram) => engram,
            Err(error) => {
                error!("knowledge graph engram load failed: {error}");
                let _ = self.finish_backfill(Some(error.to_string()));
                return;
            }
        };
        let context = self.research_context().await;

        let total_articles = articles.len() as i64;
        for (article_index, article) in articles.into_iter().enumerate() {
            let current_article_index = article_index as i64 + 1;
            if let Err(error) = self.update_backfill(|state| {
                state.current_article_uid = Some(article.uid.clone());
                state.current_article_title = article.title.clone();
                state.current_article_index = Some(current_article_index);
                Ok(())
            }) {
                error!("knowledge graph backfill state update failed: {error}");
                return;
            }
            info!(
                article_uid = %article.uid,
                article_index = current_article_index,
                total = total_articles,
                "knowledge graph backfill article started"
            );

            let text = article.kg_text();
            let result = if text.trim().is_empty() {
                Err(AppError::BadRequest("No text content".to_string()))
            } else {
                self.insert_article_text(&article.uid, &text, &mut engram, &context)
                    .await
            };

            if let Err(update_error) = self.update_backfill(|state| {
                state.processed += 1;
                state.current_article_uid = None;
                state.current_article_title = None;
                state.current_article_index = None;
                match result {
                    Ok(_) => {
                        state.inserted += 1;
                        info!(
                            article_uid = %article.uid,
                            article_index = current_article_index,
                            total = total_articles,
                            "knowledge graph backfill article finished"
                        );
                    }
                    Err(error) => {
                        warn!(
                            "knowledge graph backfill failed for {}: {}",
                            article.uid, error
                        );
                        state.failed += 1;
                        if state.error.is_none() {
                            state.error = Some(error.to_string());
                        }
                    }
                }
                Ok(())
            }) {
                error!("knowledge graph backfill state update failed: {update_error}");
                return;
            }
        }

        if let Err(error) = self.finish_backfill(None) {
            error!("knowledge graph backfill finish failed: {error}");
        }
    }

    fn update_backfill(
        &self,
        updater: impl FnOnce(&mut KGBackfillStatusResponse) -> Result<(), AppError>,
    ) -> Result<(), AppError> {
        let mut state = self.backfill_state.lock().map_err(|_| {
            AppError::Internal("knowledge graph backfill state is poisoned".to_string())
        })?;
        updater(&mut state)
    }

    fn finish_backfill(&self, error_message: Option<String>) -> Result<(), AppError> {
        self.update_backfill(|state| {
            state.running = false;
            state.current_article_uid = None;
            state.current_article_title = None;
            state.current_article_index = None;
            if let Some(error_message) = error_message {
                state.error = Some(error_message);
            }
            Ok(())
        })
    }

    pub fn get_full_backfill_status(&self) -> Result<KGFullBackfillStatus, AppError> {
        let state = self.full_backfill_state.lock().map_err(|_| {
            AppError::Internal("knowledge graph full backfill state is poisoned".to_string())
        })?;
        Ok(state.clone())
    }

    pub async fn start_full_backfill(
        &self,
        kg_batch_size: u32,
        wiki_batch_size: u32,
    ) -> Result<KGFullBackfillStartResponse, AppError> {
        if self.get_backfill_status()?.running {
            return Err(AppError::Conflict(
                "KG backfill already in progress".to_string(),
            ));
        }
        if self.get_synthesis_compile_status()?.running {
            return Err(AppError::Conflict(
                "Wiki compilation already in progress".to_string(),
            ));
        }

        let kg_batch_size = kg_batch_size.max(1);
        let wiki_batch_size = wiki_batch_size.max(1);
        self.update_full_backfill(|state| {
            if state.running {
                return Err(AppError::Conflict(
                    "Full backfill already in progress".to_string(),
                ));
            }
            *state = KGFullBackfillStatus {
                running: true,
                stop_requested: false,
                phase: "kg".to_string(),
                message: Some(format!(
                    "Starting KG backfill with batches of {kg_batch_size}"
                )),
                ..KGFullBackfillStatus::default()
            };
            Ok(())
        })?;

        let service = self.clone();
        tokio::spawn(async move {
            service
                .run_full_backfill(kg_batch_size, wiki_batch_size)
                .await;
        });

        Ok(KGFullBackfillStartResponse {
            status: "started".to_string(),
            message: "Full KG and wiki backfill started".to_string(),
        })
    }

    pub fn request_full_backfill_stop(&self) -> Result<KGFullBackfillStatus, AppError> {
        self.update_full_backfill(|state| {
            if !state.running {
                return Err(AppError::Conflict(
                    "Full backfill is not running".to_string(),
                ));
            }
            state.stop_requested = true;
            state.message = Some("Stop requested. Current batch will finish first.".to_string());
            Ok(())
        })?;
        self.get_full_backfill_status()
    }

    async fn run_full_backfill(self, kg_batch_size: u32, wiki_batch_size: u32) {
        let result = self
            .run_full_backfill_inner(kg_batch_size, wiki_batch_size)
            .await;
        if let Err(error) = &result {
            error!("full KG/wiki backfill failed: {error}");
        }

        let _ = self.update_full_backfill(|state| {
            state.running = false;
            if let Err(error) = result {
                state.phase = "failed".to_string();
                state.error = Some(error.to_string());
                state.message = Some("Full KG and wiki backfill failed".to_string());
            } else if state.stop_requested {
                state.phase = "stopped".to_string();
                state.message = Some("Full KG and wiki backfill stopped".to_string());
            } else {
                state.phase = "done".to_string();
                state.message = Some("Full KG and wiki backfill complete".to_string());
            }
            Ok(())
        });
    }

    async fn run_full_backfill_inner(
        &self,
        kg_batch_size: u32,
        wiki_batch_size: u32,
    ) -> Result<(), AppError> {
        loop {
            if self.full_backfill_stop_requested()? {
                return Ok(());
            }

            let response = self.start_backfill(kg_batch_size, 0).await?;
            if response.total_articles <= 0 {
                let _ = self.wait_for_backfill_batch().await?;
                self.update_full_backfill(|state| {
                    state.message =
                        Some("KG backfill complete. Starting wiki compile.".to_string());
                    Ok(())
                })?;
                break;
            }

            self.update_full_backfill(|state| {
                state.phase = "kg".to_string();
                state.kg_batches += 1;
                state.message = Some(response.message);
                Ok(())
            })?;

            let status = self.wait_for_backfill_batch().await?;
            self.update_full_backfill(|state| {
                state.kg_processed += status.processed;
                state.kg_inserted += status.inserted;
                state.kg_failed += status.failed;
                Ok(())
            })?;
            if status.error.is_some() && status.inserted == 0 && status.total > 0 {
                return Err(AppError::Internal(status.error.unwrap_or_else(|| {
                    "KG backfill batch failed without inserted articles".to_string()
                })));
            }
        }

        loop {
            if self.full_backfill_stop_requested()? {
                return Ok(());
            }

            let response = self
                .start_synthesis_compilation(wiki_batch_size, false, None)
                .await?;
            if response.total_entities <= 0 {
                let _ = self.wait_for_synthesis_batch().await?;
                self.update_full_backfill(|state| {
                    state.message = Some("Wiki compile complete.".to_string());
                    Ok(())
                })?;
                break;
            }

            self.update_full_backfill(|state| {
                state.phase = "wiki".to_string();
                state.wiki_batches += 1;
                state.message = Some(response.message);
                Ok(())
            })?;

            let status = self.wait_for_synthesis_batch().await?;
            self.update_full_backfill(|state| {
                state.wiki_processed += status.processed;
                state.wiki_compiled += status.compiled;
                state.wiki_failed += status.failed;
                Ok(())
            })?;
            if status.error.is_some() && status.compiled == 0 && status.total > 0 {
                return Err(AppError::Internal(status.error.unwrap_or_else(|| {
                    "Wiki compile batch failed without compiled entities".to_string()
                })));
            }
        }

        Ok(())
    }

    async fn wait_for_backfill_batch(&self) -> Result<KGBackfillStatusResponse, AppError> {
        loop {
            tokio::time::sleep(Duration::from_secs(2)).await;
            let status = self.get_backfill_status()?;
            if !status.running {
                return Ok(status);
            }
        }
    }

    async fn wait_for_synthesis_batch(&self) -> Result<KGSynthesisCompileStatus, AppError> {
        loop {
            tokio::time::sleep(Duration::from_secs(2)).await;
            let status = self.get_synthesis_compile_status()?;
            if !status.running {
                return Ok(status);
            }
        }
    }

    fn update_full_backfill(
        &self,
        updater: impl FnOnce(&mut KGFullBackfillStatus) -> Result<(), AppError>,
    ) -> Result<(), AppError> {
        let mut state = self.full_backfill_state.lock().map_err(|_| {
            AppError::Internal("knowledge graph full backfill state is poisoned".to_string())
        })?;
        updater(&mut state)
    }

    fn full_backfill_stop_requested(&self) -> Result<bool, AppError> {
        let state = self.full_backfill_state.lock().map_err(|_| {
            AppError::Internal("knowledge graph full backfill state is poisoned".to_string())
        })?;
        Ok(state.stop_requested)
    }

    async fn insert_article_text(
        &self,
        uid: &str,
        text: &str,
        engram: &mut EntityEngram,
        context: &WorkspaceResearchContext,
    ) -> Result<InsertOutcome, AppError> {
        let chunks = chunk_text_for_kg(text);
        let chunks_total = chunks.len() as i64;
        // Chunk writes commit individually (mention counts, relationship
        // weights), so a retried article must not re-run chunks that already
        // landed — that would double-increment the counters. The progress row
        // records exactly which chunks committed.
        let mut completed = self.load_extraction_progress(uid, chunks_total).await?;
        let mut total_entities = 0i64;
        let mut total_relationships = 0i64;
        let mut failed_chunks = 0i64;
        let mut session_cache = HashMap::new();

        for (chunk_index, chunk_text) in chunks.iter().enumerate() {
            let chunk_index = chunk_index as i64;
            if completed.contains(&chunk_index) {
                continue;
            }

            // One retry guards against transient LLM/parse failures; after
            // that the chunk is skipped so one bad chunk can't sink the
            // article's remaining chunks.
            let extraction = match self.extract_chunk(uid, chunk_text, context).await {
                Ok(extraction) => extraction,
                Err(first_error) => {
                    warn!("chunk {chunk_index} extraction failed for {uid}: {first_error}; retrying once");
                    match self.extract_chunk(uid, chunk_text, context).await {
                        Ok(extraction) => extraction,
                        Err(error) => {
                            warn!("chunk {chunk_index} extraction failed again for {uid}: {error}; skipping chunk");
                            failed_chunks += 1;
                            continue;
                        }
                    }
                }
            };

            if !extraction.entities.is_empty() {
                let entity_ids = self
                    .resolve_entities(uid, &extraction.entities, engram, &mut session_cache)
                    .await?;

                for entity in &extraction.entities {
                    if let Some(entity_id) = entity_ids.get(&entity.name) {
                        self.add_article_entity(
                            uid,
                            *entity_id,
                            Some(entity.name.as_str()),
                            Some(entity.description.as_str()),
                            chunk_index,
                        )
                        .await?;
                        total_entities += 1;
                    }
                }

                for relationship in &extraction.relationships {
                    let Some(source_id) = entity_ids.get(&relationship.source).copied() else {
                        continue;
                    };
                    let Some(target_id) = entity_ids.get(&relationship.target).copied() else {
                        continue;
                    };
                    if source_id == target_id {
                        continue;
                    }
                    self.upsert_relationship(
                        source_id,
                        target_id,
                        &relationship.relationship,
                        uid,
                        Some(&relationship.description),
                    )
                    .await?;
                    total_relationships += 1;
                }
            }

            completed.insert(chunk_index);
            self.save_extraction_progress(uid, chunks_total, &completed)
                .await?;
        }

        // Only a fully extracted article counts as done; partial articles stay
        // unmarked (and keep their progress row) so backfill retries the
        // missing chunks without redoing the committed ones.
        if failed_chunks == 0 {
            self.mark_article_has_kg_entities(uid).await?;
            self.clear_extraction_progress(uid).await?;
        }

        // Mark syntheses stale for all entities touched by this article.
        let touched_ids: Vec<i64> = session_cache.values().copied().collect();
        self.mark_syntheses_stale(&touched_ids).await?;

        Ok(InsertOutcome {
            entities: total_entities,
            relationships: total_relationships,
            chunks: chunks_total,
            failed_chunks,
        })
    }

    /// Loads the set of chunk indexes already committed for this article. A
    /// missing row or a chunk-count mismatch (the source text changed) starts
    /// fresh.
    async fn load_extraction_progress(
        &self,
        uid: &str,
        chunks_total: i64,
    ) -> Result<BTreeSet<i64>, AppError> {
        let database_path = self.database_path.clone();
        let uid = uid.to_string();
        run_blocking(move || {
            let conn = crate::db::open_connection(&*database_path)?;
            let row: Option<(i64, String)> = conn
                .query_row(
                    "SELECT chunks_total, completed_chunks_json
                     FROM kg_extraction_progress WHERE article_uid = ?1",
                    [uid.as_str()],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;

            match row {
                Some((stored_total, completed_json)) if stored_total == chunks_total => {
                    Ok(serde_json::from_str::<Vec<i64>>(&completed_json)
                        .unwrap_or_default()
                        .into_iter()
                        .collect())
                }
                Some(_) => {
                    conn.execute(
                        "DELETE FROM kg_extraction_progress WHERE article_uid = ?1",
                        [uid.as_str()],
                    )?;
                    Ok(BTreeSet::new())
                }
                None => Ok(BTreeSet::new()),
            }
        })
        .await
    }

    async fn save_extraction_progress(
        &self,
        uid: &str,
        chunks_total: i64,
        completed: &BTreeSet<i64>,
    ) -> Result<(), AppError> {
        let database_path = self.database_path.clone();
        let uid = uid.to_string();
        let completed_json = serde_json::to_string(&completed.iter().collect::<Vec<_>>())
            .unwrap_or_else(|_| "[]".to_string());
        run_blocking(move || {
            let conn = crate::db::open_connection(&*database_path)?;
            conn.execute(
                "INSERT INTO kg_extraction_progress (article_uid, chunks_total, completed_chunks_json, updated_at)
                 VALUES (?1, ?2, ?3, datetime('now'))
                 ON CONFLICT(article_uid) DO UPDATE SET
                   chunks_total = excluded.chunks_total,
                   completed_chunks_json = excluded.completed_chunks_json,
                   updated_at = excluded.updated_at",
                params![uid, chunks_total, completed_json],
            )?;
            Ok(())
        })
        .await
    }

    async fn clear_extraction_progress(&self, uid: &str) -> Result<(), AppError> {
        let database_path = self.database_path.clone();
        let uid = uid.to_string();
        run_blocking(move || {
            let conn = crate::db::open_connection(&*database_path)?;
            conn.execute(
                "DELETE FROM kg_extraction_progress WHERE article_uid = ?1",
                [uid.as_str()],
            )?;
            Ok(())
        })
        .await
    }

    async fn extract_chunk(
        &self,
        uid: &str,
        chunk_text: &str,
        context: &WorkspaceResearchContext,
    ) -> Result<ChunkExtraction, AppError> {
        let mut variables = std::collections::BTreeMap::new();
        variables.insert("chunk_text".to_string(), chunk_text.to_string());
        insert_workspace_prompt_vars(&mut variables, context);
        let response = self
            .llm_service
            .execute_prompt(
                "entity_extraction",
                variables,
                Some(uid),
                LlmOutputMode::Json,
            )
            .await?;
        let json_output = response.json_output.ok_or_else(|| {
            AppError::Internal("entity_extraction did not return JSON output".to_string())
        })?;
        let mut extraction: ChunkExtraction =
            serde_json::from_value(json_output).map_err(|error| {
                AppError::Internal(format!("invalid entity_extraction output: {error}"))
            })?;
        validate_extraction(&mut extraction);
        Ok(extraction)
    }

    async fn resolve_entities(
        &self,
        uid: &str,
        entities: &[ExtractedEntity],
        engram: &mut EntityEngram,
        session_cache: &mut HashMap<String, i64>,
    ) -> Result<HashMap<String, i64>, AppError> {
        let mut entity_ids = HashMap::new();
        let mut unresolved = Vec::new();

        for entity in entities {
            let cache_key = normalize_name(&entity.name);

            let resolved_id = session_cache
                .get(&cache_key)
                .copied()
                .or_else(|| engram.find_by_name(&entity.name))
                .or_else(|| engram.find_by_alias(&entity.name));

            if let Some(entity_id) = resolved_id {
                self.register_resolved(
                    entity_id,
                    &entity.name,
                    cache_key,
                    engram,
                    session_cache,
                    &mut entity_ids,
                )
                .await?;
                continue;
            }

            unresolved.push(entity.clone());
        }

        if unresolved.is_empty() {
            return Ok(entity_ids);
        }

        let names = unresolved
            .iter()
            .map(|entity| entity.name.clone())
            .collect::<Vec<_>>();
        let embeddings = self.embedding_service.embed_texts(&names).await?;

        for (entity, embedding) in unresolved.into_iter().zip(embeddings.into_iter()) {
            let cache_key = normalize_name(&entity.name);
            if let Some(entity_id) = session_cache.get(&cache_key).copied() {
                self.register_resolved(
                    entity_id,
                    &entity.name,
                    cache_key,
                    engram,
                    session_cache,
                    &mut entity_ids,
                )
                .await?;
                continue;
            }

            if let Some(candidate) = engram
                .find_similar(
                    self,
                    &embedding,
                    HIGH_CONFIDENCE_THRESHOLD,
                    &entity.entity_type,
                )
                .await?
            {
                self.register_resolved(
                    candidate.entity.id,
                    &entity.name,
                    cache_key,
                    engram,
                    session_cache,
                    &mut entity_ids,
                )
                .await?;
                continue;
            }

            let candidates = engram
                .find_candidates(self, &embedding, &entity.entity_type, 5)
                .await?;
            if let Some(candidate_id) = self
                .resolve_ambiguous_entity(uid, &entity, &candidates)
                .await?
            {
                self.register_resolved(
                    candidate_id,
                    &entity.name,
                    cache_key,
                    engram,
                    session_cache,
                    &mut entity_ids,
                )
                .await?;
                continue;
            }

            let created = self.create_entity(&entity, &embedding).await?;
            let created_id = created.id;
            engram.add_entity_with_embedding(created, embedding);
            session_cache.insert(cache_key, created_id);
            entity_ids.insert(entity.name.clone(), created_id);
        }

        Ok(entity_ids)
    }

    async fn register_resolved(
        &self,
        entity_id: i64,
        name: &str,
        cache_key: String,
        engram: &mut EntityEngram,
        session_cache: &mut HashMap<String, i64>,
        entity_ids: &mut HashMap<String, i64>,
    ) -> Result<(), AppError> {
        self.add_alias_and_increment(entity_id, name).await?;
        engram.register_alias(entity_id, name);
        session_cache.insert(cache_key, entity_id);
        entity_ids.insert(name.to_string(), entity_id);
        Ok(())
    }

    async fn resolve_ambiguous_entity(
        &self,
        uid: &str,
        entity: &ExtractedEntity,
        candidates: &[ResolutionCandidate],
    ) -> Result<Option<i64>, AppError> {
        if candidates.is_empty() {
            return Ok(None);
        }

        let candidate_ids = candidates
            .iter()
            .map(|candidate| candidate.entity.id)
            .collect::<Vec<_>>();
        let cache_entries = self
            .get_resolution_cache(
                &normalize_name(&entity.name),
                &normalize_entity_type(&entity.entity_type),
                &candidate_ids,
            )
            .await?;

        for candidate in candidates {
            if let Some((true, matched_entity_id)) = cache_entries.get(&candidate.entity.id) {
                return Ok(Some(matched_entity_id.unwrap_or(candidate.entity.id)));
            }
        }

        let all_cached = candidates
            .iter()
            .all(|candidate| cache_entries.contains_key(&candidate.entity.id));
        if all_cached {
            return Ok(None);
        }

        for candidate in candidates {
            if cache_entries.contains_key(&candidate.entity.id) {
                continue;
            }

            // A failed verification call must not abort the whole article.
            // Skip the candidate without caching: no merge on uncertainty (a
            // duplicate entity is recoverable, a wrong merge is not), and the
            // next encounter retries the verification.
            let verification = match self
                .verify_entity_candidate(uid, entity, &candidate.entity)
                .await
            {
                Ok(verification) => verification,
                Err(error) => {
                    warn!(
                        "entity verification failed for '{}' vs '{}': {error}; treating as no match",
                        entity.name, candidate.entity.canonical_name
                    );
                    continue;
                }
            };
            let matched = verification.same_entity
                && verification.confidence >= ENTITY_VERIFICATION_CONFIDENCE_THRESHOLD;
            if !verification.reasoning.trim().is_empty() {
                warn!(
                    "entity verification {} -> {}: {}",
                    entity.name, candidate.entity.canonical_name, verification.reasoning
                );
            }
            self.cache_resolution(
                &normalize_name(&entity.name),
                &normalize_entity_type(&entity.entity_type),
                candidate.entity.id,
                matched,
                matched.then_some(candidate.entity.id),
                Some(verification.confidence),
            )
            .await?;
            if matched {
                return Ok(Some(candidate.entity.id));
            }
        }

        Ok(None)
    }

    async fn verify_entity_candidate(
        &self,
        uid: &str,
        new_entity: &ExtractedEntity,
        existing: &StoredEntity,
    ) -> Result<EntityVerificationResult, AppError> {
        let mut variables = std::collections::BTreeMap::new();
        variables.insert("existing_name".to_string(), existing.canonical_name.clone());
        variables.insert(
            "existing_type".to_string(),
            normalize_entity_type(&existing.entity_type),
        );
        variables.insert(
            "existing_description".to_string(),
            existing.description.clone().unwrap_or_default(),
        );
        variables.insert("new_name".to_string(), new_entity.name.clone());
        variables.insert(
            "new_type".to_string(),
            normalize_entity_type(&new_entity.entity_type),
        );
        variables.insert(
            "new_description".to_string(),
            new_entity.description.clone(),
        );

        let response = self
            .llm_service
            .execute_prompt(
                "entity_verification",
                variables,
                Some(uid),
                LlmOutputMode::Json,
            )
            .await?;
        let json_output = response.json_output.ok_or_else(|| {
            AppError::Internal("entity_verification did not return JSON output".to_string())
        })?;

        serde_json::from_value(json_output).map_err(|error| {
            AppError::Internal(format!("invalid entity_verification output: {error}"))
        })
    }

    async fn load_entity_engram(&self) -> Result<EntityEngram, AppError> {
        let database_path = self.database_path.clone();
        let entities = run_blocking(move || {
            let conn = crate::db::open_connection(&*database_path)?;
            let mut stmt = conn.prepare(
                "
                SELECT id, canonical_name, entity_type, description, mention_count, aliases_json
                FROM kg_entities
                ORDER BY id ASC
                ",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok(StoredEntity {
                    id: row.get(0)?,
                    canonical_name: row.get(1)?,
                    entity_type: row
                        .get::<_, Option<String>>(2)?
                        .unwrap_or_else(|| "UNKNOWN".to_string()),
                    description: row.get(3)?,
                    mention_count: row.get::<_, Option<i64>>(4)?.unwrap_or(0),
                    aliases: parse_string_list(row.get::<_, Option<String>>(5)?),
                })
            })?;

            rows.collect::<Result<Vec<_>, _>>()
                .map_err(anyhow::Error::from)
        })
        .await?;

        Ok(EntityEngram::from_entities(entities))
    }

    async fn load_embeddings_for_type(
        &self,
        entity_type: &str,
    ) -> Result<Vec<CachedEmbedding>, AppError> {
        let database_path = self.database_path.clone();
        let entity_type = entity_type.to_string();

        run_blocking(move || {
            let conn = crate::db::open_connection(&*database_path)?;
            let mut stmt = conn.prepare(
                "
                SELECT e.id, v.embedding
                FROM kg_entities e
                JOIN vec_kg_entities v ON v.entity_id = e.id
                WHERE UPPER(e.entity_type) = UPPER(?1)
                ",
            )?;
            let rows = stmt.query_map([entity_type.as_str()], |row| {
                let entity_id: i64 = row.get(0)?;
                let vector = parse_embedding_row(row.get_ref(1)?)?;
                Ok(CachedEmbedding {
                    entity_id,
                    norm: vector_norm(&vector),
                    vector,
                })
            })?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(anyhow::Error::from)
        })
        .await
    }

    async fn load_articles_by_uids(&self, uids: &[String]) -> Result<Vec<ArticleInput>, AppError> {
        let database_path = self.database_path.clone();
        let uids = uids.to_vec();

        run_blocking(move || {
            let conn = crate::db::open_connection(&*database_path)?;
            if uids.is_empty() {
                return Ok(Vec::new());
            }
            let placeholders = vec!["?"; uids.len()].join(", ");
            let sql = format!(
                "SELECT uid, title, full_text, content_type, byline_summary FROM haie_rev WHERE uid IN ({placeholders})"
            );
            let params: Vec<Value> = uids.into_iter().map(Value::Text).collect();
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map(params_from_iter(params.iter()), |row| {
                Ok(ArticleInput {
                    uid: row.get(0)?,
                    title: row.get(1)?,
                    full_text: row.get(2)?,
                    content_type: row.get(3)?,
                    byline_summary: row.get(4)?,
                })
            })?;
            rows.collect::<Result<Vec<_>, _>>().map_err(anyhow::Error::from)
        })
        .await
    }

    async fn count_backfill_candidates(&self) -> Result<i64, AppError> {
        let database_path = self.database_path.clone();
        run_blocking(move || {
            let conn = crate::db::open_connection(&*database_path)?;
            let count = conn.query_row(
                "
                SELECT COUNT(*)
                FROM haie_rev
                WHERE full_text IS NOT NULL
                  AND COALESCE(has_kg_entities, 0) = 0
                ",
                [],
                |row| row.get::<_, i64>(0),
            )?;
            Ok(count)
        })
        .await
    }

    async fn load_backfill_articles(
        &self,
        batch_size: u32,
        offset: u32,
    ) -> Result<Vec<ArticleInput>, AppError> {
        let database_path = self.database_path.clone();
        run_blocking(move || {
            let conn = crate::db::open_connection(&*database_path)?;
            let mut stmt = conn.prepare(
                "
                SELECT uid, title, full_text, content_type, byline_summary
                FROM haie_rev
                WHERE full_text IS NOT NULL
                  AND COALESCE(has_kg_entities, 0) = 0
                ORDER BY COALESCE(reg_date, '') DESC, uid ASC
                LIMIT ?1 OFFSET ?2
                ",
            )?;
            let rows = stmt.query_map(
                [
                    i64::from(batch_size.max(1)).to_string(),
                    i64::from(offset).to_string(),
                ],
                |row| {
                    Ok(ArticleInput {
                        uid: row.get(0)?,
                        title: row.get(1)?,
                        full_text: row.get(2)?,
                        content_type: row.get(3)?,
                        byline_summary: row.get(4)?,
                    })
                },
            )?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(anyhow::Error::from)
        })
        .await
    }

    async fn create_entity(
        &self,
        entity: &ExtractedEntity,
        embedding: &[f32],
    ) -> Result<StoredEntity, AppError> {
        let database_path = self.database_path.clone();
        let entity = entity.clone();
        let embedding_bytes = embedding.as_bytes().to_vec();

        run_blocking(move || {
            let conn = crate::db::open_connection(&*database_path)?;
            conn.execute(
                "
                INSERT INTO kg_entities (
                    canonical_name, entity_type, description, aliases_json, mention_count, updated_at
                ) VALUES (?1, ?2, ?3, '[]', 1, datetime('now'))
                ",
                params![
                    entity.name.trim(),
                    normalize_entity_type(&entity.entity_type),
                    entity.description.trim(),
                ],
            )?;
            let entity_id = conn.last_insert_rowid();
            conn.execute(
                "
                INSERT INTO vec_kg_entities (entity_id, embedding)
                VALUES (?1, ?2)
                ",
                params![entity_id, embedding_bytes],
            )?;

            Ok(StoredEntity {
                id: entity_id,
                canonical_name: entity.name.trim().to_string(),
                entity_type: normalize_entity_type(&entity.entity_type),
                description: Some(entity.description.trim().to_string()),
                mention_count: 1,
                aliases: Vec::new(),
            })
        })
        .await
    }

    async fn add_alias_and_increment(&self, entity_id: i64, alias: &str) -> Result<(), AppError> {
        let database_path = self.database_path.clone();
        let alias = alias.trim().to_string();

        run_blocking(move || {
            let conn = crate::db::open_connection(&*database_path)?;
            let row = conn
                .query_row(
                    "
                    SELECT aliases_json
                    FROM kg_entities
                    WHERE id = ?1
                    ",
                    [entity_id],
                    |row| row.get::<_, Option<String>>(0),
                )
                .optional()?;

            let Some(raw_aliases) = row else {
                return Ok(());
            };

            let mut aliases = parse_string_list(raw_aliases);
            if !alias.is_empty()
                && !aliases
                    .iter()
                    .any(|existing| normalize_name(existing) == normalize_name(&alias))
            {
                aliases.push(alias);
            }

            conn.execute(
                "
                UPDATE kg_entities
                SET aliases_json = ?2,
                    mention_count = COALESCE(mention_count, 0) + 1,
                    updated_at = datetime('now')
                WHERE id = ?1
                ",
                params![entity_id, serde_json::to_string(&aliases)?,],
            )?;

            Ok(())
        })
        .await
    }

    async fn add_article_entity(
        &self,
        article_uid: &str,
        entity_id: i64,
        mention_text: Option<&str>,
        context: Option<&str>,
        chunk_index: i64,
    ) -> Result<(), AppError> {
        let database_path = self.database_path.clone();
        let article_uid = article_uid.to_string();
        let mention_text = mention_text.map(str::to_string);
        let context = context.map(str::to_string);

        run_blocking(move || {
            let conn = crate::db::open_connection(&*database_path)?;
            conn.execute(
                "
                INSERT OR IGNORE INTO kg_article_entities (
                    article_uid, entity_id, mention_text, context, chunk_index
                ) VALUES (?1, ?2, ?3, ?4, ?5)
                ",
                params![article_uid, entity_id, mention_text, context, chunk_index,],
            )?;
            Ok(())
        })
        .await
    }

    async fn upsert_relationship(
        &self,
        source_id: i64,
        target_id: i64,
        relationship_type: &str,
        article_uid: &str,
        description: Option<&str>,
    ) -> Result<(), AppError> {
        let database_path = self.database_path.clone();
        let (relationship_type, source_id, target_id) =
            canonicalize_relationship(relationship_type, source_id, target_id);
        let article_uid = article_uid.to_string();
        let description = description.map(|value| value.trim().to_string());

        run_blocking(move || {
            let conn = crate::db::open_connection(&*database_path)?;
            let existing = conn
                .query_row(
                    "
                    SELECT id, weight, source_articles_json, description
                    FROM kg_relationships
                    WHERE source_entity_id = ?1
                      AND target_entity_id = ?2
                      AND relationship_type = ?3
                    ",
                    params![source_id, target_id, relationship_type],
                    |row| {
                        Ok((
                            row.get::<_, i64>(0)?,
                            row.get::<_, Option<f64>>(1)?.unwrap_or(1.0),
                            row.get::<_, Option<String>>(2)?,
                            row.get::<_, Option<String>>(3)?,
                        ))
                    },
                )
                .optional()?;

            if let Some((relationship_id, weight, raw_articles, existing_description)) = existing {
                let mut articles = parse_string_list(raw_articles);
                if !articles.iter().any(|value| value == &article_uid) {
                    articles.push(article_uid);
                }
                let next_description = if existing_description
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .is_some()
                {
                    existing_description
                } else {
                    description
                };

                conn.execute(
                    "
                    UPDATE kg_relationships
                    SET weight = ?2,
                        source_articles_json = ?3,
                        description = ?4,
                        updated_at = datetime('now')
                    WHERE id = ?1
                    ",
                    params![
                        relationship_id,
                        weight + 1.0,
                        serde_json::to_string(&articles)?,
                        next_description,
                    ],
                )?;
            } else {
                conn.execute(
                    "
                    INSERT INTO kg_relationships (
                        source_entity_id, target_entity_id, relationship_type,
                        description, weight, source_articles_json, updated_at
                    ) VALUES (?1, ?2, ?3, ?4, 1.0, ?5, datetime('now'))
                    ",
                    params![
                        source_id,
                        target_id,
                        relationship_type,
                        description,
                        serde_json::to_string(&vec![article_uid])?,
                    ],
                )?;
            }

            Ok(())
        })
        .await
    }

    async fn mark_article_has_kg_entities(&self, uid: &str) -> Result<(), AppError> {
        let database_path = self.database_path.clone();
        let uid = uid.to_string();
        run_blocking(move || {
            let conn = crate::db::open_connection(&*database_path)?;
            conn.execute(
                "
                UPDATE haie_rev
                SET has_kg_entities = 1,
                    updated_at = datetime('now')
                WHERE uid = ?1
                ",
                [uid.as_str()],
            )?;
            Ok(())
        })
        .await
    }

    async fn mark_syntheses_stale(&self, entity_ids: &[i64]) -> Result<(), AppError> {
        if entity_ids.is_empty() {
            return Ok(());
        }
        let database_path = self.database_path.clone();
        let entity_ids = entity_ids.to_vec();
        run_blocking(move || {
            let conn = crate::db::open_connection(&*database_path)?;
            let placeholders = vec!["?"; entity_ids.len()].join(", ");

            // Mark entity syntheses stale.
            let sql = format!(
                "UPDATE kg_entity_syntheses SET stale = 1, updated_at = datetime('now')
                 WHERE entity_id IN ({placeholders}) AND stale = 0"
            );
            let params: Vec<Value> = entity_ids.iter().copied().map(Value::Integer).collect();
            conn.execute(&sql, params_from_iter(params.iter()))?;

            // Clear relationship evidence for affected entities.
            let sql = format!(
                "UPDATE kg_relationships SET evidence_summary = NULL
                 WHERE (source_entity_id IN ({placeholders}) OR target_entity_id IN ({placeholders}))
                   AND evidence_summary IS NOT NULL"
            );
            let mut params2: Vec<Value> = entity_ids.iter().copied().map(Value::Integer).collect();
            params2.extend(entity_ids.iter().copied().map(Value::Integer));
            conn.execute(&sql, params_from_iter(params2.iter()))?;

            Ok(())
        })
        .await
    }

    async fn get_resolution_cache(
        &self,
        query_name: &str,
        query_type: &str,
        candidate_ids: &[i64],
    ) -> Result<HashMap<i64, (bool, Option<i64>)>, AppError> {
        if candidate_ids.is_empty() {
            return Ok(HashMap::new());
        }

        let database_path = self.database_path.clone();
        let query_name = query_name.to_string();
        let query_type = query_type.to_string();
        let candidate_ids = candidate_ids.to_vec();

        run_blocking(move || {
            let conn = crate::db::open_connection(&*database_path)?;
            let placeholders = vec!["?"; candidate_ids.len()].join(", ");
            let sql = format!(
                "
                SELECT candidate_id, is_match, matched_entity_id
                FROM kg_resolution_cache
                WHERE query_name = ?1
                  AND query_type = ?2
                  AND candidate_id IN ({placeholders})
                "
            );
            let mut params = vec![Value::Text(query_name), Value::Text(query_type)];
            params.extend(candidate_ids.iter().copied().map(Value::Integer));

            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map(params_from_iter(params.iter()), |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)? != 0,
                    row.get::<_, Option<i64>>(2)?,
                ))
            })?;

            let mut cache = HashMap::new();
            for row in rows {
                let (candidate_id, is_match, matched_entity_id) = row?;
                cache.insert(candidate_id, (is_match, matched_entity_id));
            }
            Ok(cache)
        })
        .await
    }

    async fn cache_resolution(
        &self,
        query_name: &str,
        query_type: &str,
        candidate_id: i64,
        is_match: bool,
        matched_entity_id: Option<i64>,
        confidence: Option<f64>,
    ) -> Result<(), AppError> {
        let database_path = self.database_path.clone();
        let query_name = query_name.to_string();
        let query_type = query_type.to_string();

        run_blocking(move || {
            let conn = crate::db::open_connection(&*database_path)?;
            conn.execute(
                "
                INSERT INTO kg_resolution_cache (
                    query_name, query_type, candidate_id, is_match, matched_entity_id, confidence
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                ON CONFLICT(query_name, query_type, candidate_id)
                DO UPDATE SET
                    is_match = excluded.is_match,
                    matched_entity_id = excluded.matched_entity_id,
                    confidence = excluded.confidence
                ",
                params![
                    query_name,
                    query_type,
                    candidate_id,
                    if is_match { 1 } else { 0 },
                    matched_entity_id,
                    confidence,
                ],
            )?;
            Ok(())
        })
        .await
    }

    // --- Synthesis Read Endpoints ---

    pub async fn get_entity_synthesis(
        &self,
        entity_name: &str,
    ) -> Result<KGEntitySynthesis, AppError> {
        let database_path = self.database_path.clone();
        let entity_name = entity_name.trim().to_string();
        let entity_like = format!("%{}%", entity_name.to_lowercase());
        let entity_name_for_error = entity_name.clone();

        run_blocking(move || {
            let conn = crate::db::open_connection(&*database_path)?;
            let row = conn
                .query_row(
                    "
                    SELECT s.id, s.entity_id, e.canonical_name, e.entity_type,
                           s.summary, s.synthesis, s.key_aspects_json, s.related_entities_json,
                           s.source_article_count, s.compiled_at, s.stale, s.version
                    FROM kg_entity_syntheses s
                    JOIN kg_entities e ON e.id = s.entity_id
                    WHERE LOWER(e.canonical_name) = LOWER(?1)
                       OR LOWER(COALESCE(e.aliases_json, '')) LIKE ?2
                    ORDER BY e.mention_count DESC
                    LIMIT 1
                    ",
                    params![entity_name, entity_like],
                    |row| {
                        Ok((
                            row.get::<_, i64>(1)?,    // entity_id
                            row.get::<_, String>(2)?, // canonical_name
                            row.get::<_, Option<String>>(3)?
                                .unwrap_or_else(|| "UNKNOWN".to_string()), // entity_type
                            row.get::<_, Option<String>>(4)?.unwrap_or_default(), // summary
                            row.get::<_, Option<String>>(5)?.unwrap_or_default(), // synthesis
                            row.get::<_, Option<String>>(6)?, // key_aspects_json
                            row.get::<_, Option<String>>(7)?, // related_entities_json
                            row.get::<_, Option<i64>>(8)?.unwrap_or(0), // source_article_count
                            row.get::<_, Option<String>>(9)?, // compiled_at
                            row.get::<_, Option<i64>>(10)?.unwrap_or(0) != 0, // stale
                            row.get::<_, Option<i64>>(11)?.unwrap_or(1), // version
                        ))
                    },
                )
                .optional()?;

            let Some((
                entity_id,
                entity_name,
                entity_type,
                summary,
                synthesis,
                key_aspects_json,
                related_entities_json,
                source_article_count,
                compiled_at,
                stale,
                version,
            )) = row
            else {
                return Err(anyhow::anyhow!(
                    "No synthesis found for entity: {entity_name}"
                ));
            };

            let key_aspects = key_aspects_json
                .and_then(|json| serde_json::from_str::<Vec<String>>(&json).ok())
                .unwrap_or_default();
            let related_entities = related_entities_json
                .and_then(|json| serde_json::from_str::<Vec<KGSynthesisRelatedEntity>>(&json).ok())
                .unwrap_or_default();

            Ok(KGEntitySynthesis {
                entity_id,
                entity_name,
                entity_type,
                summary,
                synthesis,
                key_aspects,
                related_entities,
                source_article_count,
                compiled_at,
                stale,
                version,
            })
        })
        .await
        .map_err(|_| {
            AppError::NotFound(format!(
                "No synthesis found for entity: {entity_name_for_error}"
            ))
        })
    }

    pub async fn list_syntheses(
        &self,
        query: KGSynthesisListQuery,
        workspace_id: i64,
    ) -> Result<KGSynthesisListResponse, AppError> {
        let database_path = self.database_path.clone();

        run_blocking(move || {
            let conn = crate::db::open_connection(&*database_path)?;
            let stale_only_flag: i64 = if query.stale_only { 1 } else { 0 };
            let entity_type = query.entity_type.clone();
            let limit = i64::from(query.limit.clamp(1, 500));
            let offset = i64::from(query.offset);

            let sql = format!(
                "
                SELECT s.entity_id, e.canonical_name, e.entity_type, s.summary,
                       s.source_article_count, s.stale, s.compiled_at
                FROM kg_entity_syntheses s
                JOIN kg_entities e ON e.id = s.entity_id
                WHERE (?1 = 0 OR s.stale = 1)
                  AND (?2 IS NULL OR UPPER(e.entity_type) = UPPER(?2))
                  AND COALESCE(s.source_article_count, 0) >= ?3
                  AND s.entity_id IN (SELECT kae.entity_id FROM kg_article_entities kae
                       JOIN haie_rev h ON h.uid = kae.article_uid WHERE h.workspace_id = ?6)
                  AND {WIKI_ENTITY_FILTER_SQL}
                ORDER BY s.stale DESC, s.source_article_count DESC, e.mention_count DESC
                LIMIT ?4 OFFSET ?5
                "
            );
            let mut stmt = conn.prepare(&sql)?;
            let syntheses = stmt
                .query_map(
                    params![
                        stale_only_flag,
                        entity_type,
                        WIKI_MIN_SOURCE_ARTICLES,
                        limit,
                        offset,
                        workspace_id
                    ],
                    |row| {
                        Ok(KGEntitySynthesisSummary {
                            entity_id: row.get(0)?,
                            entity_name: row.get(1)?,
                            entity_type: row
                                .get::<_, Option<String>>(2)?
                                .unwrap_or_else(|| "UNKNOWN".to_string()),
                            summary: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
                            source_article_count: row.get::<_, Option<i64>>(4)?.unwrap_or(0),
                            stale: row.get::<_, Option<i64>>(5)?.unwrap_or(0) != 0,
                            compiled_at: row.get(6)?,
                        })
                    },
                )?
                .collect::<Result<Vec<_>, _>>()?;

            let total_sql = format!(
                "
                SELECT COUNT(*)
                FROM kg_entity_syntheses s
                JOIN kg_entities e ON e.id = s.entity_id
                WHERE (?1 = 0 OR s.stale = 1)
                  AND (?2 IS NULL OR UPPER(e.entity_type) = UPPER(?2))
                  AND COALESCE(s.source_article_count, 0) >= ?3
                  AND s.entity_id IN (SELECT kae.entity_id FROM kg_article_entities kae
                       JOIN haie_rev h ON h.uid = kae.article_uid WHERE h.workspace_id = ?4)
                  AND {WIKI_ENTITY_FILTER_SQL}
                "
            );
            let total: i64 = conn.query_row(
                &total_sql,
                params![
                    stale_only_flag,
                    entity_type,
                    WIKI_MIN_SOURCE_ARTICLES,
                    workspace_id
                ],
                |row| row.get(0),
            )?;

            let stale_sql = format!(
                "
                SELECT COUNT(*)
                FROM kg_entity_syntheses s
                JOIN kg_entities e ON e.id = s.entity_id
                WHERE s.stale = 1
                  AND COALESCE(s.source_article_count, 0) >= ?1
                  AND s.entity_id IN (SELECT kae.entity_id FROM kg_article_entities kae
                       JOIN haie_rev h ON h.uid = kae.article_uid WHERE h.workspace_id = ?2)
                  AND {WIKI_ENTITY_FILTER_SQL}
                "
            );
            let stale_count: i64 = conn.query_row(
                &stale_sql,
                params![WIKI_MIN_SOURCE_ARTICLES, workspace_id],
                |row| row.get(0),
            )?;

            Ok(KGSynthesisListResponse {
                syntheses,
                total,
                stale_count,
            })
        })
        .await
    }

    pub async fn search_syntheses(
        &self,
        query: &str,
        limit: u32,
    ) -> Result<Vec<KGEntitySynthesisSummary>, AppError> {
        let fts_query = build_fts_query(query);
        let database_path = self.database_path.clone();
        let limit = i64::from(limit.clamp(1, 100));

        run_blocking(move || {
            let conn = crate::db::open_connection(&*database_path)?;
            let sql = format!(
                "
                SELECT s.entity_id, e.canonical_name, e.entity_type, s.summary,
                       s.source_article_count, s.stale, s.compiled_at,
                       -bm25(fts_kg_syntheses) as score
                FROM fts_kg_syntheses f
                JOIN kg_entity_syntheses s ON f.rowid = s.id
                JOIN kg_entities e ON e.id = s.entity_id
                WHERE fts_kg_syntheses MATCH ?1
                  AND COALESCE(s.source_article_count, 0) >= ?2
                  AND {WIKI_ENTITY_FILTER_SQL}
                ORDER BY score DESC
                LIMIT ?3
                "
            );
            let mut stmt = conn.prepare(&sql)?;
            let results = stmt
                .query_map(params![fts_query, WIKI_MIN_SOURCE_ARTICLES, limit], |row| {
                    Ok(KGEntitySynthesisSummary {
                        entity_id: row.get(0)?,
                        entity_name: row.get(1)?,
                        entity_type: row
                            .get::<_, Option<String>>(2)?
                            .unwrap_or_else(|| "UNKNOWN".to_string()),
                        summary: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
                        source_article_count: row.get::<_, Option<i64>>(4)?.unwrap_or(0),
                        stale: row.get::<_, Option<i64>>(5)?.unwrap_or(0) != 0,
                        compiled_at: row.get(6)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(results)
        })
        .await
    }

    pub async fn analyze_gaps(&self, workspace_id: i64) -> Result<KGGapAnalysisResponse, AppError> {
        let database_path = self.database_path.clone();
        let context = self
            .workspace_service
            .research_context(workspace_id)
            .await
            .unwrap_or_default();

        let (
            entity_summaries,
            type_distribution,
            relationship_stats,
            isolated_entities,
            entities_reviewed,
        ) = run_blocking(move || {
            let conn = crate::db::open_connection(&*database_path)?;

            let mut stmt = conn.prepare(&format!(
                "
                    SELECT e.canonical_name, e.entity_type,
                           COALESCE(s.summary, e.description, '') as summary
                    FROM kg_entities e
                    LEFT JOIN kg_entity_syntheses s ON s.entity_id = e.id
                    WHERE e.id IN {WS_ENTITY_SCOPE}
                    ORDER BY e.mention_count DESC
                    LIMIT 30
                    "
            ))?;
            let rows = stmt.query_map([workspace_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?
                        .unwrap_or_else(|| "UNKNOWN".to_string()),
                    row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                ))
            })?;
            let mut entity_parts = Vec::new();
            let mut count = 0i64;
            for row in rows {
                let (name, entity_type, summary) = row?;
                entity_parts.push(format!("- {name} [{entity_type}]: {summary}"));
                count += 1;
            }
            let entity_summaries = if entity_parts.is_empty() {
                "(no entities)".to_string()
            } else {
                entity_parts.join("\n")
            };

            let mut stmt = conn.prepare(&format!(
                "SELECT entity_type, COUNT(*) FROM kg_entities
                 WHERE id IN {WS_ENTITY_SCOPE} GROUP BY entity_type"
            ))?;
            let rows = stmt.query_map([workspace_id], |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?
                        .unwrap_or_else(|| "UNKNOWN".to_string()),
                    row.get::<_, i64>(1)?,
                ))
            })?;
            let mut type_parts = Vec::new();
            for row in rows {
                let (entity_type, c) = row?;
                type_parts.push(format!("- {entity_type}: {c}"));
            }
            let type_distribution = if type_parts.is_empty() {
                "(no entities)".to_string()
            } else {
                type_parts.join("\n")
            };

            let mut stmt = conn.prepare(&format!(
                "SELECT relationship_type, COUNT(*)
                     FROM kg_relationships
                     WHERE source_entity_id IN {WS_ENTITY_SCOPE}
                       AND target_entity_id IN {WS_ENTITY_SCOPE}
                     GROUP BY relationship_type
                     ORDER BY COUNT(*) DESC
                     LIMIT 10"
            ))?;
            let rows = stmt.query_map([workspace_id, workspace_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })?;
            let mut rel_parts = Vec::new();
            for row in rows {
                let (rel_type, c) = row?;
                rel_parts.push(format!("- {rel_type}: {c}"));
            }
            let relationship_stats = if rel_parts.is_empty() {
                "(no relationships)".to_string()
            } else {
                rel_parts.join("\n")
            };

            let mut stmt = conn.prepare(&format!(
                "
                    SELECT e.canonical_name FROM kg_entities e
                    WHERE e.id IN {WS_ENTITY_SCOPE}
                      AND NOT EXISTS (
                        SELECT 1 FROM kg_relationships r
                        WHERE r.source_entity_id = e.id OR r.target_entity_id = e.id
                    )
                    LIMIT 10
                    "
            ))?;
            let rows = stmt.query_map([workspace_id], |row| row.get::<_, String>(0))?;
            let mut isolated_parts = Vec::new();
            for row in rows {
                isolated_parts.push(format!("- {}", row?));
            }
            let isolated_entities = if isolated_parts.is_empty() {
                "(none)".to_string()
            } else {
                isolated_parts.join("\n")
            };

            Ok((
                entity_summaries,
                type_distribution,
                relationship_stats,
                isolated_entities,
                count,
            ))
        })
        .await?;

        let mut variables = std::collections::BTreeMap::new();
        variables.insert("entity_summaries".to_string(), entity_summaries);
        variables.insert("type_distribution".to_string(), type_distribution);
        variables.insert("relationship_stats".to_string(), relationship_stats);
        variables.insert("isolated_entities".to_string(), isolated_entities);
        insert_workspace_prompt_vars(&mut variables, &context);

        let response = self
            .llm_service
            .execute_prompt("kg_gap_analysis", variables, None, LlmOutputMode::Json)
            .await?;
        let json_output = response.json_output.ok_or_else(|| {
            AppError::Internal("kg_gap_analysis did not return JSON output".to_string())
        })?;

        let issues: Vec<KGGapAnalysisResult> =
            serde_json::from_value(json_output).map_err(|error| {
                AppError::Internal(format!("invalid kg_gap_analysis output: {error}"))
            })?;

        Ok(KGGapAnalysisResponse {
            issues,
            entities_reviewed,
        })
    }

    /// The "gap finder": combines the workspace's primary question + gap note
    /// with the scoped KG gap analysis and asks the LLM for the refined / next
    /// research question. Persists the result and returns the refined question.
    pub async fn generate_gap_bridge(
        &self,
        workspace_id: i64,
        primary_question: String,
        gap_note: String,
    ) -> Result<String, AppError> {
        let gaps = self.analyze_gaps(workspace_id).await?;
        let gap_issues = if gaps.issues.is_empty() {
            "(no structural gaps detected)".to_string()
        } else {
            gaps.issues
                .iter()
                .map(|issue| {
                    format!(
                        "- {} [{}]: {} (confidence {:.2})",
                        issue.entity_name, issue.issue_type, issue.suggestion, issue.confidence
                    )
                })
                .collect::<Vec<_>>()
                .join("\n")
        };

        let mut variables = std::collections::BTreeMap::new();
        variables.insert("primary_question".to_string(), primary_question);
        variables.insert("gap_note".to_string(), gap_note);
        variables.insert("gap_issues".to_string(), gap_issues);

        let response = self
            .llm_service
            .execute_prompt("gap_finder", variables, None, LlmOutputMode::Text)
            .await?;
        let refined = response.raw_text.trim().to_string();

        let database_path = self.database_path.clone();
        let refined_for_db = refined.clone();
        let issues_json = serde_json::to_string(&gaps.issues).unwrap_or_else(|_| "[]".to_string());
        let entities_reviewed = gaps.entities_reviewed;
        run_blocking(move || {
            let conn = crate::db::open_connection(&*database_path)?;
            conn.execute(
                "INSERT INTO kg_gap_findings (workspace_id, entities_reviewed, issues_json, refined_question)
                 VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![workspace_id, entities_reviewed, issues_json, refined_for_db],
            )?;
            Ok(())
        })
        .await?;

        Ok(refined)
    }

    // --- Synthesis Compilation ---

    pub fn get_synthesis_compile_status(&self) -> Result<KGSynthesisCompileStatus, AppError> {
        let state = self.synthesis_compile_state.lock().map_err(|_| {
            AppError::Internal("knowledge graph synthesis compile state is poisoned".to_string())
        })?;
        Ok(state.clone())
    }

    pub async fn start_synthesis_compilation(
        &self,
        batch_size: u32,
        force_all: bool,
        entity_ids: Option<Vec<i64>>,
    ) -> Result<KGSynthesisCompileStartResponse, AppError> {
        let total_eligible = self
            .count_synthesis_candidates(force_all, entity_ids.as_deref())
            .await?;
        let actual_batch = total_eligible.min(i64::from(batch_size.max(1)));

        self.update_synthesis_compile(|state| {
            if state.running {
                return Err(AppError::Conflict(
                    "Synthesis compilation already in progress".to_string(),
                ));
            }
            *state = KGSynthesisCompileStatus {
                running: true,
                processed: 0,
                compiled: 0,
                failed: 0,
                total: actual_batch,
                current_entity_id: None,
                current_entity_index: None,
                error: None,
            };
            Ok(())
        })?;

        let service = self.clone();
        tokio::spawn(async move {
            let finisher = service.clone();
            let handle = tokio::spawn(async move {
                service
                    .run_synthesis_compilation(batch_size.max(1), force_all, entity_ids)
                    .await;
            });

            if let Err(error) = handle.await {
                let message = if error.is_panic() {
                    "synthesis compilation worker panicked".to_string()
                } else {
                    format!("synthesis compilation worker stopped unexpectedly: {error}")
                };
                error!("{message}");
                let _ = finisher.finish_synthesis_compile(Some(message));
            }
        });

        Ok(KGSynthesisCompileStartResponse {
            status: "started".to_string(),
            message: format!("Synthesis compilation started for up to {actual_batch} entities"),
            total_entities: actual_batch,
        })
    }

    async fn run_synthesis_compilation(
        self,
        batch_size: u32,
        force_all: bool,
        entity_ids: Option<Vec<i64>>,
    ) {
        let entities = match self
            .load_synthesis_candidates(batch_size, force_all, entity_ids.as_deref())
            .await
        {
            Ok(entities) => entities,
            Err(error) => {
                error!("synthesis compilation failed to load entities: {error}");
                let _ = self.finish_synthesis_compile(Some(error.to_string()));
                return;
            }
        };

        if let Err(error) = self.update_synthesis_compile(|state| {
            state.total = entities.len() as i64;
            Ok(())
        }) {
            error!("synthesis compile state update failed: {error}");
            return;
        }

        let total_entities = entities.len() as i64;
        for (entity_index, entity_id) in entities.into_iter().enumerate() {
            let current_entity_index = entity_index as i64 + 1;
            if let Err(error) = self.update_synthesis_compile(|state| {
                state.current_entity_id = Some(entity_id);
                state.current_entity_index = Some(current_entity_index);
                Ok(())
            }) {
                error!("synthesis compile state update failed: {error}");
                return;
            }
            info!(
                entity_id,
                entity_index = current_entity_index,
                total = total_entities,
                "synthesis compilation entity started"
            );

            let result = self.compile_single_synthesis(entity_id).await;

            // After compiling entity, compile evidence for top relationships lacking it.
            if result.is_ok() {
                if let Err(evidence_error) = self.compile_top_relationship_evidence(entity_id).await
                {
                    warn!(
                        "relationship evidence compilation failed for entity {entity_id}: {evidence_error}"
                    );
                }
            }

            if let Err(update_error) = self.update_synthesis_compile(|state| {
                state.processed += 1;
                state.current_entity_id = None;
                state.current_entity_index = None;
                match result {
                    Ok(()) => {
                        state.compiled += 1;
                        info!(
                            entity_id,
                            entity_index = current_entity_index,
                            total = total_entities,
                            "synthesis compilation entity finished"
                        );
                    }
                    Err(error) => {
                        warn!(
                            "synthesis compilation failed for entity {}: {}",
                            entity_id, error
                        );
                        state.failed += 1;
                        if state.error.is_none() {
                            state.error = Some(error.to_string());
                        }
                    }
                }
                Ok(())
            }) {
                error!("synthesis compile state update failed: {update_error}");
                return;
            }
        }

        if let Err(error) = self.finish_synthesis_compile(None) {
            error!("synthesis compilation finish failed: {error}");
        }
    }

    fn update_synthesis_compile(
        &self,
        updater: impl FnOnce(&mut KGSynthesisCompileStatus) -> Result<(), AppError>,
    ) -> Result<(), AppError> {
        let mut state = self.synthesis_compile_state.lock().map_err(|_| {
            AppError::Internal("knowledge graph synthesis compile state is poisoned".to_string())
        })?;
        updater(&mut state)
    }

    fn finish_synthesis_compile(&self, error_message: Option<String>) -> Result<(), AppError> {
        self.update_synthesis_compile(|state| {
            state.running = false;
            state.current_entity_id = None;
            state.current_entity_index = None;
            if let Some(error_message) = error_message {
                state.error = Some(error_message);
            }
            Ok(())
        })
    }

    async fn count_synthesis_candidates(
        &self,
        force_all: bool,
        entity_ids: Option<&[i64]>,
    ) -> Result<i64, AppError> {
        let database_path = self.database_path.clone();
        let entity_ids = entity_ids.map(|ids| ids.to_vec());

        run_blocking(move || {
            let conn = crate::db::open_connection(&*database_path)?;

            if let Some(ids) = entity_ids {
                // Count only specified entities
                let placeholders = vec!["?"; ids.len()].join(", ");
                let sql = format!("SELECT COUNT(*) FROM kg_entities WHERE id IN ({placeholders})");
                let params: Vec<Value> = ids.into_iter().map(Value::Integer).collect();
                let count = conn.query_row(&sql, params_from_iter(params.iter()), |row| {
                    row.get::<_, i64>(0)
                })?;
                Ok(count)
            } else if force_all {
                // Count all entities that have at least one article mention
                let sql = format!(
                    "
                    SELECT COUNT(*) FROM (
                        SELECT e.id
                        FROM kg_entities e
                        JOIN kg_article_entities kae ON kae.entity_id = e.id
                        WHERE {WIKI_ENTITY_FILTER_SQL}
                        GROUP BY e.id
                        HAVING COUNT(DISTINCT kae.article_uid) >= ?1
                    )
                    "
                );
                let count =
                    conn.query_row(&sql, [WIKI_MIN_SOURCE_ARTICLES], |row| row.get::<_, i64>(0))?;
                Ok(count)
            } else {
                // Count wiki-eligible entities with stale syntheses or no synthesis at all.
                let sql = format!(
                    "
                    SELECT COUNT(*) FROM (
                        SELECT e.id
                        FROM kg_entities e
                        JOIN kg_article_entities kae ON kae.entity_id = e.id
                        LEFT JOIN kg_entity_syntheses s ON s.entity_id = e.id
                        WHERE {WIKI_ENTITY_FILTER_SQL}
                          AND (s.entity_id IS NULL OR s.stale = 1)
                        GROUP BY e.id
                        HAVING COUNT(DISTINCT kae.article_uid) >= ?1
                    )
                    "
                );
                let count =
                    conn.query_row(&sql, [WIKI_MIN_SOURCE_ARTICLES], |row| row.get::<_, i64>(0))?;
                Ok(count)
            }
        })
        .await
    }

    async fn load_synthesis_candidates(
        &self,
        batch_size: u32,
        force_all: bool,
        entity_ids: Option<&[i64]>,
    ) -> Result<Vec<i64>, AppError> {
        let database_path = self.database_path.clone();
        let batch_size = i64::from(batch_size.max(1));
        let entity_ids = entity_ids.map(|ids| ids.to_vec());

        run_blocking(move || {
            let conn = crate::db::open_connection(&*database_path)?;

            if let Some(ids) = entity_ids {
                let placeholders = vec!["?"; ids.len()].join(", ");
                let sql =
                    format!("SELECT id FROM kg_entities WHERE id IN ({placeholders}) LIMIT ?",);
                let mut params: Vec<Value> = ids.into_iter().map(Value::Integer).collect();
                params.push(Value::Integer(batch_size));
                let mut stmt = conn.prepare(&sql)?;
                let rows =
                    stmt.query_map(params_from_iter(params.iter()), |row| row.get::<_, i64>(0))?;
                rows.collect::<Result<Vec<_>, _>>()
                    .map_err(anyhow::Error::from)
            } else if force_all {
                let sql = format!(
                    "
                    SELECT e.id
                    FROM kg_entities e
                    JOIN kg_article_entities kae ON kae.entity_id = e.id
                    WHERE {WIKI_ENTITY_FILTER_SQL}
                    GROUP BY e.id
                    HAVING COUNT(DISTINCT kae.article_uid) >= ?1
                    ORDER BY
                      COUNT(DISTINCT kae.article_uid) DESC,
                      CASE UPPER(COALESCE(e.entity_type, ''))
                        WHEN 'CONCEPT' THEN 1
                        WHEN 'TECHNOLOGY' THEN 2
                        WHEN 'METHODOLOGY' THEN 3
                        WHEN 'REGULATION' THEN 4
                        WHEN 'MEDICAL_CONDITION' THEN 5
                        WHEN 'DATASET' THEN 6
                        WHEN 'ORGANIZATION' THEN 7
                        ELSE 8
                      END,
                      e.mention_count DESC,
                      e.canonical_name ASC
                    LIMIT ?2
                    "
                );
                let mut stmt = conn.prepare(&sql)?;
                let rows = stmt
                    .query_map(params![WIKI_MIN_SOURCE_ARTICLES, batch_size], |row| {
                        row.get::<_, i64>(0)
                    })?;
                rows.collect::<Result<Vec<_>, _>>()
                    .map_err(anyhow::Error::from)
            } else {
                let sql = format!(
                    "
                    SELECT e.id
                    FROM kg_entities e
                    JOIN kg_article_entities kae ON kae.entity_id = e.id
                    LEFT JOIN kg_entity_syntheses s ON s.entity_id = e.id
                    WHERE {WIKI_ENTITY_FILTER_SQL}
                      AND (s.entity_id IS NULL OR s.stale = 1)
                    GROUP BY e.id
                    HAVING COUNT(DISTINCT kae.article_uid) >= ?1
                    ORDER BY
                      COUNT(DISTINCT kae.article_uid) DESC,
                      CASE UPPER(COALESCE(e.entity_type, ''))
                        WHEN 'CONCEPT' THEN 1
                        WHEN 'TECHNOLOGY' THEN 2
                        WHEN 'METHODOLOGY' THEN 3
                        WHEN 'REGULATION' THEN 4
                        WHEN 'MEDICAL_CONDITION' THEN 5
                        WHEN 'DATASET' THEN 6
                        WHEN 'ORGANIZATION' THEN 7
                        ELSE 8
                      END,
                      e.mention_count DESC,
                      e.canonical_name ASC
                    LIMIT ?2
                    "
                );
                let mut stmt = conn.prepare(&sql)?;
                let rows = stmt
                    .query_map(params![WIKI_MIN_SOURCE_ARTICLES, batch_size], |row| {
                        row.get::<_, i64>(0)
                    })?;
                rows.collect::<Result<Vec<_>, _>>()
                    .map_err(anyhow::Error::from)
            }
        })
        .await
    }

    async fn gather_synthesis_context(&self, entity_id: i64) -> Result<SynthesisContext, AppError> {
        let database_path = self.database_path.clone();

        run_blocking(move || {
            let conn = crate::db::open_connection(&*database_path)?;

            // 1. Entity metadata
            let (entity_name, entity_type, description, aliases_json): (
                String,
                String,
                Option<String>,
                Option<String>,
            ) = conn.query_row(
                "SELECT canonical_name, entity_type, description, aliases_json
                 FROM kg_entities WHERE id = ?",
                [entity_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get::<_, Option<String>>(1)?
                            .unwrap_or_else(|| "UNKNOWN".to_string()),
                        row.get(2)?,
                        row.get(3)?,
                    ))
                },
            )?;

            let aliases = parse_string_list(aliases_json);
            let aliases_str = if aliases.is_empty() {
                "(none)".to_string()
            } else {
                aliases.join(", ")
            };

            // 2. Article mention contexts (limited to 20, truncated total ~6000 chars)
            let mut stmt = conn.prepare(
                "SELECT kae.mention_text, kae.context, h.title, h.first_author
                 FROM kg_article_entities kae
                 JOIN haie_rev h ON h.uid = kae.article_uid
                 WHERE kae.entity_id = ?
                 ORDER BY kae.id DESC
                 LIMIT 20",
            )?;
            let mention_rows = stmt.query_map([entity_id], |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            })?;

            let mut mention_contexts = String::new();
            for row in mention_rows {
                let (mention_text, context, title, first_author) = row?;
                let title_str = title.unwrap_or_else(|| "Unknown Article".to_string());
                let author_str = first_author.unwrap_or_else(|| "Unknown".to_string());
                let context_str = context
                    .or(mention_text)
                    .unwrap_or_else(|| "(no context)".to_string());

                let entry = format!("### From \"{title_str}\" ({author_str})\nContext: {context_str}\n\n");

                if mention_contexts.len() + entry.len() > 6000 {
                    break;
                }
                mention_contexts.push_str(&entry);
            }
            if mention_contexts.is_empty() {
                mention_contexts = "(no mention contexts available)".to_string();
            }

            // 3. Relationships with neighbor names
            let mut stmt = conn.prepare(
                "SELECT
                   CASE WHEN r.source_entity_id = ?1 THEN te.canonical_name ELSE se.canonical_name END,
                   CASE WHEN r.source_entity_id = ?1 THEN te.entity_type ELSE se.entity_type END,
                   r.relationship_type, r.weight
                 FROM kg_relationships r
                 JOIN kg_entities se ON se.id = r.source_entity_id
                 JOIN kg_entities te ON te.id = r.target_entity_id
                 WHERE r.source_entity_id = ?1 OR r.target_entity_id = ?1
                 ORDER BY r.weight DESC
                 LIMIT 20",
            )?;
            let rel_rows = stmt.query_map([entity_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?
                        .unwrap_or_else(|| "UNKNOWN".to_string()),
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<f64>>(3)?.unwrap_or(1.0),
                ))
            })?;

            let mut relationships = String::new();
            let mut neighbor_parts = Vec::new();
            for row in rel_rows {
                let (neighbor, neighbor_type, rel_type, _weight) = row?;
                relationships.push_str(&format!(
                    "- {} -> {neighbor} ({neighbor_type})\n",
                    rel_type.to_uppercase()
                ));
                neighbor_parts.push(format!("{neighbor} ({neighbor_type})"));
            }
            if relationships.is_empty() {
                relationships = "(no relationships)".to_string();
            }
            let neighbor_entities = if neighbor_parts.is_empty() {
                "(none)".to_string()
            } else {
                neighbor_parts.join(", ")
            };

            // 4. Current synthesis if exists
            let current_synthesis = conn
                .query_row(
                    "SELECT synthesis FROM kg_entity_syntheses WHERE entity_id = ?",
                    [entity_id],
                    |row| row.get::<_, Option<String>>(0),
                )
                .optional()?
                .flatten()
                .unwrap_or_default();
            let current_synthesis = if current_synthesis.is_empty() {
                "(first compilation)".to_string()
            } else {
                current_synthesis
            };

            Ok(SynthesisContext {
                entity_name,
                entity_type: description
                    .map(|d| format!("{entity_type} — {d}"))
                    .unwrap_or(entity_type),
                aliases: aliases_str,
                mention_contexts,
                relationships,
                neighbor_entities,
                current_synthesis,
            })
        })
        .await
    }

    async fn compile_single_synthesis(&self, entity_id: i64) -> Result<(), AppError> {
        let context = self.gather_synthesis_context(entity_id).await?;
        let research_context = self.research_context().await;

        let mut variables = std::collections::BTreeMap::new();
        variables.insert("entity_name".to_string(), context.entity_name.clone());
        variables.insert("entity_type".to_string(), context.entity_type);
        variables.insert("aliases".to_string(), context.aliases);
        variables.insert("mention_contexts".to_string(), context.mention_contexts);
        variables.insert("relationships".to_string(), context.relationships);
        variables.insert("neighbor_entities".to_string(), context.neighbor_entities);
        variables.insert("current_synthesis".to_string(), context.current_synthesis);
        insert_workspace_prompt_vars(&mut variables, &research_context);

        let response = self
            .llm_service
            .execute_prompt("entity_synthesis", variables, None, LlmOutputMode::Json)
            .await?;
        let json_output = response.json_output.ok_or_else(|| {
            AppError::Internal("entity_synthesis did not return JSON output".to_string())
        })?;
        let output: SynthesisGenerationOutput =
            serde_json::from_value(json_output).map_err(|error| {
                AppError::Internal(format!("invalid entity_synthesis output: {error}"))
            })?;
        // An empty/truncated synthesis must not overwrite a previous good one;
        // failing here keeps the old row and counts as a failed compile.
        validate_synthesis_output(&output)?;

        // Count source articles
        let database_path = self.database_path.clone();
        let key_aspects_json =
            serde_json::to_string(&output.key_aspects).unwrap_or_else(|_| "[]".to_string());
        let related_entities_json =
            serde_json::to_string(&output.related_entities).unwrap_or_else(|_| "[]".to_string());
        let summary = output.summary;
        let synthesis = output.synthesis;

        run_blocking(move || {
            let conn = crate::db::open_connection(&*database_path)?;

            let source_article_count: i64 = conn.query_row(
                "SELECT COUNT(DISTINCT article_uid) FROM kg_article_entities WHERE entity_id = ?",
                [entity_id],
                |row| row.get(0),
            )?;

            conn.execute(
                "INSERT INTO kg_entity_syntheses (entity_id, synthesis, summary, key_aspects_json, related_entities_json, source_article_count, compiled_at, stale, version)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, datetime('now'), 0, 1)
                 ON CONFLICT(entity_id) DO UPDATE SET
                   synthesis = excluded.synthesis,
                   summary = excluded.summary,
                   key_aspects_json = excluded.key_aspects_json,
                   related_entities_json = excluded.related_entities_json,
                   source_article_count = excluded.source_article_count,
                   compiled_at = excluded.compiled_at,
                   stale = 0,
                   version = version + 1,
                   updated_at = datetime('now')",
                params![
                    entity_id,
                    synthesis,
                    summary,
                    key_aspects_json,
                    related_entities_json,
                    source_article_count,
                ],
            )?;

            Ok(())
        })
        .await?;

        if let Err(error) = self.export_wiki_pages(entity_id).await {
            warn!("wiki Markdown export failed for entity {entity_id}: {error}");
            // The synthesis itself is good, but the on-disk wiki page is now
            // out of date — flip the row back to stale so the next compile
            // pass re-exports it.
            let database_path = self.database_path.clone();
            run_blocking(move || {
                let conn = crate::db::open_connection(&*database_path)?;
                conn.execute(
                    "UPDATE kg_entity_syntheses SET stale = 1, updated_at = datetime('now')
                     WHERE entity_id = ?1",
                    [entity_id],
                )?;
                Ok(())
            })
            .await?;
        }

        Ok(())
    }

    async fn export_wiki_pages(&self, entity_id: i64) -> Result<(), AppError> {
        let database_path = self.database_path.clone();
        let daily_date = Local::now().date_naive().to_string();
        let daily_date_for_query = daily_date.clone();

        let payload = run_blocking(move || {
            let conn = crate::db::open_connection(&*database_path)?;
            let entity = load_wiki_entity(&conn, entity_id)?;
            let sources = load_wiki_sources_for_entity(&conn, entity_id)?;
            let all_sources = load_all_wiki_sources(&conn)?;
            let index = load_wiki_index(&conn)?;
            let daily_sources = load_wiki_daily_sources(&conn, &daily_date_for_query)?;

            Ok(WikiExportPayload {
                entity,
                sources,
                all_sources,
                index,
                daily_date,
                daily_sources,
            })
        })
        .await?;

        let root = self.wiki_export_dir.as_ref();
        let entities_dir = root.join("entities");
        let sources_dir = root.join("sources");
        let daily_dir = root.join("daily");
        tokio::fs::create_dir_all(&entities_dir).await?;
        tokio::fs::create_dir_all(&sources_dir).await?;
        tokio::fs::create_dir_all(&daily_dir).await?;

        let entity_file =
            entity_markdown_filename(payload.entity.entity_id, &payload.entity.entity_name);
        tokio::fs::write(
            entities_dir.join(&entity_file),
            render_entity_markdown(&payload.entity, &payload.sources, &payload.index),
        )
        .await?;

        for source in &payload.all_sources {
            tokio::fs::write(
                sources_dir.join(source_markdown_filename(&source.uid)),
                render_source_markdown(source),
            )
            .await?;
        }

        for source in &payload.sources {
            tokio::fs::write(
                sources_dir.join(source_markdown_filename(&source.uid)),
                render_source_markdown(source),
            )
            .await?;
        }
        let entity_source_uids = payload
            .sources
            .iter()
            .map(|source| source.uid.as_str())
            .collect::<BTreeSet<_>>();
        for source in &payload.daily_sources {
            if entity_source_uids.contains(source.uid.as_str()) {
                continue;
            }
            tokio::fs::write(
                sources_dir.join(source_markdown_filename(&source.uid)),
                render_source_markdown(source),
            )
            .await?;
        }

        tokio::fs::write(root.join("index.md"), render_index_markdown(&payload.index)).await?;
        tokio::fs::write(
            daily_dir.join(format!("{}.md", payload.daily_date)),
            render_daily_markdown(&payload.daily_date, &payload.daily_sources),
        )
        .await?;

        Ok(())
    }

    async fn compile_top_relationship_evidence(&self, entity_id: i64) -> Result<(), AppError> {
        // Load top 5 relationships by weight that lack evidence_summary.
        let database_path = self.database_path.clone();
        let relationship_ids = run_blocking(move || {
            let conn = crate::db::open_connection(&*database_path)?;
            let mut stmt = conn.prepare(
                "SELECT id FROM kg_relationships
                 WHERE (source_entity_id = ?1 OR target_entity_id = ?1)
                   AND evidence_summary IS NULL
                 ORDER BY weight DESC
                 LIMIT 5",
            )?;
            let rows = stmt.query_map([entity_id], |row| row.get::<_, i64>(0))?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(anyhow::Error::from)
        })
        .await?;

        for relationship_id in relationship_ids {
            if let Err(error) = self.compile_relationship_evidence(relationship_id).await {
                warn!("evidence compilation failed for relationship {relationship_id}: {error}");
            }
        }

        Ok(())
    }

    async fn compile_relationship_evidence(&self, relationship_id: i64) -> Result<(), AppError> {
        // 1. Load relationship with entity names.
        let database_path = self.database_path.clone();
        let rel_info = run_blocking(move || {
            let conn = crate::db::open_connection(&*database_path)?;
            let (source_name, target_name, rel_type, source_id, target_id): (
                String,
                String,
                String,
                i64,
                i64,
            ) = conn.query_row(
                "SELECT se.canonical_name, te.canonical_name, r.relationship_type,
                        r.source_entity_id, r.target_entity_id
                 FROM kg_relationships r
                 JOIN kg_entities se ON se.id = r.source_entity_id
                 JOIN kg_entities te ON te.id = r.target_entity_id
                 WHERE r.id = ?",
                [relationship_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )?;

            // 2. Load article contexts for both entities.
            let mut stmt = conn.prepare(
                "SELECT DISTINCT h.title, h.first_author, kae.context, kae.mention_text
                 FROM kg_article_entities kae
                 JOIN haie_rev h ON h.uid = kae.article_uid
                 WHERE kae.entity_id IN (?1, ?2)
                   AND kae.article_uid IN (
                     SELECT DISTINCT article_uid FROM kg_article_entities WHERE entity_id = ?1
                     INTERSECT
                     SELECT DISTINCT article_uid FROM kg_article_entities WHERE entity_id = ?2
                   )
                 ORDER BY kae.id DESC
                 LIMIT 10",
            )?;
            let rows = stmt.query_map(params![source_id, target_id], |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            })?;

            let mut article_contexts = String::new();
            for row in rows {
                let (title, author, context, mention) = row?;
                let title_str = title.unwrap_or_else(|| "Unknown Article".to_string());
                let author_str = author.unwrap_or_else(|| "Unknown".to_string());
                let context_str = context
                    .or(mention)
                    .unwrap_or_else(|| "(no context)".to_string());
                let entry = format!("### From \"{title_str}\" ({author_str})\n{context_str}\n\n");
                if article_contexts.len() + entry.len() > 4000 {
                    break;
                }
                article_contexts.push_str(&entry);
            }
            if article_contexts.is_empty() {
                article_contexts = "(no shared article contexts available)".to_string();
            }

            Ok((source_name, target_name, rel_type, article_contexts))
        })
        .await?;

        let (source_name, target_name, rel_type, article_contexts) = rel_info;

        // 3. Call LLM.
        let mut variables = std::collections::BTreeMap::new();
        variables.insert("source_entity".to_string(), source_name);
        variables.insert("target_entity".to_string(), target_name);
        variables.insert("relationship_type".to_string(), rel_type);
        variables.insert("article_contexts".to_string(), article_contexts);
        let research_context = self.research_context().await;
        insert_workspace_prompt_vars(&mut variables, &research_context);

        let response = self
            .llm_service
            .execute_prompt(
                "relationship_evidence",
                variables,
                None,
                LlmOutputMode::Json,
            )
            .await?;
        let json_output = response.json_output.ok_or_else(|| {
            AppError::Internal("relationship_evidence did not return JSON output".to_string())
        })?;
        let output: RelationshipEvidenceOutput =
            serde_json::from_value(json_output).map_err(|error| {
                AppError::Internal(format!("invalid relationship_evidence output: {error}"))
            })?;

        // 4. Update the relationship.
        let database_path = self.database_path.clone();
        let evidence = output.evidence_summary;
        run_blocking(move || {
            let conn = crate::db::open_connection(&*database_path)?;
            conn.execute(
                "UPDATE kg_relationships SET evidence_summary = ? WHERE id = ?",
                params![evidence, relationship_id],
            )?;
            Ok(())
        })
        .await
    }
}

struct SynthesisContext {
    entity_name: String,
    entity_type: String,
    aliases: String,
    mention_contexts: String,
    relationships: String,
    neighbor_entities: String,
    current_synthesis: String,
}

fn insert_workspace_prompt_vars(
    variables: &mut BTreeMap<String, String>,
    context: &WorkspaceResearchContext,
) {
    variables.insert(
        "workspace_name".to_string(),
        empty_prompt_value(&context.name),
    );
    variables.insert(
        "collection_context".to_string(),
        context.collection_context(),
    );
    variables.insert(
        "topic_descriptor".to_string(),
        empty_prompt_value(&context.topic_descriptor),
    );
    variables.insert(
        "primary_question".to_string(),
        empty_prompt_value(&context.primary_question),
    );
    variables.insert("seed_concepts".to_string(), context.seed_concepts_text());
    variables.insert(
        "refined_question".to_string(),
        empty_prompt_value(&context.refined_question),
    );
}

fn empty_prompt_value(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        "(not set)".to_string()
    } else {
        trimmed.to_string()
    }
}

fn query_entities(
    conn: &Connection,
    query: &str,
    limit: i64,
) -> Result<Vec<MatchedEntity>, anyhow::Error> {
    let query_like = format!("%{}%", query.to_lowercase());
    let query_prefix = format!("{}%", query.to_lowercase());

    let mut stmt = conn.prepare(
        "
        SELECT e.id, e.canonical_name, e.entity_type, e.description, e.mention_count,
               e.aliases_json, s.summary
        FROM kg_entities e
        LEFT JOIN kg_entity_syntheses s ON s.entity_id = e.id
        WHERE lower(e.canonical_name) LIKE ?1
           OR lower(COALESCE(e.aliases_json, '')) LIKE ?1
        ORDER BY
            CASE
                WHEN lower(e.canonical_name) = lower(?2) THEN 0
                WHEN lower(e.canonical_name) LIKE ?3 THEN 1
                ELSE 2
            END,
            e.mention_count DESC,
            e.canonical_name ASC
        LIMIT ?4
        ",
    )?;

    let rows = stmt.query_map(
        [
            &query_like as &dyn rusqlite::ToSql,
            &query,
            &query_prefix,
            &limit,
        ],
        |row| {
            let name: String = row.get(1)?;
            let aliases = parse_string_list(row.get::<_, Option<String>>(5)?);
            Ok(MatchedEntity {
                id: row.get(0)?,
                similarity: compute_similarity(query, &name, &aliases),
                name,
                entity_type: row
                    .get::<_, Option<String>>(2)?
                    .unwrap_or_else(|| "UNKNOWN".to_string()),
                description: row.get(3)?,
                mention_count: row.get::<_, Option<i64>>(4)?.unwrap_or(0),
                aliases,
                synthesis_summary: row.get(6)?,
            })
        },
    )?;

    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

fn query_relationships(
    conn: &Connection,
    entity_ids: &[i64],
    limit: i64,
) -> Result<Vec<KGSearchRelationship>, anyhow::Error> {
    if entity_ids.is_empty() {
        return Ok(Vec::new());
    }

    let placeholders = vec!["?"; entity_ids.len()].join(", ");
    let sql = format!(
        "
        SELECT src.canonical_name, tgt.canonical_name, rel.relationship_type, rel.weight,
               rel.source_articles_json
        FROM kg_relationships rel
        JOIN kg_entities src ON src.id = rel.source_entity_id
        JOIN kg_entities tgt ON tgt.id = rel.target_entity_id
        WHERE rel.source_entity_id IN ({placeholders})
           OR rel.target_entity_id IN ({placeholders})
        ORDER BY rel.weight DESC, src.canonical_name ASC, tgt.canonical_name ASC
        LIMIT ?
        "
    );

    let mut params = entity_ids
        .iter()
        .copied()
        .map(Value::Integer)
        .collect::<Vec<_>>();
    params.extend(entity_ids.iter().copied().map(Value::Integer));
    params.push(Value::Integer(limit));

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(params.iter()), |row| {
        let source_articles = parse_string_list(row.get::<_, Option<String>>(4)?);
        Ok(KGSearchRelationship {
            source: row.get(0)?,
            target: row.get(1)?,
            relationship_type: row.get(2)?,
            weight: row.get::<_, Option<f64>>(3)?.unwrap_or(1.0),
            article_count: source_articles.len() as i64,
        })
    })?;

    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

fn query_sources(
    conn: &Connection,
    entity_ids: &[i64],
    limit: i64,
) -> Result<Vec<KGSearchSource>, anyhow::Error> {
    if entity_ids.is_empty() {
        return Ok(Vec::new());
    }

    let placeholders = vec!["?"; entity_ids.len()].join(", ");
    let sql = format!(
        "
        SELECT DISTINCT kae.article_uid,
               COALESCE(art.title, kae.article_uid),
               art.url,
               COALESCE(ac.content, kae.context, kae.mention_text, ''),
               COALESCE(ac.chunk_index, kae.chunk_index, 0),
               COALESCE(ac.chunk_type, 'kg_context')
        FROM kg_article_entities kae
        JOIN haie_rev art ON art.uid = kae.article_uid
        LEFT JOIN article_chunks ac
          ON ac.article_uid = kae.article_uid
         AND ac.chunk_index = kae.chunk_index
        WHERE kae.entity_id IN ({placeholders})
        ORDER BY COALESCE(art.reg_date, '') DESC, kae.article_uid ASC, COALESCE(ac.chunk_index, kae.chunk_index, 0) ASC
        LIMIT ?
        "
    );

    let mut params = entity_ids
        .iter()
        .copied()
        .map(Value::Integer)
        .collect::<Vec<_>>();
    params.push(Value::Integer(limit));

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(params.iter()), |row| {
        let chunk_index = row.get::<_, Option<i64>>(4)?.unwrap_or(0);
        let chunk_type = row
            .get::<_, Option<String>>(5)?
            .unwrap_or_else(|| "kg_context".to_string());
        Ok(KGSearchSource {
            article_uid: row.get(0)?,
            title: row.get(1)?,
            url: row.get(2)?,
            chunk_content: truncate_text(row.get::<_, Option<String>>(3)?.unwrap_or_default(), 320),
            similarity: 1.0,
            chunk_reference: format!("{chunk_type} #{chunk_index}"),
        })
    })?;

    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

fn build_context(sources: &[KGSearchSource]) -> Option<String> {
    if sources.is_empty() {
        return None;
    }

    let mut parts = Vec::new();
    for source in sources.iter().take(5) {
        parts.push(format!(
            "[{}] {}\n{}",
            source.chunk_reference, source.title, source.chunk_content
        ));
    }
    Some(parts.join("\n\n"))
}

#[derive(Debug)]
struct WikiExportPayload {
    entity: WikiEntityPage,
    sources: Vec<WikiSourceArticle>,
    all_sources: Vec<WikiSourceArticle>,
    index: Vec<KGEntitySynthesisSummary>,
    daily_date: String,
    daily_sources: Vec<WikiSourceArticle>,
}

#[derive(Debug)]
struct WikiEntityPage {
    entity_id: i64,
    entity_name: String,
    entity_type: String,
    summary: String,
    synthesis: String,
    key_aspects: Vec<String>,
    related_entities: Vec<KGSynthesisRelatedEntity>,
    source_article_count: i64,
    compiled_at: Option<String>,
    stale: bool,
    version: i64,
}

#[derive(Debug)]
struct WikiSourceArticle {
    uid: String,
    title: Option<String>,
    url: Option<String>,
    first_author: Option<String>,
    pub_date: Option<String>,
    journal: Option<String>,
    byline_summary: Option<String>,
    why_it_matters: Option<String>,
    key_argument: Option<String>,
    main_findings: Option<String>,
    reg_date: Option<String>,
}

fn load_wiki_entity(conn: &Connection, entity_id: i64) -> Result<WikiEntityPage, anyhow::Error> {
    let row = conn.query_row(
        "
        SELECT s.entity_id, e.canonical_name, e.entity_type, s.summary, s.synthesis,
               s.key_aspects_json, s.related_entities_json, s.source_article_count,
               s.compiled_at, s.stale, s.version
        FROM kg_entity_syntheses s
        JOIN kg_entities e ON e.id = s.entity_id
        WHERE s.entity_id = ?1
        ",
        [entity_id],
        |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?
                    .unwrap_or_else(|| "UNKNOWN".to_string()),
                row.get::<_, Option<String>>(3)?.unwrap_or_default(),
                row.get::<_, Option<String>>(4)?.unwrap_or_default(),
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, Option<i64>>(7)?.unwrap_or(0),
                row.get::<_, Option<String>>(8)?,
                row.get::<_, Option<i64>>(9)?.unwrap_or(0) != 0,
                row.get::<_, Option<i64>>(10)?.unwrap_or(1),
            ))
        },
    )?;

    let (
        entity_id,
        entity_name,
        entity_type,
        summary,
        synthesis,
        key_aspects_json,
        related_entities_json,
        source_article_count,
        compiled_at,
        stale,
        version,
    ) = row;

    Ok(WikiEntityPage {
        entity_id,
        entity_name,
        entity_type,
        summary,
        synthesis,
        key_aspects: key_aspects_json
            .and_then(|value| serde_json::from_str::<Vec<String>>(&value).ok())
            .unwrap_or_default(),
        related_entities: related_entities_json
            .and_then(|value| serde_json::from_str::<Vec<KGSynthesisRelatedEntity>>(&value).ok())
            .unwrap_or_default(),
        source_article_count,
        compiled_at,
        stale,
        version,
    })
}

fn load_wiki_sources_for_entity(
    conn: &Connection,
    entity_id: i64,
) -> Result<Vec<WikiSourceArticle>, anyhow::Error> {
    let mut stmt = conn.prepare(
        "
        SELECT DISTINCT h.uid, h.title, h.url, h.first_author, h.pub_date, h.journal,
               h.byline_summary, h.why_it_matters, h.key_argument, h.main_findings,
               h.reg_date
        FROM kg_article_entities kae
        JOIN haie_rev h ON h.uid = kae.article_uid
        WHERE kae.entity_id = ?1
        ORDER BY h.pub_date DESC, h.reg_date DESC, h.title ASC
        LIMIT 100
        ",
    )?;
    let rows = stmt.query_map([entity_id], map_wiki_source_row)?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(anyhow::Error::from)
}

fn load_all_wiki_sources(conn: &Connection) -> Result<Vec<WikiSourceArticle>, anyhow::Error> {
    let mut stmt = conn.prepare(
        "
        SELECT uid, title, url, first_author, pub_date, journal, byline_summary,
               why_it_matters, key_argument, main_findings, reg_date
        FROM haie_rev
        WHERE COALESCE(uid, '') != ''
        ORDER BY pub_date DESC, reg_date DESC, title ASC
        ",
    )?;
    let rows = stmt.query_map([], map_wiki_source_row)?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(anyhow::Error::from)
}

fn load_wiki_daily_sources(
    conn: &Connection,
    date: &str,
) -> Result<Vec<WikiSourceArticle>, anyhow::Error> {
    let mut stmt = conn.prepare(
        "
        SELECT uid, title, url, first_author, pub_date, journal, byline_summary,
               why_it_matters, key_argument, main_findings, reg_date
        FROM haie_rev
        WHERE reg_date = ?1
        ORDER BY title ASC
        LIMIT 200
        ",
    )?;
    let rows = stmt.query_map([date], map_wiki_source_row)?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(anyhow::Error::from)
}

fn load_wiki_index(conn: &Connection) -> Result<Vec<KGEntitySynthesisSummary>, anyhow::Error> {
    let sql = format!(
        "
        SELECT s.entity_id, e.canonical_name, e.entity_type, s.summary,
               s.source_article_count, s.stale, s.compiled_at
        FROM kg_entity_syntheses s
        JOIN kg_entities e ON e.id = s.entity_id
        WHERE COALESCE(s.source_article_count, 0) >= ?1
          AND COALESCE(s.stale, 0) = 0
          AND {WIKI_ENTITY_FILTER_SQL}
        ORDER BY s.source_article_count DESC, e.mention_count DESC, e.canonical_name ASC
        LIMIT 500
        "
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([WIKI_MIN_SOURCE_ARTICLES], |row| {
        Ok(KGEntitySynthesisSummary {
            entity_id: row.get(0)?,
            entity_name: row.get(1)?,
            entity_type: row
                .get::<_, Option<String>>(2)?
                .unwrap_or_else(|| "UNKNOWN".to_string()),
            summary: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
            source_article_count: row.get::<_, Option<i64>>(4)?.unwrap_or(0),
            stale: row.get::<_, Option<i64>>(5)?.unwrap_or(0) != 0,
            compiled_at: row.get(6)?,
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(anyhow::Error::from)
}

fn map_wiki_source_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<WikiSourceArticle> {
    Ok(WikiSourceArticle {
        uid: row.get(0)?,
        title: row.get(1)?,
        url: row.get(2)?,
        first_author: row.get(3)?,
        pub_date: row.get(4)?,
        journal: row.get(5)?,
        byline_summary: row.get(6)?,
        why_it_matters: row.get(7)?,
        key_argument: row.get(8)?,
        main_findings: row.get(9)?,
        reg_date: row.get(10)?,
    })
}

fn render_entity_markdown(
    entity: &WikiEntityPage,
    sources: &[WikiSourceArticle],
    index: &[KGEntitySynthesisSummary],
) -> String {
    let mut out = String::new();
    out.push_str("---\n");
    out.push_str("type: entity\n");
    out.push_str(&format!("entity_id: {}\n", entity.entity_id));
    out.push_str(&format!("entity_type: {}\n", entity.entity_type));
    out.push_str(&format!("version: {}\n", entity.version));
    out.push_str(&format!("stale: {}\n", entity.stale));
    if let Some(compiled_at) = &entity.compiled_at {
        out.push_str(&format!("compiled_at: {compiled_at}\n"));
    }
    out.push_str("---\n\n");

    out.push_str(&format!(
        "# {}\n\n",
        clean_markdown_text(&entity.entity_name)
    ));
    if !entity.summary.trim().is_empty() {
        out.push_str(&format!(
            "{}\n\n",
            clean_markdown_text(entity.summary.trim())
        ));
    }
    out.push_str(&format!(
        "- Type: {}\n- Source articles: {}\n- Wiki link: [[{}]]\n\n",
        entity.entity_type,
        entity.source_article_count,
        clean_markdown_text(&entity.entity_name)
    ));

    if !entity.key_aspects.is_empty() {
        out.push_str("## Key Aspects\n\n");
        for aspect in &entity.key_aspects {
            out.push_str(&format!("- {}\n", clean_markdown_text(aspect.trim())));
        }
        out.push('\n');
    }

    if !entity.synthesis.trim().is_empty() {
        out.push_str("## Synthesis\n\n");
        let linked_synthesis = export_wiki_links(entity.synthesis.trim(), index);
        out.push_str(&clean_markdown_text(&linked_synthesis));
        out.push_str("\n\n");
    }

    if !entity.related_entities.is_empty() {
        out.push_str("## Related Entities\n\n");
        for related in &entity.related_entities {
            let label = escape_markdown_link_label(&related.name);
            if let Some(filename) = entity_link_filename(index, &related.name) {
                out.push_str(&format!(
                    "- [{}]({}) - {} ({})\n",
                    label,
                    filename,
                    clean_markdown_text(&related.relationship_type),
                    clean_markdown_text(&related.entity_type)
                ));
            } else {
                out.push_str(&format!(
                    "- [[{}]] - {} ({})\n",
                    clean_markdown_text(&related.name),
                    clean_markdown_text(&related.relationship_type),
                    clean_markdown_text(&related.entity_type)
                ));
            }
        }
        out.push('\n');
    }

    if !sources.is_empty() {
        out.push_str("## Source Articles\n\n");
        for source in sources {
            let title = source.title.as_deref().unwrap_or(&source.uid);
            out.push_str(&format!(
                "- [{}](../sources/{})",
                escape_markdown_link_label(title),
                source_markdown_filename(&source.uid)
            ));
            if let Some(pub_date) = source.pub_date.as_deref().filter(|value| !value.is_empty()) {
                out.push_str(&format!(" ({pub_date})"));
            }
            out.push('\n');
        }
    }

    out
}

fn render_source_markdown(source: &WikiSourceArticle) -> String {
    let title = clean_markdown_text(source.title.as_deref().unwrap_or(&source.uid));
    let mut out = String::new();
    out.push_str("---\n");
    out.push_str("type: source\n");
    out.push_str(&format!("uid: {}\n", source.uid));
    if let Some(reg_date) = &source.reg_date {
        out.push_str(&format!("reg_date: {reg_date}\n"));
    }
    out.push_str("---\n\n");
    out.push_str(&format!("# {title}\n\n"));

    append_optional_line(&mut out, "Author", source.first_author.as_deref());
    append_optional_line(&mut out, "Published", source.pub_date.as_deref());
    append_optional_line(&mut out, "Journal", source.journal.as_deref());
    if let Some(url) = source.url.as_deref().filter(|value| !value.is_empty()) {
        out.push_str(&format!("- URL: <{url}>\n"));
    }
    out.push('\n');

    append_optional_section(&mut out, "Summary", source.byline_summary.as_deref());
    append_optional_section(&mut out, "Why It Matters", source.why_it_matters.as_deref());
    append_optional_section(&mut out, "Key Argument", source.key_argument.as_deref());
    append_optional_section(&mut out, "Main Findings", source.main_findings.as_deref());

    out
}

fn render_index_markdown(index: &[KGEntitySynthesisSummary]) -> String {
    let mut out = String::new();
    out.push_str("# Healthcare AI Ethics Wiki\n\n");
    out.push_str("SQLite is the source of truth. This Markdown tree is regenerated from entity syntheses compiled from daily and backfilled articles.\n\n");

    for item in index {
        out.push_str(&format!(
            "- [{}](entities/{}) - {} sources",
            escape_markdown_link_label(&item.entity_name),
            entity_markdown_filename(item.entity_id, &item.entity_name),
            item.source_article_count
        ));
        if item.stale {
            out.push_str(" - stale");
        }
        if let Some(compiled_at) = &item.compiled_at {
            out.push_str(&format!(" - {compiled_at}"));
        }
        if !item.summary.trim().is_empty() {
            out.push_str(&format!(
                "\n  {}\n",
                clean_markdown_text(item.summary.trim())
            ));
        } else {
            out.push('\n');
        }
    }

    out
}

fn render_daily_markdown(date: &str, sources: &[WikiSourceArticle]) -> String {
    let mut out = String::new();
    out.push_str(&format!("# Daily Articles - {date}\n\n"));

    if sources.is_empty() {
        out.push_str("No articles registered for this date.\n");
        return out;
    }

    for source in sources {
        let title = source.title.as_deref().unwrap_or(&source.uid);
        out.push_str(&format!(
            "- [{}](../sources/{})",
            escape_markdown_link_label(title),
            source_markdown_filename(&source.uid)
        ));
        if let Some(journal) = source.journal.as_deref().filter(|value| !value.is_empty()) {
            out.push_str(&format!(" - {}", clean_markdown_text(journal)));
        }
        out.push('\n');
    }

    out
}

fn append_optional_line(out: &mut String, label: &str, value: Option<&str>) {
    if let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) {
        out.push_str(&format!("- {label}: {}\n", clean_markdown_text(value)));
    }
}

fn append_optional_section(out: &mut String, heading: &str, value: Option<&str>) {
    if let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) {
        out.push_str(&format!(
            "## {heading}\n\n{}\n\n",
            clean_markdown_text(value)
        ));
    }
}

fn entity_markdown_filename(entity_id: i64, entity_name: &str) -> String {
    let slug = wiki_slug(entity_name);
    if slug.is_empty() {
        format!("entity-{entity_id}.md")
    } else {
        format!("{entity_id}-{slug}.md")
    }
}

fn source_markdown_filename(uid: &str) -> String {
    let slug = wiki_slug(uid);
    if slug.is_empty() {
        format!("source-{}.md", urlencoding::encode(uid))
    } else {
        format!("{slug}.md")
    }
}

fn wiki_slug(value: &str) -> String {
    let mut slug = String::new();
    let mut last_dash = false;

    for character in value.chars() {
        if character.is_ascii_alphanumeric() {
            slug.push(character.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash && !slug.is_empty() {
            slug.push('-');
            last_dash = true;
        }
    }

    slug.trim_matches('-').to_string()
}

fn escape_markdown_link_label(value: &str) -> String {
    clean_markdown_text(value)
        .replace('[', "\\[")
        .replace(']', "\\]")
}

fn clean_markdown_text(value: &str) -> String {
    value
        .chars()
        .filter(|character| !character.is_control() || matches!(*character, '\n' | '\r' | '\t'))
        .collect()
}

fn export_wiki_links(markdown: &str, index: &[KGEntitySynthesisSummary]) -> String {
    let mut rendered = String::with_capacity(markdown.len());
    let mut rest = markdown;

    while let Some(start) = rest.find("[[") {
        rendered.push_str(&rest[..start]);
        let after_start = &rest[start + 2..];
        let Some(end) = after_start.find("]]") else {
            rendered.push_str(&rest[start..]);
            return rendered;
        };

        let raw = &after_start[..end];
        let (target, label) = raw
            .split_once('|')
            .map(|(target, label)| (target.trim(), label.trim()))
            .unwrap_or_else(|| {
                let target = raw.trim();
                (target, target)
            });

        if target.is_empty() {
            rendered.push_str("[[]]");
        } else if let Some(filename) = entity_link_filename(index, target) {
            rendered.push_str(&format!(
                "[{}]({})",
                escape_markdown_link_label(label),
                filename
            ));
        } else {
            rendered.push_str(&format!("[[{}]]", clean_markdown_text(raw)));
        }

        rest = &after_start[end + 2..];
    }

    rendered.push_str(rest);
    rendered
}

fn entity_link_filename(index: &[KGEntitySynthesisSummary], entity_name: &str) -> Option<String> {
    let target = normalize_name(entity_name);
    index
        .iter()
        .find(|item| normalize_name(&item.entity_name) == target)
        .map(|item| entity_markdown_filename(item.entity_id, &item.entity_name))
}

fn query_graph_edges(
    conn: &Connection,
    node_ids: &[i64],
    node_names: &[String],
    limit: i64,
) -> Result<Vec<KGGraphEdge>, anyhow::Error> {
    if node_ids.is_empty() {
        return Ok(Vec::new());
    }

    let placeholders = vec!["?"; node_ids.len()].join(", ");
    let sql = format!(
        "
        SELECT src.canonical_name, tgt.canonical_name, rel.relationship_type, rel.weight
        FROM kg_relationships rel
        JOIN kg_entities src ON src.id = rel.source_entity_id
        JOIN kg_entities tgt ON tgt.id = rel.target_entity_id
        WHERE rel.source_entity_id IN ({placeholders})
          AND rel.target_entity_id IN ({placeholders})
        ORDER BY rel.weight DESC, src.canonical_name ASC, tgt.canonical_name ASC
        LIMIT ?
        "
    );

    let mut params = node_ids
        .iter()
        .copied()
        .map(Value::Integer)
        .collect::<Vec<_>>();
    params.extend(node_ids.iter().copied().map(Value::Integer));
    params.push(Value::Integer(limit));

    let valid_names = node_names.iter().cloned().collect::<BTreeSet<_>>();
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(params.iter()), |row| {
        let source: String = row.get(0)?;
        let target: String = row.get(1)?;
        Ok((
            source,
            target,
            row.get::<_, String>(2)?,
            row.get::<_, Option<f64>>(3)?.unwrap_or(1.0),
        ))
    })?;

    let mut edges = Vec::new();
    for row in rows {
        let (source, target, relationship_type, weight) = row?;
        if valid_names.contains(&source) && valid_names.contains(&target) {
            edges.push(KGGraphEdge {
                source,
                target,
                properties: json_object([
                    ("relationship_type", JsonValue::String(relationship_type)),
                    ("weight", json!(weight)),
                ]),
            });
        }
    }

    Ok(edges)
}

/// Passive relationship phrasings and their active counterparts. A passive
/// match flips the edge direction so "B developed by A" and "A develops B"
/// land on the same row.
const PASSIVE_RELATIONSHIP_FORMS: [(&str, &str); 14] = [
    ("developed by", "develops"),
    ("used by", "uses"),
    ("caused by", "causes"),
    ("influenced by", "influences"),
    ("regulated by", "regulates"),
    ("proposed by", "proposes"),
    ("applied by", "applies"),
    ("created by", "creates"),
    ("introduced by", "introduces"),
    ("evaluated by", "evaluates"),
    ("supported by", "supports"),
    ("funded by", "funds"),
    ("studied by", "studies"),
    ("addressed by", "addresses"),
];

/// Canonical form of a relationship edge: trimmed, lowercased,
/// whitespace-collapsed, with passive voice rewritten as active (which swaps
/// source and target). LLM extraction is not consistent about voice, and
/// without this the same fact accumulates as two separate edges.
pub(crate) fn canonicalize_relationship(
    relationship_type: &str,
    source_id: i64,
    target_id: i64,
) -> (String, i64, i64) {
    let normalized = relationship_type
        .trim()
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");

    if let Some((_, active)) = PASSIVE_RELATIONSHIP_FORMS
        .iter()
        .find(|(passive, _)| *passive == normalized)
    {
        return ((*active).to_string(), target_id, source_id);
    }

    (normalized, source_id, target_id)
}

/// Minimum trimmed length for a usable entity description; shorter ones are
/// noise that pollutes synthesis context.
const MIN_ENTITY_DESCRIPTION_CHARS: usize = 15;

fn is_reasonable_entity_name(name: &str) -> bool {
    let trimmed = name.trim();
    let char_count = trimmed.chars().count();
    (2..=120).contains(&char_count) && trimmed.chars().any(char::is_alphabetic)
}

/// Rejects an LLM synthesis that came back structurally valid but empty or
/// truncated — overwriting a previous good synthesis with it would lose data.
fn validate_synthesis_output(output: &SynthesisGenerationOutput) -> Result<(), AppError> {
    if output.summary.trim().is_empty() {
        return Err(AppError::Internal(
            "entity_synthesis returned an empty summary".to_string(),
        ));
    }
    if output.synthesis.trim().chars().count() < 80 {
        return Err(AppError::Internal(
            "entity_synthesis returned a missing or truncated synthesis".to_string(),
        ));
    }
    Ok(())
}

fn validate_extraction(extraction: &mut ChunkExtraction) {
    extraction.entities.retain(|entity| {
        is_reasonable_entity_name(&entity.name)
            && !normalize_entity_type(&entity.entity_type).is_empty()
            && entity.description.trim().chars().count() >= MIN_ENTITY_DESCRIPTION_CHARS
            && !is_metadata_entity_name(&entity.name)
    });

    let entity_names = extraction
        .entities
        .iter()
        .map(|entity| normalize_name(&entity.name))
        .collect::<BTreeSet<_>>();

    extraction.relationships.retain(|relationship| {
        !relationship.relationship.trim().is_empty()
            && entity_names.contains(&normalize_name(&relationship.source))
            && entity_names.contains(&normalize_name(&relationship.target))
    });
}

fn is_metadata_entity_name(value: &str) -> bool {
    let normalized = normalize_name(value);
    if normalized.is_empty() {
        return true;
    }
    if WIKI_EXCLUDED_ENTITY_NAMES.contains(&normalized.as_str()) {
        return true;
    }
    if is_four_digit_year(&normalized) {
        return true;
    }
    if is_literal_publication_date(&normalized) {
        return true;
    }
    if normalized.starts_with("pmc")
        && normalized
            .strip_prefix("pmc")
            .is_some_and(|rest| rest.chars().all(|character| character.is_ascii_digit()))
    {
        return true;
    }
    if normalized.starts_with("pubmed")
        && normalized
            .strip_prefix("pubmed")
            .is_some_and(|rest| rest.chars().all(|character| character.is_ascii_digit()))
    {
        return true;
    }
    if normalized.starts_with("pmid")
        && normalized
            .strip_prefix("pmid")
            .is_some_and(|rest| rest.chars().all(|character| character.is_ascii_digit()))
    {
        return true;
    }
    if normalized.starts_with("doi:")
        || normalized.starts_with("license:")
        || normalized.starts_with("volume ")
        || normalized.starts_with("issue ")
    {
        return true;
    }
    false
}

fn is_literal_publication_date(value: &str) -> bool {
    const MONTHS: [&str; 12] = [
        "january",
        "february",
        "march",
        "april",
        "may",
        "june",
        "july",
        "august",
        "september",
        "october",
        "november",
        "december",
    ];

    if value.len() > 40 || !MONTHS.iter().any(|month| value.contains(month)) {
        return false;
    }

    let mut has_year = false;
    let mut has_day = false;
    for token in value.split(|character: char| !character.is_ascii_alphanumeric()) {
        if is_four_digit_year(token) {
            has_year = true;
        } else if matches!(token.parse::<u8>(), Ok(day) if (1..=31).contains(&day)) {
            has_day = true;
        }
    }

    has_year && has_day
}

fn is_four_digit_year(value: &str) -> bool {
    value.len() == 4
        && value.chars().all(|character| character.is_ascii_digit())
        && matches!(value.parse::<i32>(), Ok(year) if (1900..=2100).contains(&year))
}

fn prepare_kg_text(title: Option<&str>, content: Option<&str>) -> String {
    let mut text = content
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| title.map(str::trim).unwrap_or_default())
        .to_string();

    if let Some(title) = title.map(str::trim).filter(|value| !value.is_empty()) {
        if text != title {
            text = format!("Title: {title}\n\n{text}");
        }
    }

    bound_kg_text(&text)
}

/// Bounds the extraction text to [`KG_TEXT_MAX_CHARS`]. Overlong documents are
/// sampled head + tail rather than head-only, so results, discussion, and
/// conclusions contribute entities too.
fn bound_kg_text(text: &str) -> String {
    let char_count = text.chars().count();
    if char_count <= KG_TEXT_MAX_CHARS {
        return text.to_string();
    }

    let head_chars = KG_TEXT_MAX_CHARS - KG_TEXT_TAIL_CHARS;
    let head: String = text.chars().take(head_chars).collect();
    let tail: String = text
        .chars()
        .skip(char_count - KG_TEXT_TAIL_CHARS)
        .collect();
    format!("{head}\n\n[... middle of document omitted ...]\n\n{tail}")
}

fn chunk_text_for_kg(text: &str) -> Vec<String> {
    let words = text.split_whitespace().collect::<Vec<_>>();
    if words.is_empty() {
        return Vec::new();
    }

    let mut chunks = Vec::new();
    let mut start = 0usize;
    while start < words.len() && chunks.len() < KG_MAX_CHUNKS {
        let end = (start + KG_CHUNK_WORDS).min(words.len());
        chunks.push(words[start..end].join(" "));
        if end == words.len() {
            break;
        }
        let step = KG_CHUNK_WORDS.saturating_sub(KG_CHUNK_OVERLAP_WORDS).max(1);
        start += step;
    }
    chunks
}

fn parse_embedding_row(value: ValueRef<'_>) -> rusqlite::Result<Vec<f32>> {
    match value {
        ValueRef::Text(raw) => serde_json::from_slice::<Vec<f32>>(raw).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                raw.len(),
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        }),
        ValueRef::Blob(raw) => {
            if raw.len() % 4 == 0 {
                let mut vector = Vec::with_capacity(raw.len() / 4);
                for chunk in raw.chunks_exact(4) {
                    vector.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
                }
                Ok(vector)
            } else {
                serde_json::from_slice::<Vec<f32>>(raw).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        raw.len(),
                        rusqlite::types::Type::Blob,
                        Box::new(error),
                    )
                })
            }
        }
        other => Err(rusqlite::Error::FromSqlConversionFailure(
            0,
            other.data_type(),
            "Unsupported embedding value type".into(),
        )),
    }
}

fn vector_norm(vector: &[f32]) -> f32 {
    vector.iter().map(|value| value * value).sum::<f32>().sqrt()
}

fn cosine_similarity(left: &[f32], left_norm: f32, right: &[f32], right_norm: f32) -> f32 {
    if left.is_empty() || right.is_empty() || left.len() != right.len() {
        return 0.0;
    }
    if left_norm <= f32::EPSILON || right_norm <= f32::EPSILON {
        return 0.0;
    }
    let dot = left
        .iter()
        .zip(right.iter())
        .map(|(left, right)| left * right)
        .sum::<f32>();
    dot / (left_norm * right_norm)
}

fn normalize_name(value: &str) -> String {
    value.trim().to_lowercase()
}

fn normalize_entity_type(value: &str) -> String {
    value.trim().to_uppercase()
}

fn parse_entity_types(raw: Option<String>) -> Vec<String> {
    raw.unwrap_or_default()
        .split(',')
        .map(|item| item.trim().to_uppercase())
        .filter(|item| !item.is_empty())
        .collect()
}

fn parse_string_list(raw: Option<String>) -> Vec<String> {
    raw.and_then(|value| serde_json::from_str::<Vec<String>>(&value).ok())
        .unwrap_or_default()
}

fn compute_similarity(query: &str, canonical_name: &str, aliases: &[String]) -> Option<f64> {
    let query = query.to_lowercase();
    let name = canonical_name.to_lowercase();
    if name == query {
        return Some(1.0);
    }
    if name.starts_with(&query) {
        return Some(0.95);
    }
    if name.contains(&query) {
        return Some(0.85);
    }
    if aliases.iter().any(|alias| alias.to_lowercase() == query) {
        return Some(0.9);
    }
    if aliases
        .iter()
        .any(|alias| alias.to_lowercase().contains(&query))
    {
        return Some(0.8);
    }
    None
}

fn truncate_text(value: String, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value;
    }
    value.chars().take(max_chars).collect::<String>() + "..."
}

fn json_object<const N: usize>(pairs: [(&str, JsonValue); N]) -> Map<String, JsonValue> {
    let mut object = Map::new();
    for (key, value) in pairs {
        object.insert(key.to_string(), value);
    }
    object
}

#[cfg(test)]
mod tests {
    use super::{
        KG_TEXT_MAX_CHARS, KG_TEXT_TAIL_CHARS, bound_kg_text, canonicalize_relationship,
        is_metadata_entity_name, validate_extraction, validate_synthesis_output,
    };
    use crate::models::knowledge_graph::{
        ChunkExtraction, ExtractedEntity, ExtractedRelationship, SynthesisGenerationOutput,
    };

    #[test]
    fn kg_text_bound_samples_head_and_tail() {
        // Short input passes through untouched.
        assert_eq!(bound_kg_text("short"), "short");

        let text = format!(
            "{}{}",
            "A".repeat(KG_TEXT_MAX_CHARS),
            "Z".repeat(KG_TEXT_TAIL_CHARS)
        );

        let bounded = bound_kg_text(&text);

        assert!(bounded.starts_with("AAA"));
        assert!(bounded.ends_with("ZZZ"), "document tail is preserved");
        assert!(bounded.contains("middle of document omitted"));
        assert!(bounded.chars().count() < text.chars().count());
    }

    #[test]
    fn relationship_canonicalization_normalizes_and_flips_passive_voice() {
        assert_eq!(
            canonicalize_relationship("  Develops  ", 1, 2),
            ("develops".to_string(), 1, 2)
        );
        assert_eq!(
            canonicalize_relationship("Developed   By", 1, 2),
            ("develops".to_string(), 2, 1)
        );
        assert_eq!(
            canonicalize_relationship("used by", 7, 9),
            ("uses".to_string(), 9, 7)
        );
        // Canonicalizing twice is a no-op.
        let (rel, src, tgt) = canonicalize_relationship("developed by", 1, 2);
        assert_eq!(canonicalize_relationship(&rel, src, tgt), (rel.clone(), src, tgt));
    }

    #[test]
    fn synthesis_validation_rejects_empty_or_truncated_output() {
        let valid = SynthesisGenerationOutput {
            summary: "A summary.".to_string(),
            synthesis: "S".repeat(120),
            key_aspects: Vec::new(),
            related_entities: Vec::new(),
        };
        assert!(validate_synthesis_output(&valid).is_ok());

        let empty_summary = SynthesisGenerationOutput {
            summary: "  ".to_string(),
            synthesis: "S".repeat(120),
            key_aspects: Vec::new(),
            related_entities: Vec::new(),
        };
        assert!(validate_synthesis_output(&empty_summary).is_err());

        let short_synthesis = SynthesisGenerationOutput {
            summary: "A summary.".to_string(),
            synthesis: "too short".to_string(),
            key_aspects: Vec::new(),
            related_entities: Vec::new(),
        };
        assert!(validate_synthesis_output(&short_synthesis).is_err());
    }

    #[test]
    fn extraction_validation_drops_junk_entities_and_orphan_relationships() {
        let entity = |name: &str, description: &str| ExtractedEntity {
            name: name.to_string(),
            entity_type: "CONCEPT".to_string(),
            description: description.to_string(),
        };
        let mut extraction = ChunkExtraction {
            entities: vec![
                entity("Machine Learning", "A field of AI focused on learning from data."),
                entity("X", "A single-character name is too short to keep."),
                entity("Federated Learning", "short"),
                entity("12345", "All-digit names carry no entity meaning here."),
            ],
            relationships: vec![
                ExtractedRelationship {
                    source: "Machine Learning".to_string(),
                    target: "Federated Learning".to_string(),
                    relationship: "includes".to_string(),
                    description: String::new(),
                },
            ],
        };

        validate_extraction(&mut extraction);

        assert_eq!(extraction.entities.len(), 1);
        assert_eq!(extraction.entities[0].name, "Machine Learning");
        // The relationship's target was dropped, so the edge goes too.
        assert!(extraction.relationships.is_empty());
    }

    #[test]
    fn metadata_filter_removes_publication_artifacts() {
        assert!(is_metadata_entity_name("2026"));
        assert!(is_metadata_entity_name("PMC12900275"));
        assert!(is_metadata_entity_name("PMID42123456"));
        assert!(is_metadata_entity_name("Creative Commons License"));
        assert!(is_metadata_entity_name("Volume 14"));
        assert!(is_metadata_entity_name("Research Article"));
        assert!(is_metadata_entity_name("Journal Article"));
        assert!(is_metadata_entity_name("JMIR Publications Inc."));
        assert!(is_metadata_entity_name("06 February 2026"));
    }

    #[test]
    fn metadata_filter_keeps_domain_concepts() {
        assert!(!is_metadata_entity_name("Artificial Intelligence"));
        assert!(!is_metadata_entity_name("large language models"));
        assert!(!is_metadata_entity_name("LLM"));
        assert!(!is_metadata_entity_name("Machine Learning"));
        assert!(!is_metadata_entity_name("ChatGPT-4o"));
    }
}
