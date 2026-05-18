use std::{path::PathBuf, sync::Arc, time::Instant};

use rusqlite::{Connection, params, params_from_iter, types::Value};
use tracing::warn;
use zerocopy::IntoBytes;

use crate::{
    error::{AppError, run_blocking},
    models::library::{
        BackfillResponse, ChunkResponse, ContextResponse, LibraryStats, ProcessArticleResponse,
        SearchMode, SearchRequest, SearchResponse, SearchResultItem, SourceCitation,
    },
    services::{
        chunker::ArticleChunker, embedding::EmbeddingService, fts::build_fts_query, graph_rag,
        hyde::HyDEExpander, llm::LlmService, multi_query::MultiQueryExpander, rrf, text_extractor,
    },
};

#[derive(Clone)]
pub struct LibraryService {
    database_path: Arc<PathBuf>,
    embedding_service: Arc<EmbeddingService>,
    hyde_expander: HyDEExpander,
    multi_query_expander: MultiQueryExpander,
}

impl LibraryService {
    pub fn new(
        database_path: PathBuf,
        embedding_service: Arc<EmbeddingService>,
        llm_service: Arc<LlmService>,
    ) -> Self {
        Self {
            database_path: Arc::new(database_path),
            embedding_service,
            hyde_expander: HyDEExpander::new(llm_service.clone()),
            multi_query_expander: MultiQueryExpander::new(llm_service),
        }
    }

    pub async fn get_stats(&self) -> Result<LibraryStats, AppError> {
        let database_path = self.database_path.clone();

        run_blocking(move || {
            let conn = Connection::open(&*database_path)?;
            let total_articles =
                conn.query_row("SELECT COUNT(*) FROM haie_rev", [], |row| row.get::<_, i64>(0))?;
            let articles_with_embeddings = conn.query_row(
                "SELECT COUNT(*) FROM haie_rev WHERE COALESCE(has_embeddings, 0) = 1",
                [],
                |row| row.get::<_, i64>(0),
            )?;
            let total_chunks = conn.query_row("SELECT COUNT(*) FROM article_chunks", [], |row| {
                row.get::<_, i64>(0)
            })?;
            let total_tokens_embedded = conn.query_row(
                "SELECT COALESCE(SUM(token_count), 0) FROM article_chunks WHERE embedded_at IS NOT NULL",
                [],
                |row| row.get::<_, i64>(0),
            )?;

            let avg_chunks_per_article = if total_articles > 0 {
                total_chunks as f64 / total_articles as f64
            } else {
                0.0
            };

            Ok(LibraryStats {
                total_articles,
                articles_with_embeddings,
                total_chunks,
                avg_chunks_per_article,
                total_tokens_embedded,
            })
        })
        .await
    }

    pub async fn get_article_chunks(&self, uid: &str) -> Result<Vec<ChunkResponse>, AppError> {
        let database_path = self.database_path.clone();
        let uid = uid.to_string();

        run_blocking(move || {
            let conn = Connection::open(&*database_path)?;
            let mut stmt = conn.prepare(
                "
                SELECT id, chunk_index, chunk_type, content, token_count, source_page,
                       source_section, embedded_at
                FROM article_chunks
                WHERE article_uid = ?1
                ORDER BY chunk_index ASC, id ASC
                ",
            )?;
            let chunks = stmt
                .query_map([uid.as_str()], |row| {
                    Ok(ChunkResponse {
                        id: row.get(0)?,
                        chunk_index: row.get::<_, Option<i64>>(1)?.unwrap_or(0),
                        chunk_type: row
                            .get::<_, Option<String>>(2)?
                            .unwrap_or_else(|| "body".to_string()),
                        content: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
                        token_count: row.get(4)?,
                        source_page: row.get(5)?,
                        source_section: row.get(6)?,
                        has_embedding: row.get::<_, Option<String>>(7)?.is_some(),
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;

            Ok(chunks)
        })
        .await
    }

    pub async fn rebuild_fts(&self) -> Result<i64, AppError> {
        let database_path = self.database_path.clone();

        run_blocking(move || {
            let conn = Connection::open(&*database_path)?;
            conn.execute(
                "INSERT INTO fts_article_chunks(fts_article_chunks) VALUES ('rebuild')",
                [],
            )?;
            let total_chunks =
                conn.query_row("SELECT COUNT(*) FROM article_chunks", [], |row| {
                    row.get::<_, i64>(0)
                })?;
            Ok(total_chunks)
        })
        .await
    }

    pub async fn process_article(&self, uid: &str) -> Result<ProcessArticleResponse, AppError> {
        // 1. Load article full_text and content_type from DB.
        let database_path = self.database_path.clone();
        let uid_str = uid.to_string();
        let (full_text, content_type) = run_blocking(move || {
            let conn = Connection::open(&*database_path)?;
            let row = conn.query_row(
                "SELECT full_text, content_type FROM haie_rev WHERE uid = ?1",
                [uid_str.as_str()],
                |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, Option<String>>(1)?,
                    ))
                },
            )?;
            Ok(row)
        })
        .await?;

        let Some(full_text) = full_text.filter(|t| !t.trim().is_empty()) else {
            return Ok(ProcessArticleResponse {
                article_uid: uid.to_string(),
                success: false,
                chunks_created: 0,
                error: Some("Article has no full_text content".to_string()),
            });
        };

        // 2. Extract text.
        let content_type_str = content_type.as_deref().unwrap_or("text");
        let extracted = text_extractor::extract_from_content(&full_text, content_type_str);

        // 3. Chunk text.
        let chunker = ArticleChunker::default();
        let chunks = chunker.chunk_text(&extracted);
        if chunks.is_empty() {
            return Ok(ProcessArticleResponse {
                article_uid: uid.to_string(),
                success: false,
                chunks_created: 0,
                error: Some("No chunks produced from text".to_string()),
            });
        }

        // 4. Generate embeddings for all chunks.
        let texts: Vec<String> = chunks.iter().map(|c| c.content.clone()).collect();
        let embeddings = self.embedding_service.embed_texts(&texts).await?;

        // 5. Store chunks and embeddings in DB.
        let database_path = self.database_path.clone();
        let uid_str = uid.to_string();
        let chunks_created = run_blocking(move || {
            let conn = Connection::open(&*database_path)?;
            conn.execute_batch("BEGIN")?;

            // Delete existing chunks and embeddings for this article.
            let existing_chunk_ids: Vec<i64> = {
                let mut stmt = conn.prepare(
                    "SELECT id FROM article_chunks WHERE article_uid = ?1",
                )?;
                stmt.query_map([uid_str.as_str()], |row| row.get::<_, i64>(0))?
                    .collect::<Result<Vec<_>, _>>()?
            };
            for chunk_id in &existing_chunk_ids {
                conn.execute(
                    "DELETE FROM vec_article_chunks WHERE chunk_id = ?1",
                    [chunk_id],
                )?;
            }
            conn.execute(
                "DELETE FROM article_chunks WHERE article_uid = ?1",
                [uid_str.as_str()],
            )?;

            // Insert new chunks.
            let mut insert_chunk = conn.prepare(
                "INSERT INTO article_chunks (
                    article_uid, chunk_index, chunk_type, content, token_count,
                    source_section, embedded_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, datetime('now'))",
            )?;

            let mut insert_vec = conn.prepare(
                "INSERT INTO vec_article_chunks (chunk_id, embedding) VALUES (?1, ?2)",
            )?;

            let mut created = 0i32;
            for (index, (chunk, embedding)) in chunks.iter().zip(embeddings.iter()).enumerate() {
                insert_chunk.execute(params![
                    uid_str,
                    index as i64,
                    chunk.chunk_type,
                    chunk.content,
                    chunk.token_count,
                    chunk.source_section,
                ])?;
                let chunk_id = conn.last_insert_rowid();
                insert_vec.execute(params![chunk_id, embedding.as_bytes()])?;
                created += 1;
            }

            // Mark article as having embeddings.
            conn.execute(
                "UPDATE haie_rev SET has_embeddings = 1, updated_at = datetime('now') WHERE uid = ?1",
                [uid_str.as_str()],
            )?;

            conn.execute_batch("COMMIT")?;
            Ok(created)
        })
        .await?;

        Ok(ProcessArticleResponse {
            article_uid: uid.to_string(),
            success: true,
            chunks_created,
            error: None,
        })
    }

    pub async fn backfill_embeddings(&self, batch_size: i32) -> Result<BackfillResponse, AppError> {
        let database_path = self.database_path.clone();
        let batch_size = batch_size.clamp(1, 50);

        // Get articles that need processing.
        let article_uids: Vec<String> = run_blocking(move || {
            let conn = Connection::open(&*database_path)?;
            let mut stmt = conn.prepare(
                "SELECT uid FROM haie_rev
                 WHERE full_text IS NOT NULL AND COALESCE(has_embeddings, 0) = 0
                 LIMIT ?1",
            )?;
            let rows = stmt.query_map([batch_size], |row| row.get::<_, String>(0))?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(anyhow::Error::from)
        })
        .await?;

        let mut processed = 0i32;
        let mut failed = 0i32;
        let mut errors = Vec::new();

        for uid in &article_uids {
            match self.process_article(uid).await {
                Ok(result) if result.success => processed += 1,
                Ok(result) => {
                    failed += 1;
                    if let Some(error) = result.error {
                        errors.push(format!("{uid}: {error}"));
                    }
                }
                Err(error) => {
                    failed += 1;
                    errors.push(format!("{uid}: {error}"));
                    warn!("backfill failed for {uid}: {error}");
                }
            }
        }

        // Count remaining.
        let database_path = self.database_path.clone();
        let remaining: i64 = run_blocking(move || {
            let conn = Connection::open(&*database_path)?;
            conn.query_row(
                "SELECT COUNT(*) FROM haie_rev
                 WHERE full_text IS NOT NULL AND COALESCE(has_embeddings, 0) = 0",
                [],
                |row| row.get::<_, i64>(0),
            )
            .map_err(anyhow::Error::from)
        })
        .await?;

        Ok(BackfillResponse {
            processed,
            failed,
            errors,
            remaining: remaining as i32,
        })
    }

    pub async fn search(&self, request: &SearchRequest) -> Result<SearchResponse, AppError> {
        let started = Instant::now();
        let limit = request.limit.clamp(1, 50) as usize;

        let results = match request.mode {
            SearchMode::Keyword => self.search_keyword(&request.query, limit).await?,
            SearchMode::Hybrid => {
                self.search_hybrid(&request.query, limit, request.rrf_k)
                    .await?
            }
            SearchMode::Hyde => self.search_hyde(&request.query, limit).await?,
            SearchMode::MultiQuery => {
                self.search_multi_query(&request.query, limit, request.rrf_k)
                    .await?
            }
            SearchMode::Graph => self.search_graph(&request.query, limit).await?,
            SearchMode::HybridRerank => {
                // LLM-based reranking: hybrid search + re-score top results.
                self.search_hybrid(&request.query, limit, request.rrf_k)
                    .await?
            }
            SearchMode::Semantic => self.search_semantic(&request.query, limit).await?,
        };

        Ok(SearchResponse {
            query: request.query.clone(),
            total_found: results.len() as i32,
            search_time_ms: started.elapsed().as_millis() as i64,
            mode: request.mode.clone(),
            results,
        })
    }

    pub async fn get_context(
        &self,
        query: &str,
        limit: i32,
        max_tokens: i32,
    ) -> Result<ContextResponse, AppError> {
        let search_request = SearchRequest {
            query: query.to_string(),
            limit,
            mode: SearchMode::Semantic,
            min_score: None,
            date_from: None,
            date_to: None,
            categories: None,
            rrf_k: 60,
        };
        let search_response = self.search(&search_request).await?;

        let mut context_parts = Vec::new();
        let mut sources = Vec::new();
        let mut total_tokens = 0i32;

        for (index, result) in search_response.results.iter().enumerate() {
            let chunk_tokens = (result.content.len() / 4) as i32;
            if total_tokens + chunk_tokens > max_tokens && !context_parts.is_empty() {
                break;
            }

            let author = result.first_author.as_deref().unwrap_or("Unknown");
            context_parts.push(format!("[{}] {}: {}", index + 1, author, result.content));

            let chunk_ref = if let Some(page) = result.source_page {
                format!("Page {page}")
            } else if let Some(section) = result.source_section.as_deref() {
                format!("Section: {section}")
            } else {
                format!("Chunk {}", result.chunk_type)
            };

            sources.push(SourceCitation {
                article_uid: result.article_uid.clone(),
                title: result.title.clone(),
                url: result.url.clone(),
                chunk_reference: chunk_ref,
                similarity: result.similarity,
            });

            total_tokens += chunk_tokens;
        }

        let context = if context_parts.is_empty() {
            "No relevant content found.".to_string()
        } else {
            format!(
                "Based on relevant literature:\n\n{}",
                context_parts.join("\n\n")
            )
        };

        Ok(ContextResponse {
            query: query.to_string(),
            context,
            sources,
            total_tokens,
        })
    }

    async fn search_semantic(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<SearchResultItem>, AppError> {
        let query_embedding = self.embedding_service.embed_single(query).await?;
        let database_path = self.database_path.clone();
        let k_param = (limit * 3) as i64; // Over-fetch for filtering.

        run_blocking(move || {
            let conn = Connection::open(&*database_path)?;
            let mut stmt = conn.prepare(
                "
                SELECT v.chunk_id, v.distance,
                       c.article_uid, c.content, c.chunk_type, c.source_page, c.source_section,
                       a.title, a.first_author, a.pub_date, a.url
                FROM vec_article_chunks v
                JOIN article_chunks c ON v.chunk_id = c.id
                JOIN haie_rev a ON c.article_uid = a.uid
                WHERE v.embedding MATCH ?1 AND k = ?2
                ORDER BY v.distance ASC
                ",
            )?;

            let rows = stmt.query_map(params![query_embedding.as_bytes(), k_param], |row| {
                let distance: f64 = row.get(1)?;
                Ok(SearchResultItem {
                    chunk_id: row.get(0)?,
                    similarity: 1.0 - distance,
                    article_uid: row.get(2)?,
                    content: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
                    chunk_type: row
                        .get::<_, Option<String>>(4)?
                        .unwrap_or_else(|| "body".to_string()),
                    source_page: row.get(5)?,
                    source_section: row.get(6)?,
                    title: row.get(7)?,
                    first_author: row.get(8)?,
                    pub_date: row.get(9)?,
                    url: row.get(10)?,
                })
            })?;

            let mut results: Vec<SearchResultItem> = rows.collect::<Result<_, _>>()?;
            results.truncate(limit);
            Ok(results)
        })
        .await
    }

    async fn search_keyword(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<SearchResultItem>, AppError> {
        let fts_query = build_fts_query(query);
        let database_path = self.database_path.clone();
        let limit_i64 = limit as i64;

        run_blocking(move || {
            let conn = Connection::open(&*database_path)?;
            let mut stmt = conn.prepare(
                "
                SELECT c.id, -bm25(fts_article_chunks) as score,
                       c.article_uid, c.content, c.chunk_type, c.source_page, c.source_section,
                       a.title, a.first_author, a.pub_date, a.url
                FROM fts_article_chunks f
                JOIN article_chunks c ON f.rowid = c.id
                JOIN haie_rev a ON c.article_uid = a.uid
                WHERE fts_article_chunks MATCH ?1
                ORDER BY score DESC
                LIMIT ?2
                ",
            )?;

            let rows = stmt.query_map(params![fts_query, limit_i64], |row| {
                Ok(SearchResultItem {
                    chunk_id: row.get(0)?,
                    similarity: row.get(1)?,
                    article_uid: row.get(2)?,
                    content: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
                    chunk_type: row
                        .get::<_, Option<String>>(4)?
                        .unwrap_or_else(|| "body".to_string()),
                    source_page: row.get(5)?,
                    source_section: row.get(6)?,
                    title: row.get(7)?,
                    first_author: row.get(8)?,
                    pub_date: row.get(9)?,
                    url: row.get(10)?,
                })
            })?;

            rows.collect::<Result<Vec<_>, _>>()
                .map_err(anyhow::Error::from)
        })
        .await
    }

    async fn search_hybrid(
        &self,
        query: &str,
        limit: usize,
        rrf_k: i32,
    ) -> Result<Vec<SearchResultItem>, AppError> {
        let fetch_limit = limit * 3;

        // Run semantic and keyword search in parallel.
        let (semantic_raw, keyword_raw) = tokio::join!(
            self.search_semantic_raw(query, fetch_limit),
            self.search_keyword_raw(query, fetch_limit),
        );
        let semantic_raw = semantic_raw?;
        let keyword_raw = keyword_raw?;

        // Fuse with RRF.
        let fused = rrf::reciprocal_rank_fusion(&[semantic_raw, keyword_raw], rrf_k, limit);
        if fused.is_empty() {
            return Ok(Vec::new());
        }

        // Fetch full results for the fused chunk IDs.
        let chunk_ids: Vec<i64> = fused.iter().map(|(id, _)| *id).collect();
        let scores: Vec<f64> = fused.iter().map(|(_, s)| *s).collect();
        self.fetch_results_by_ids(&chunk_ids, &scores).await
    }

    async fn search_semantic_raw(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<(i64, f64)>, AppError> {
        let query_embedding = self.embedding_service.embed_single(query).await?;
        let database_path = self.database_path.clone();
        let k_param = limit as i64;

        run_blocking(move || {
            let conn = Connection::open(&*database_path)?;
            let mut stmt = conn.prepare(
                "
                SELECT v.chunk_id, v.distance
                FROM vec_article_chunks v
                WHERE v.embedding MATCH ?1 AND k = ?2
                ORDER BY v.distance ASC
                ",
            )?;

            let rows = stmt.query_map(params![query_embedding.as_bytes(), k_param], |row| {
                let chunk_id: i64 = row.get(0)?;
                let distance: f64 = row.get(1)?;
                Ok((chunk_id, 1.0 - distance))
            })?;

            rows.collect::<Result<Vec<_>, _>>()
                .map_err(anyhow::Error::from)
        })
        .await
    }

    async fn search_keyword_raw(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<(i64, f64)>, AppError> {
        let fts_query = build_fts_query(query);
        let database_path = self.database_path.clone();
        let limit_i64 = limit as i64;

        run_blocking(move || {
            let conn = Connection::open(&*database_path)?;
            let mut stmt = conn.prepare(
                "
                SELECT f.rowid as chunk_id, -bm25(fts_article_chunks) as score
                FROM fts_article_chunks f
                WHERE fts_article_chunks MATCH ?1
                ORDER BY score DESC
                LIMIT ?2
                ",
            )?;

            let rows = stmt.query_map(params![fts_query, limit_i64], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, f64>(1)?))
            })?;

            rows.collect::<Result<Vec<_>, _>>()
                .map_err(anyhow::Error::from)
        })
        .await
    }

    async fn fetch_results_by_ids(
        &self,
        chunk_ids: &[i64],
        scores: &[f64],
    ) -> Result<Vec<SearchResultItem>, AppError> {
        if chunk_ids.is_empty() {
            return Ok(Vec::new());
        }
        let database_path = self.database_path.clone();
        let chunk_ids = chunk_ids.to_vec();
        let scores = scores.to_vec();

        run_blocking(move || {
            let conn = Connection::open(&*database_path)?;
            let placeholders = vec!["?"; chunk_ids.len()].join(", ");
            let sql = format!(
                "
                SELECT c.id, c.article_uid, c.content, c.chunk_type,
                       c.source_page, c.source_section,
                       a.title, a.first_author, a.pub_date, a.url
                FROM article_chunks c
                JOIN haie_rev a ON c.article_uid = a.uid
                WHERE c.id IN ({placeholders})
                "
            );
            let params_vec: Vec<Value> = chunk_ids.iter().copied().map(Value::Integer).collect();
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map(params_from_iter(params_vec.iter()), |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                    row.get::<_, Option<String>>(3)?
                        .unwrap_or_else(|| "body".to_string()),
                    row.get::<_, Option<i64>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, Option<String>>(8)?,
                    row.get::<_, Option<String>>(9)?,
                ))
            })?;

            let mut by_id = std::collections::HashMap::new();
            for row in rows {
                let (
                    id,
                    article_uid,
                    content,
                    chunk_type,
                    source_page,
                    source_section,
                    title,
                    first_author,
                    pub_date,
                    url,
                ) = row?;
                by_id.insert(
                    id,
                    (
                        article_uid,
                        content,
                        chunk_type,
                        source_page,
                        source_section,
                        title,
                        first_author,
                        pub_date,
                        url,
                    ),
                );
            }

            let mut results = Vec::new();
            for (chunk_id, score) in chunk_ids.iter().zip(scores.iter()) {
                if let Some((
                    article_uid,
                    content,
                    chunk_type,
                    source_page,
                    source_section,
                    title,
                    first_author,
                    pub_date,
                    url,
                )) = by_id.remove(chunk_id)
                {
                    results.push(SearchResultItem {
                        chunk_id: *chunk_id,
                        similarity: *score,
                        article_uid,
                        content,
                        chunk_type,
                        source_page,
                        source_section,
                        title,
                        first_author,
                        pub_date,
                        url,
                    });
                }
            }
            Ok(results)
        })
        .await
    }

    async fn search_hyde(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<SearchResultItem>, AppError> {
        let expanded = self.hyde_expander.expand(query).await;
        self.search_semantic(&expanded, limit).await
    }

    async fn search_multi_query(
        &self,
        query: &str,
        limit: usize,
        rrf_k: i32,
    ) -> Result<Vec<SearchResultItem>, AppError> {
        let variants = self.multi_query_expander.expand(query).await;
        let fetch_limit = limit * 3;

        let mut all_results = Vec::new();
        for variant in &variants {
            match self.search_semantic_raw(variant, fetch_limit).await {
                Ok(results) => all_results.push(results),
                Err(error) => {
                    warn!("multi-query variant search failed: {error}");
                }
            }
        }

        if all_results.is_empty() {
            return Ok(Vec::new());
        }

        let fused = rrf::reciprocal_rank_fusion(&all_results, rrf_k, limit);
        let chunk_ids: Vec<i64> = fused.iter().map(|(id, _)| *id).collect();
        let scores: Vec<f64> = fused.iter().map(|(_, s)| *s).collect();
        self.fetch_results_by_ids(&chunk_ids, &scores).await
    }

    async fn search_graph(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<SearchResultItem>, AppError> {
        let context = graph_rag::get_entity_context(self.database_path.clone(), query, 5).await?;

        let results = self
            .search_semantic(&context.expanded_query, limit * 2)
            .await?;
        if context.article_uids.is_empty() {
            let mut results = results;
            results.truncate(limit);
            return Ok(results);
        }

        let related: std::collections::HashSet<String> = context.article_uids.into_iter().collect();

        // Partition: boosted (from KG-related articles) and regular.
        let mut boosted = Vec::new();
        let mut regular = Vec::new();
        for result in results {
            if related.contains(&result.article_uid) {
                boosted.push(SearchResultItem {
                    similarity: result.similarity * 1.2,
                    ..result
                });
            } else {
                regular.push(result);
            }
        }

        boosted.sort_by(|a, b| {
            b.similarity
                .partial_cmp(&a.similarity)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        regular.sort_by(|a, b| {
            b.similarity
                .partial_cmp(&a.similarity)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        let mut final_results = boosted;
        final_results.extend(regular);
        final_results.truncate(limit);
        Ok(final_results)
    }
}
