use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result};
use rusqlite::Connection;
use tokio::task;

use crate::config::AppConfig;

/// `busy_timeout=5000` lets concurrent callers retry under WAL instead of
/// failing with `SQLITE_BUSY` — matters more on Windows where file locking
/// is stricter.
pub fn open_connection(path: impl AsRef<Path>) -> rusqlite::Result<Connection> {
    let conn = Connection::open(path)?;
    conn.busy_timeout(Duration::from_millis(5000))?;
    Ok(conn)
}

pub async fn initialize(config: &AppConfig) -> Result<()> {
    initialize_workspace_db(
        config.storage.database_path.clone(),
        config.embedding_dimensions,
    )
    .await
}

/// Initializes a single workspace's database file (full data schema). Each
/// workspace lives in its own file, so isolation is physical.
pub async fn initialize_workspace_db(database_path: PathBuf, embedding_dimensions: u32) -> Result<()> {
    if let Some(parent) = database_path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("failed to create database directory {parent:?}"))?;
    }

    task::spawn_blocking(move || initialize_sync(&database_path, embedding_dimensions))
        .await
        .context("sqlite init task failed")??;

    Ok(())
}

/// Initializes the meta/registry database that lists workspaces and points at
/// each one's data file. Seeds a default "Healthcare AI Ethics" workspace that
/// reuses the existing primary database file.
pub async fn initialize_meta(meta_path: PathBuf, default_db_filename: String) -> Result<()> {
    if let Some(parent) = meta_path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("failed to create meta directory {parent:?}"))?;
    }

    task::spawn_blocking(move || initialize_meta_sync(&meta_path, &default_db_filename))
        .await
        .context("meta init task failed")??;

    Ok(())
}

fn initialize_meta_sync(meta_path: &std::path::Path, default_db_filename: &str) -> Result<()> {
    let conn = Connection::open(meta_path)
        .with_context(|| format!("failed to open meta database at {}", meta_path.display()))?;

    conn.execute_batch(
        r#"
        PRAGMA journal_mode=WAL;
        PRAGMA busy_timeout=5000;

        CREATE TABLE IF NOT EXISTS workspaces (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL,
            slug TEXT NOT NULL UNIQUE,
            db_filename TEXT NOT NULL,
            primary_question TEXT NOT NULL DEFAULT '',
            gap_note TEXT NOT NULL DEFAULT '',
            refined_question TEXT NOT NULL DEFAULT '',
            seed_concepts_json TEXT NOT NULL DEFAULT '[]',
            override_queries_json TEXT NOT NULL DEFAULT '[]',
            topic_descriptor TEXT NOT NULL DEFAULT '',
            lookback_days INTEGER NOT NULL DEFAULT 180,
            is_active INTEGER NOT NULL DEFAULT 0,
            created_at TEXT DEFAULT (datetime('now')),
            updated_at TEXT DEFAULT (datetime('now'))
        );

        CREATE INDEX IF NOT EXISTS idx_workspaces_slug ON workspaces(slug);
        "#,
    )?;

    conn.execute(
        "INSERT INTO workspaces
            (name, slug, db_filename, primary_question, topic_descriptor, seed_concepts_json, is_active)
         SELECT 'Healthcare AI Ethics', 'healthcare-ai-ethics', ?1,
                'What are the ethical implications of AI in healthcare?',
                'the ethics of artificial intelligence in healthcare and clinical medicine',
                '[\"artificial intelligence\",\"clinical decision support\",\"algorithmic fairness\",\"patient privacy\",\"AI governance\"]',
                1
         WHERE NOT EXISTS (SELECT 1 FROM workspaces)",
        [default_db_filename],
    )?;

    Ok(())
}

fn initialize_sync(database_path: &std::path::Path, embedding_dimensions: u32) -> Result<()> {
    let conn = Connection::open(database_path).with_context(|| {
        format!(
            "failed to open sqlite database at {}",
            database_path.display()
        )
    })?;

    conn.execute_batch(
        r#"
        PRAGMA journal_mode=WAL;
        PRAGMA foreign_keys=ON;
        PRAGMA busy_timeout=5000;

        CREATE TABLE IF NOT EXISTS workspaces (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL,
            slug TEXT NOT NULL UNIQUE,
            primary_question TEXT NOT NULL DEFAULT '',
            gap_note TEXT NOT NULL DEFAULT '',
            refined_question TEXT NOT NULL DEFAULT '',
            seed_concepts_json TEXT NOT NULL DEFAULT '[]',
            override_queries_json TEXT NOT NULL DEFAULT '[]',
            topic_descriptor TEXT NOT NULL DEFAULT '',
            lookback_days INTEGER NOT NULL DEFAULT 180,
            is_active INTEGER NOT NULL DEFAULT 0,
            created_at TEXT DEFAULT (datetime('now')),
            updated_at TEXT DEFAULT (datetime('now'))
        );

        CREATE INDEX IF NOT EXISTS idx_workspaces_slug ON workspaces(slug);

        CREATE TABLE IF NOT EXISTS haie_rev (
            uid TEXT PRIMARY KEY,
            url TEXT,
            category TEXT,
            reg_date TEXT,
            title TEXT,
            first_author TEXT,
            authors TEXT,
            pub_date TEXT,
            journal TEXT,
            doi TEXT,
            ai_tech TEXT,
            clinical_domain TEXT,
            ethics_framework TEXT,
            primary_issue TEXT,
            secondary_issues TEXT,
            key_stakeholders TEXT,
            practical_impl TEXT,
            byline_summary TEXT,
            why_it_matters TEXT,
            key_argument TEXT,
            main_findings TEXT,
            normative_claims TEXT,
            limitations TEXT,
            theoretical_strengths TEXT,
            theoretical_weaknesses TEXT,
            empirical_strengths TEXT,
            empirical_weaknesses TEXT,
            scholarly_rigor INTEGER,
            novelty INTEGER,
            relevance_score INTEGER,
            practical_impact INTEGER,
            interdisciplinary INTEGER,
            critical_concerns INTEGER,
            total_score INTEGER,
            priority TEXT,
            full_text TEXT,
            content_type TEXT,
            has_embeddings INTEGER DEFAULT 0,
            has_kg_entities INTEGER DEFAULT 0,
            created_at TEXT DEFAULT (datetime('now')),
            updated_at TEXT DEFAULT (datetime('now'))
        );

        CREATE INDEX IF NOT EXISTS idx_haie_rev_reg_date ON haie_rev(reg_date);
        CREATE INDEX IF NOT EXISTS idx_haie_rev_category ON haie_rev(category);
        CREATE INDEX IF NOT EXISTS idx_haie_rev_priority ON haie_rev(priority);

        CREATE TABLE IF NOT EXISTS prompt_versions (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            prompt_name TEXT NOT NULL,
            version INTEGER NOT NULL,
            content TEXT NOT NULL,
            model TEXT,
            temperature REAL,
            description TEXT,
            changed_by TEXT,
            created_at TEXT DEFAULT (datetime('now')),
            UNIQUE(prompt_name, version)
        );

        CREATE TABLE IF NOT EXISTS prompt_traces (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            prompt_name TEXT NOT NULL,
            prompt_version INTEGER,
            article_uid TEXT,
            model TEXT,
            input_text TEXT,
            output_text TEXT,
            tokens_input INTEGER,
            tokens_output INTEGER,
            tokens_total INTEGER,
            latency_ms INTEGER,
            cost_usd REAL,
            success INTEGER DEFAULT 1,
            error_message TEXT,
            created_at TEXT DEFAULT (datetime('now'))
        );

        CREATE INDEX IF NOT EXISTS idx_prompt_traces_article ON prompt_traces(article_uid);
        CREATE INDEX IF NOT EXISTS idx_prompt_traces_prompt ON prompt_traces(prompt_name);

        CREATE TABLE IF NOT EXISTS job_runs (
            id TEXT PRIMARY KEY,
            source TEXT NOT NULL,
            days_back INTEGER NOT NULL DEFAULT 2,
            status TEXT NOT NULL,
            requested_at TEXT DEFAULT (datetime('now')),
            started_at TEXT,
            finished_at TEXT,
            candidates_found INTEGER DEFAULT 0,
            candidates_screened INTEGER DEFAULT 0,
            candidates_relevant INTEGER DEFAULT 0,
            candidates_fetched INTEGER DEFAULT 0,
            candidates_evaluated INTEGER DEFAULT 0,
            candidates_saved INTEGER DEFAULT 0,
            candidates_embedded INTEGER DEFAULT 0,
            candidates_skipped INTEGER DEFAULT 0,
            errors INTEGER DEFAULT 0,
            current_item TEXT,
            current_step TEXT,
            error_message TEXT
        );

        CREATE INDEX IF NOT EXISTS idx_job_runs_requested_at ON job_runs(requested_at);
        CREATE INDEX IF NOT EXISTS idx_job_runs_status ON job_runs(status);

        CREATE TABLE IF NOT EXISTS job_events (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            run_id TEXT NOT NULL REFERENCES job_runs(id) ON DELETE CASCADE,
            event_type TEXT NOT NULL,
            payload_json TEXT,
            created_at TEXT DEFAULT (datetime('now'))
        );

        CREATE INDEX IF NOT EXISTS idx_job_events_run_id ON job_events(run_id);

        CREATE TABLE IF NOT EXISTS kg_entities (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            canonical_name TEXT NOT NULL UNIQUE,
            entity_type TEXT NOT NULL,
            description TEXT,
            aliases_json TEXT DEFAULT '[]',
            mention_count INTEGER DEFAULT 1,
            created_at TEXT DEFAULT (datetime('now')),
            updated_at TEXT DEFAULT (datetime('now'))
        );

        CREATE INDEX IF NOT EXISTS idx_kg_entities_name ON kg_entities(canonical_name);
        CREATE INDEX IF NOT EXISTS idx_kg_entities_type ON kg_entities(entity_type);
        CREATE INDEX IF NOT EXISTS idx_kg_entities_name_lower ON kg_entities(LOWER(canonical_name));

        CREATE TABLE IF NOT EXISTS kg_relationships (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            source_entity_id INTEGER NOT NULL REFERENCES kg_entities(id) ON DELETE CASCADE,
            target_entity_id INTEGER NOT NULL REFERENCES kg_entities(id) ON DELETE CASCADE,
            relationship_type TEXT NOT NULL,
            description TEXT,
            weight REAL DEFAULT 1.0,
            source_articles_json TEXT DEFAULT '[]',
            created_at TEXT DEFAULT (datetime('now')),
            updated_at TEXT DEFAULT (datetime('now')),
            UNIQUE(source_entity_id, target_entity_id, relationship_type)
        );

        CREATE INDEX IF NOT EXISTS idx_kg_relationships_source ON kg_relationships(source_entity_id);
        CREATE INDEX IF NOT EXISTS idx_kg_relationships_target ON kg_relationships(target_entity_id);
        CREATE INDEX IF NOT EXISTS idx_kg_relationships_type ON kg_relationships(relationship_type);

        CREATE TABLE IF NOT EXISTS kg_article_entities (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            article_uid TEXT NOT NULL REFERENCES haie_rev(uid) ON DELETE CASCADE,
            entity_id INTEGER NOT NULL REFERENCES kg_entities(id) ON DELETE CASCADE,
            mention_text TEXT,
            context TEXT,
            chunk_index INTEGER DEFAULT 0,
            created_at TEXT DEFAULT (datetime('now')),
            UNIQUE(article_uid, entity_id, chunk_index)
        );

        CREATE INDEX IF NOT EXISTS idx_kg_article_entities_uid ON kg_article_entities(article_uid);
        CREATE INDEX IF NOT EXISTS idx_kg_article_entities_entity ON kg_article_entities(entity_id);

        CREATE TABLE IF NOT EXISTS kg_resolution_cache (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            query_name TEXT NOT NULL,
            query_type TEXT NOT NULL,
            candidate_id INTEGER NOT NULL REFERENCES kg_entities(id) ON DELETE CASCADE,
            is_match INTEGER NOT NULL,
            matched_entity_id INTEGER REFERENCES kg_entities(id) ON DELETE SET NULL,
            confidence REAL,
            created_at TEXT DEFAULT (datetime('now')),
            UNIQUE(query_name, query_type, candidate_id)
        );

        CREATE INDEX IF NOT EXISTS idx_kg_resolution_cache_query
            ON kg_resolution_cache(query_name, query_type);
        CREATE INDEX IF NOT EXISTS idx_kg_resolution_cache_candidate
            ON kg_resolution_cache(candidate_id);

        CREATE TABLE IF NOT EXISTS article_chunks (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            article_uid TEXT NOT NULL REFERENCES haie_rev(uid) ON DELETE CASCADE,
            chunk_index INTEGER NOT NULL DEFAULT 0,
            chunk_type TEXT NOT NULL DEFAULT 'body',
            content TEXT NOT NULL,
            token_count INTEGER,
            source_page INTEGER,
            source_section TEXT,
            embedded_at TEXT,
            created_at TEXT DEFAULT (datetime('now'))
        );

        CREATE INDEX IF NOT EXISTS idx_article_chunks_uid ON article_chunks(article_uid);
        CREATE INDEX IF NOT EXISTS idx_article_chunks_uid_index ON article_chunks(article_uid, chunk_index);

        CREATE TABLE IF NOT EXISTS kg_entity_syntheses (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            entity_id INTEGER NOT NULL UNIQUE REFERENCES kg_entities(id) ON DELETE CASCADE,
            synthesis TEXT NOT NULL DEFAULT '',
            summary TEXT NOT NULL DEFAULT '',
            key_aspects_json TEXT DEFAULT '[]',
            related_entities_json TEXT DEFAULT '[]',
            source_article_count INTEGER DEFAULT 0,
            compiled_at TEXT,
            stale INTEGER DEFAULT 1,
            version INTEGER DEFAULT 0,
            created_at TEXT DEFAULT (datetime('now')),
            updated_at TEXT DEFAULT (datetime('now'))
        );

        CREATE INDEX IF NOT EXISTS idx_kg_entity_syntheses_stale ON kg_entity_syntheses(stale);

        CREATE TABLE IF NOT EXISTS kg_gap_findings (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            workspace_id INTEGER NOT NULL,
            entities_reviewed INTEGER DEFAULT 0,
            issues_json TEXT NOT NULL DEFAULT '[]',
            refined_question TEXT NOT NULL DEFAULT '',
            created_at TEXT DEFAULT (datetime('now'))
        );

        CREATE INDEX IF NOT EXISTS idx_kg_gap_findings_ws ON kg_gap_findings(workspace_id);
        "#,
    )?;

    // vec0 virtual tables must run outside execute_batch — virtual table DDL
    // can conflict with batched statements. Drop and recreate on dim change.
    let dim = embedding_dimensions;
    migrate_vec_table(
        &conn,
        "vec_article_chunks",
        &format!("chunk_id INTEGER PRIMARY KEY, embedding float[{dim}] distance_metric=cosine"),
    )?;
    migrate_vec_table(
        &conn,
        "vec_kg_entities",
        &format!("entity_id INTEGER PRIMARY KEY, embedding float[{dim}] distance_metric=cosine"),
    )?;

    // FTS5 virtual tables for BM25 keyword search
    create_fts_table(&conn)?;
    create_synthesis_fts_table(&conn)?;

    ensure_column(&conn, "kg_relationships", "evidence_summary", "TEXT")?;
    ensure_column(&conn, "prompt_versions", "model", "TEXT")?;
    ensure_column(&conn, "prompt_versions", "temperature", "REAL")?;
    ensure_column(&conn, "prompt_versions", "description", "TEXT")?;
    ensure_column(&conn, "prompt_versions", "changed_by", "TEXT")?;
    ensure_column(&conn, "haie_rev", "doi", "TEXT")?;
    ensure_column(&conn, "haie_rev", "full_text", "TEXT")?;
    ensure_column(&conn, "haie_rev", "content_type", "TEXT")?;
    ensure_column(&conn, "haie_rev", "has_embeddings", "INTEGER DEFAULT 0")?;
    ensure_column(&conn, "haie_rev", "has_kg_entities", "INTEGER DEFAULT 0")?;

    // Multi-workspace: nullable workspace_id on data tables (NULL on
    // prompt_versions = global default; non-NULL = workspace override).
    ensure_column(&conn, "haie_rev", "workspace_id", "INTEGER")?;
    ensure_column(&conn, "job_runs", "workspace_id", "INTEGER")?;
    ensure_column(&conn, "prompt_versions", "workspace_id", "INTEGER")?;
    ensure_column(&conn, "prompt_traces", "workspace_id", "INTEGER")?;

    conn.execute_batch(
        "
        CREATE INDEX IF NOT EXISTS idx_haie_rev_workspace ON haie_rev(workspace_id);
        CREATE INDEX IF NOT EXISTS idx_job_runs_workspace ON job_runs(workspace_id);
        CREATE INDEX IF NOT EXISTS idx_prompt_versions_workspace
            ON prompt_versions(prompt_name, workspace_id);
        CREATE INDEX IF NOT EXISTS idx_prompt_traces_workspace ON prompt_traces(workspace_id);
        ",
    )?;

    seed_default_workspace_and_backfill(&conn)?;

    Ok(())
}

/// Seeds the default "Healthcare AI Ethics" workspace on first run and assigns
/// any pre-existing rows (from the single-topic era) to it. `prompt_versions`
/// stays NULL = global defaults shared by every workspace until overridden.
fn seed_default_workspace_and_backfill(conn: &Connection) -> Result<()> {
    conn.execute(
        "INSERT INTO workspaces
            (name, slug, primary_question, topic_descriptor, seed_concepts_json, is_active)
         SELECT 'Healthcare AI Ethics', 'healthcare-ai-ethics',
                'What are the ethical implications of AI in healthcare?',
                'the ethics of artificial intelligence in healthcare and clinical medicine',
                '[\"artificial intelligence\",\"clinical decision support\",\"algorithmic fairness\",\"patient privacy\",\"AI governance\"]',
                1
         WHERE NOT EXISTS (SELECT 1 FROM workspaces)",
        [],
    )?;

    let default_id: i64 = conn.query_row(
        "SELECT id FROM workspaces ORDER BY id LIMIT 1",
        [],
        |row| row.get(0),
    )?;

    conn.execute(
        "UPDATE haie_rev SET workspace_id = ?1 WHERE workspace_id IS NULL",
        [default_id],
    )?;
    conn.execute(
        "UPDATE job_runs SET workspace_id = ?1 WHERE workspace_id IS NULL",
        [default_id],
    )?;
    conn.execute(
        "UPDATE prompt_traces SET workspace_id = ?1 WHERE workspace_id IS NULL",
        [default_id],
    )?;

    Ok(())
}

fn ensure_column(conn: &Connection, table: &str, column: &str, column_type: &str) -> Result<()> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let columns = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    if !columns.iter().any(|existing| existing == column) {
        conn.execute(
            &format!("ALTER TABLE {table} ADD COLUMN {column} {column_type}"),
            [],
        )?;
    }

    Ok(())
}

fn table_exists(conn: &Connection, name: &str) -> Result<bool> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type IN ('table', 'view') AND name = ?1",
        [name],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

fn is_vec0_table(conn: &Connection, name: &str) -> Result<bool> {
    let sql: Option<String> = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE name = ?1",
            [name],
            |row| row.get(0),
        )
        .unwrap_or(None);
    Ok(sql.as_deref().is_some_and(|s| s.contains("vec0")))
}

fn migrate_vec_table(conn: &Connection, name: &str, schema: &str) -> Result<()> {
    if table_exists(conn, name)? {
        if is_vec0_table(conn, name)? && schema_matches(conn, name, schema) {
            return Ok(());
        }
        conn.execute(&format!("DROP TABLE {name}"), [])?;
    }
    conn.execute_batch(&format!("CREATE VIRTUAL TABLE {name} USING vec0({schema})"))?;
    Ok(())
}

fn schema_matches(conn: &Connection, name: &str, expected_schema: &str) -> bool {
    let sql: Option<String> = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE name = ?1",
            [name],
            |row| row.get(0),
        )
        .unwrap_or(None);
    sql.as_deref().is_some_and(|s| s.contains(expected_schema))
}

fn create_fts_table(conn: &Connection) -> Result<()> {
    if !table_exists(conn, "fts_article_chunks")? {
        conn.execute_batch(
            "CREATE VIRTUAL TABLE fts_article_chunks USING fts5(
                content,
                content='article_chunks',
                content_rowid='id',
                tokenize='porter unicode61'
            )",
        )?;
    }

    conn.execute_batch(
        "
        CREATE TRIGGER IF NOT EXISTS fts_chunks_insert AFTER INSERT ON article_chunks BEGIN
            INSERT INTO fts_article_chunks(rowid, content) VALUES (NEW.id, NEW.content);
        END;

        CREATE TRIGGER IF NOT EXISTS fts_chunks_delete AFTER DELETE ON article_chunks BEGIN
            INSERT INTO fts_article_chunks(fts_article_chunks, rowid, content)
            VALUES ('delete', OLD.id, OLD.content);
        END;

        CREATE TRIGGER IF NOT EXISTS fts_chunks_update AFTER UPDATE ON article_chunks BEGIN
            INSERT INTO fts_article_chunks(fts_article_chunks, rowid, content)
            VALUES ('delete', OLD.id, OLD.content);
            INSERT INTO fts_article_chunks(rowid, content) VALUES (NEW.id, NEW.content);
        END;
        ",
    )?;

    Ok(())
}

fn create_synthesis_fts_table(conn: &Connection) -> Result<()> {
    if !table_exists(conn, "fts_kg_syntheses")? {
        conn.execute_batch(
            "CREATE VIRTUAL TABLE fts_kg_syntheses USING fts5(
                synthesis,
                summary,
                content='kg_entity_syntheses',
                content_rowid='id',
                tokenize='porter unicode61'
            )",
        )?;
    }

    conn.execute_batch(
        "
        CREATE TRIGGER IF NOT EXISTS fts_syntheses_insert AFTER INSERT ON kg_entity_syntheses BEGIN
            INSERT INTO fts_kg_syntheses(rowid, synthesis, summary)
            VALUES (NEW.id, NEW.synthesis, NEW.summary);
        END;

        CREATE TRIGGER IF NOT EXISTS fts_syntheses_delete AFTER DELETE ON kg_entity_syntheses BEGIN
            INSERT INTO fts_kg_syntheses(fts_kg_syntheses, rowid, synthesis, summary)
            VALUES ('delete', OLD.id, OLD.synthesis, OLD.summary);
        END;

        CREATE TRIGGER IF NOT EXISTS fts_syntheses_update AFTER UPDATE ON kg_entity_syntheses BEGIN
            INSERT INTO fts_kg_syntheses(fts_kg_syntheses, rowid, synthesis, summary)
            VALUES ('delete', OLD.id, OLD.synthesis, OLD.summary);
            INSERT INTO fts_kg_syntheses(rowid, synthesis, summary)
            VALUES (NEW.id, NEW.synthesis, NEW.summary);
        END;
        ",
    )?;

    Ok(())
}
