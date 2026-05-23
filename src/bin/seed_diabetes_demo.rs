use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use chrono::Utc;
use researchwiki::{
    app::{bootstrap_db, first_launch_seed},
    config::{AppConfig, EmbeddingConfig, LlmConfig, StorageConfig},
    db, register_sqlite_vec,
};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::json;

const WORKSPACE_NAME: &str = "Diabetes chatbot self-management evidence map";
const WORKSPACE_SLUG: &str = "diabetes-chatbot-self-management-evidence-map";
const WORKSPACE_DB: &str = "ws_diabetes_chatbot_self_management_evidence_map.db";
const SOURCE_PACKAGE: &str = "MDR_diabetes_chatbot_self_management_v1";
const PRIMARY_QUESTION: &str =
    "Do chatbot/conversational agents improve diabetes self-management outcomes?";
const GAP_NOTE: &str = "The current evidence supports feasibility and possible benefit, but the next research question should separate chatbot-based education from newer LLM-based counseling, and should predefine safety escalation, misinformation handling, and subgroup effects.";
const REFINED_QUESTION: &str = "In adults with type 2 diabetes, does an LLM-based counseling agent with predefined safety escalation and misinformation handling, compared with static chatbot education or usual digital education, improve HbA1c, adherence, HRQoL, and safety outcomes across prespecified subgroups over 6-12 months?";

fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    researchwiki::init_tracing();
    register_sqlite_vec();

    let config = demo_config_from_env().context("load demo config")?;
    ensure_demo_settings(&config)?;
    first_launch_seed(&config).context("seed app directories")?;
    refresh_demo_prompts(&config).context("refresh demo prompts")?;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("create runtime")?;

    runtime.block_on(async {
        bootstrap_db(&config).await?;
        let root = config
            .storage
            .database_path
            .parent()
            .map(PathBuf::from)
            .context("database path has no parent")?;
        let meta_path = root.join("meta.db");
        let workspace_id = upsert_workspace(&meta_path)?;
        let workspace_db = root.join(WORKSPACE_DB);
        db::initialize_workspace_db(workspace_db.clone(), config.embedding_dimensions).await?;
        seed_workspace_db(&workspace_db, workspace_id)?;
        println!("Seeded demo workspace:");
        println!("  workspace : {WORKSPACE_NAME}");
        println!("  source    : {SOURCE_PACKAGE}");
        println!("  meta db   : {}", meta_path.display());
        println!("  data db   : {}", workspace_db.display());
        Ok::<_, anyhow::Error>(())
    })
}

fn demo_config_from_env() -> Result<AppConfig> {
    let root = std::env::current_dir()?.join(".demo-data");
    let env_path =
        |key: &str, fallback: PathBuf| std::env::var_os(key).map(PathBuf::from).unwrap_or(fallback);

    Ok(AppConfig {
        storage: StorageConfig {
            database_path: env_path("DATABASE_PATH", root.join("haie.db")),
            prompts_dir: env_path("PROMPTS_DIR", root.join("prompts")),
            settings_file: env_path("SETTINGS_FILE", root.join("settings.json")),
            wiki_export_dir: env_path("WIKI_EXPORT_DIR", root.join("wiki")),
        },
        llm: LlmConfig {
            base_url: std::env::var("LLM_BASE_URL")
                .unwrap_or_else(|_| "http://localhost:11434/v1".to_string()),
            model: std::env::var("LLM_MODEL").unwrap_or_else(|_| "demo-local".to_string()),
            api_key: std::env::var("LLM_API_KEY").unwrap_or_default(),
            disable_thinking: true,
            connect_timeout_seconds: env_parse("LLM_CONNECT_TIMEOUT_SECONDS", 5),
            request_timeout_seconds: env_parse("LLM_REQUEST_TIMEOUT_SECONDS", 300),
            max_attempts: env_parse("LLM_MAX_ATTEMPTS", 1),
            max_concurrent_requests: env_parse("LLM_MAX_CONCURRENT_REQUESTS", 1),
        },
        embedding: EmbeddingConfig {
            base_url: std::env::var("EMBEDDING_BASE_URL").unwrap_or_default(),
            model: std::env::var("EMBEDDING_MODEL").unwrap_or_default(),
            api_key: std::env::var("EMBEDDING_API_KEY").unwrap_or_default(),
        },
        embedding_dimensions: std::env::var("EMBEDDING_DIMENSIONS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(1536),
        contact_email: std::env::var("RESEARCHWIKI_CONTACT_EMAIL")
            .or_else(|_| std::env::var("UNPAYWALL_EMAIL"))
            .unwrap_or_default(),
        semantic_scholar_api_key: std::env::var("SEMANTIC_SCHOLAR_API_KEY").unwrap_or_default(),
    })
}

fn env_parse<T: std::str::FromStr>(key: &str, default: T) -> T {
    std::env::var(key)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn ensure_demo_settings(config: &AppConfig) -> Result<()> {
    if let Some(parent) = config.storage.settings_file.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create settings dir {}", parent.display()))?;
    }
    let settings = json!({
        "api_keys": {},
        "scheduler": {
            "arxiv_schedule_hour": 19,
            "arxiv_schedule_minute": 0,
            "pmc_schedule_hour": 18,
            "pmc_schedule_minute": 0,
            "pubmed_schedule_hour": 18,
            "pubmed_schedule_minute": 30,
            "enabled": true
        },
        "newsletter": {
            "default_article_count": 7,
            "default_days": 7
        },
        "library_enabled": false,
        "kg_enabled": true,
        "llm": {
            "base_url": config.llm.base_url,
            "model": config.llm.model,
            "api_key": config.llm.api_key,
            "disable_thinking": true,
            "connect_timeout_seconds": config.llm.connect_timeout_seconds,
            "request_timeout_seconds": config.llm.request_timeout_seconds,
            "max_attempts": config.llm.max_attempts,
            "max_concurrent_requests": config.llm.max_concurrent_requests
        },
        "embedding": {
            "base_url": config.embedding.base_url,
            "model": config.embedding.model,
            "api_key": config.embedding.api_key
        },
        "embedding_dimensions": config.embedding_dimensions
    });
    fs::write(
        &config.storage.settings_file,
        serde_json::to_vec_pretty(&settings)?,
    )
    .with_context(|| format!("write settings {}", config.storage.settings_file.display()))?;
    Ok(())
}

fn refresh_demo_prompts(config: &AppConfig) -> Result<()> {
    let source = std::env::current_dir()?.join("prompts");
    copy_dir_overwrite(&source, &config.storage.prompts_dir)
}

fn copy_dir_overwrite(source: &Path, target: &Path) -> Result<()> {
    fs::create_dir_all(target)
        .with_context(|| format!("create prompt dir {}", target.display()))?;
    for entry in
        fs::read_dir(source).with_context(|| format!("read prompts {}", source.display()))?
    {
        let entry = entry?;
        let from = entry.path();
        let to = target.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_overwrite(&from, &to)?;
        } else {
            fs::copy(&from, &to)
                .with_context(|| format!("copy prompt {} to {}", from.display(), to.display()))?;
        }
    }
    Ok(())
}

fn upsert_workspace(meta_path: &PathBuf) -> Result<i64> {
    let conn = db::open_connection(meta_path)
        .with_context(|| format!("open meta db {}", meta_path.display()))?;
    conn.execute_batch("PRAGMA foreign_keys=ON;")?;

    let seed_concepts = json!([
        "Type 2 diabetes",
        "Chatbot intervention",
        "Conversational agent",
        "LLM patient education",
        "Self-management education",
        "HbA1c",
        "Adherence",
        "HRQoL",
        "Safety escalation",
        "Evidence gap"
    ])
    .to_string();
    let override_queries = json!([
        "type 2 diabetes chatbot HbA1c adherence randomized trial",
        "diabetes conversational agent self-management quality of life",
        "large language model diabetes patient education safety escalation misinformation"
    ])
    .to_string();
    let topic_descriptor = "chatbot and conversational agent interventions for type 2 diabetes self-management, HbA1c, adherence, HRQoL, and safety escalation";

    conn.execute(
        "INSERT INTO workspaces
            (name, slug, db_filename, primary_question, gap_note, refined_question,
             seed_concepts_json, override_queries_json, topic_descriptor, lookback_days, is_active)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 3650, 1)
         ON CONFLICT(slug) DO UPDATE SET
            name = excluded.name,
            db_filename = excluded.db_filename,
            primary_question = excluded.primary_question,
            gap_note = excluded.gap_note,
            refined_question = excluded.refined_question,
            seed_concepts_json = excluded.seed_concepts_json,
            override_queries_json = excluded.override_queries_json,
            topic_descriptor = excluded.topic_descriptor,
            lookback_days = excluded.lookback_days,
            is_active = 1,
            updated_at = datetime('now')",
        params![
            WORKSPACE_NAME,
            WORKSPACE_SLUG,
            WORKSPACE_DB,
            PRIMARY_QUESTION,
            GAP_NOTE,
            REFINED_QUESTION,
            seed_concepts,
            override_queries,
            topic_descriptor,
        ],
    )?;
    let id: i64 = conn.query_row(
        "SELECT id FROM workspaces WHERE slug = ?1",
        [WORKSPACE_SLUG],
        |row| row.get(0),
    )?;
    conn.execute(
        "UPDATE workspaces SET is_active = CASE WHEN id = ?1 THEN 1 ELSE 0 END",
        [id],
    )?;
    Ok(id)
}

fn seed_workspace_db(db_path: &PathBuf, workspace_id: i64) -> Result<()> {
    let conn = db::open_connection(db_path)
        .with_context(|| format!("open workspace db {}", db_path.display()))?;
    conn.execute_batch("PRAGMA foreign_keys=ON;")?;

    upsert_local_workspace(&conn, workspace_id)?;
    clear_demo_rows(&conn, workspace_id)?;
    seed_articles(&conn, workspace_id)?;
    let entity_ids = seed_entities(&conn)?;
    seed_article_entities(&conn, &entity_ids)?;
    seed_relationships(&conn, &entity_ids)?;
    seed_syntheses(&conn, &entity_ids)?;
    seed_gap_finding(&conn, workspace_id)?;
    seed_job_history(&conn, workspace_id)?;
    Ok(())
}

fn upsert_local_workspace(conn: &Connection, workspace_id: i64) -> Result<()> {
    conn.execute(
        "INSERT INTO workspaces
            (id, name, slug, primary_question, gap_note, refined_question,
             seed_concepts_json, override_queries_json, topic_descriptor, lookback_days, is_active)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 3650, 1)
         ON CONFLICT(id) DO UPDATE SET
            name = excluded.name,
            slug = excluded.slug,
            primary_question = excluded.primary_question,
            gap_note = excluded.gap_note,
            refined_question = excluded.refined_question,
            seed_concepts_json = excluded.seed_concepts_json,
            override_queries_json = excluded.override_queries_json,
            topic_descriptor = excluded.topic_descriptor,
            lookback_days = excluded.lookback_days,
            is_active = 1,
            updated_at = datetime('now')",
        params![
            workspace_id,
            WORKSPACE_NAME,
            WORKSPACE_SLUG,
            PRIMARY_QUESTION,
            GAP_NOTE,
            REFINED_QUESTION,
            json!([
                "Type 2 diabetes",
                "Chatbot intervention",
                "Conversational agent",
                "LLM patient education",
                "Self-management education",
                "HbA1c",
                "Adherence",
                "HRQoL",
                "Safety escalation",
                "Evidence gap"
            ])
            .to_string(),
            json!([
                "type 2 diabetes chatbot HbA1c adherence randomized trial",
                "diabetes conversational agent self-management quality of life",
                "large language model diabetes patient education safety escalation misinformation"
            ])
            .to_string(),
            "chatbot and conversational agent interventions for type 2 diabetes self-management, HbA1c, adherence, HRQoL, and safety escalation",
        ],
    )?;
    Ok(())
}

fn clear_demo_rows(conn: &Connection, workspace_id: i64) -> Result<()> {
    let mut uids = demo_uids()
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
    let mut stmt = conn.prepare("SELECT uid FROM haie_rev WHERE workspace_id = ?1")?;
    let existing = stmt
        .query_map([workspace_id], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    for uid in existing {
        if !uids.iter().any(|known| known == &uid) {
            uids.push(uid);
        }
    }
    for uid in &uids {
        conn.execute(
            "DELETE FROM vec_article_chunks
             WHERE chunk_id IN (SELECT id FROM article_chunks WHERE article_uid = ?1)",
            [uid.as_str()],
        )?;
        conn.execute(
            "DELETE FROM article_chunks WHERE article_uid = ?1",
            [uid.as_str()],
        )?;
        conn.execute(
            "DELETE FROM kg_article_entities WHERE article_uid = ?1",
            [uid.as_str()],
        )?;
        conn.execute("DELETE FROM haie_rev WHERE uid = ?1", [uid.as_str()])?;
    }
    conn.execute(
        "DELETE FROM job_events WHERE run_id LIKE 'demo-diabetes-%'",
        [],
    )?;
    conn.execute(
        "DELETE FROM job_runs WHERE workspace_id = ?1 OR id LIKE 'demo-diabetes-%'",
        [workspace_id],
    )?;
    conn.execute(
        "DELETE FROM kg_gap_findings WHERE workspace_id = ?1",
        [workspace_id],
    )?;

    conn.execute("DELETE FROM kg_relationships", [])?;
    conn.execute("DELETE FROM kg_entity_syntheses", [])?;
    conn.execute("DELETE FROM kg_article_entities", [])?;
    conn.execute("DELETE FROM kg_entities", [])?;

    for name in entity_names() {
        if let Some(id) = conn
            .query_row(
                "SELECT id FROM kg_entities WHERE canonical_name = ?1",
                [name],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
        {
            conn.execute(
                "DELETE FROM kg_relationships WHERE source_entity_id = ?1 OR target_entity_id = ?1",
                [id],
            )?;
            conn.execute("DELETE FROM kg_entity_syntheses WHERE entity_id = ?1", [id])?;
            conn.execute("DELETE FROM kg_article_entities WHERE entity_id = ?1", [id])?;
            conn.execute("DELETE FROM kg_entities WHERE id = ?1", [id])?;
        }
    }
    Ok(())
}

fn seed_articles(conn: &Connection, workspace_id: i64) -> Result<()> {
    let articles = [
        DemoArticle {
            uid: "demo-diabetes-chatbot-rct-2023",
            category: "pubmed",
            title: "Conversational chatbot support for type 2 diabetes self-management: randomized feasibility trial",
            first_author: "Evidence Map Demo",
            pub_date: "2023-10-12",
            journal: "Journal of Diabetes Digital Health",
            doi: "10.5555/demo.diabetes.chatbot.2023",
            byline_summary: "Chatbot education and reminders were feasible and associated with modest HbA1c improvement.",
            why_it_matters: "Supports feasibility while leaving safety escalation and LLM counseling questions unresolved.",
            main_findings: "Participants using a structured chatbot had better engagement and small HbA1c reductions; adherence signals were positive but heterogeneous.",
            limitations: "Short follow-up, small sample, limited safety escalation reporting, and no LLM-based counseling arm.",
            full_text: "A structured diabetes chatbot delivered self-management education, medication reminders, and goal-setting prompts. Outcomes included HbA1c, adherence, HRQoL, and engagement. The trial reported feasibility and possible benefit but did not test large language model counseling or misinformation handling.",
        },
        DemoArticle {
            uid: "demo-diabetes-agent-adherence-2022",
            category: "europepmc",
            title: "Conversational agents for medication adherence in adults with diabetes",
            first_author: "Evidence Map Demo",
            pub_date: "2022-07-04",
            journal: "Digital Therapeutics Review",
            doi: "10.5555/demo.diabetes.adherence.2022",
            byline_summary: "Conversational reminders improved self-reported adherence, but outcomes varied by baseline digital literacy.",
            why_it_matters: "Highlights subgroup effects that should be prespecified in the next trial.",
            main_findings: "Adherence nudges and self-monitoring check-ins were acceptable; HbA1c effects were inconsistent.",
            limitations: "Underpowered subgroup analysis and limited reporting of escalation to clinicians.",
            full_text: "Conversational agent interventions for diabetes medication adherence used reminders, motivational prompts, and feedback loops. The evidence was strongest for acceptability and engagement. Studies rarely predefined subgroup effects or clinician escalation.",
        },
        DemoArticle {
            uid: "demo-diabetes-llm-counseling-2024",
            category: "medrxiv",
            title: "LLM patient education for diabetes: opportunities, misinformation risks, and escalation design",
            first_author: "Evidence Map Demo",
            pub_date: "2024-03-18",
            journal: "Preprint Evidence Map",
            doi: "10.5555/demo.diabetes.llm.2024",
            byline_summary: "LLM counseling could personalize education but requires misinformation handling and safety escalation.",
            why_it_matters: "Separates newer LLM counseling from rule-based chatbot education.",
            main_findings: "LLM-generated counseling was responsive and patient-centered in simulation, but safety behaviors depended on guardrails.",
            limitations: "Simulation evidence, no clinical outcome trial, uncertain real-world escalation fidelity.",
            full_text: "Large language model patient education for type 2 diabetes may support personalized counseling, but it introduces misinformation risks. A refined trial should define safety escalation triggers, handling of unsafe advice, and subgroup analyses.",
        },
        DemoArticle {
            uid: "demo-diabetes-hrqol-2021",
            category: "openalex",
            title: "Digital self-management education and quality of life outcomes in type 2 diabetes",
            first_author: "Evidence Map Demo",
            pub_date: "2021-11-22",
            journal: "Patient Education Outcomes",
            doi: "10.5555/demo.diabetes.hrqol.2021",
            byline_summary: "Self-management education can improve confidence and HRQoL when paired with sustained engagement.",
            why_it_matters: "Frames HRQoL as an endpoint beyond HbA1c.",
            main_findings: "Patient education interventions improved self-efficacy; HRQoL effects were more likely when support was interactive.",
            limitations: "Intervention components were mixed and chatbot-specific effects were hard to isolate.",
            full_text: "Diabetes self-management education trials measured HbA1c, adherence, self-efficacy, and health-related quality of life. Interactive support may improve HRQoL, but chatbot and conversational agent components were not consistently separated.",
        },
        DemoArticle {
            uid: "demo-diabetes-safety-escalation-2025",
            category: "clinical_trials",
            title: "Protocol features for safety escalation in conversational diabetes support",
            first_author: "Evidence Map Demo",
            pub_date: "2025-02-09",
            journal: "ClinicalTrials.gov",
            doi: "NCT00000000-demo",
            byline_summary: "Safety escalation protocols should cover hypoglycemia, medication questions, and urgent symptoms.",
            why_it_matters: "Turns the evidence gap into a trial-design requirement.",
            main_findings: "Safety escalation and misinformation audits are increasingly specified, but outcome reporting remains immature.",
            limitations: "Protocol-stage evidence and no completed comparative LLM counseling outcomes yet.",
            full_text: "A diabetes conversational support protocol predefined safety escalation for hypoglycemia, medication changes, and urgent symptoms. The protocol also specified misinformation handling and subgroup effects by digital literacy and baseline HbA1c.",
        },
    ];

    for article in articles {
        conn.execute(
            "INSERT INTO haie_rev
                (uid, url, category, reg_date, title, first_author, authors, pub_date, journal, doi,
                 ai_tech, clinical_domain, primary_issue, secondary_issues, key_stakeholders,
                 byline_summary, why_it_matters, main_findings, limitations, scholarly_rigor,
                 novelty, relevance_score, practical_impact, interdisciplinary, critical_concerns,
                 total_score, priority, full_text, content_type, has_embeddings, has_kg_entities,
                 workspace_id)
             VALUES
                (?1, ?2, ?3, date('now'), ?4, ?5, ?5, ?6, ?7, ?8,
                 'chatbot / conversational agent', 'type 2 diabetes',
                 'self-management education outcomes', 'HbA1c; adherence; HRQoL; safety escalation',
                 'patients; clinicians; trialists', ?9, ?10, ?11, ?12,
                 4, 4, 5, 4, 3, -1, 83, 'Tier1', ?13, 'text', 0, 1, ?14)",
            params![
                article.uid,
                format!("demo://{SOURCE_PACKAGE}/{}", article.uid),
                article.category,
                article.title,
                article.first_author,
                article.pub_date,
                article.journal,
                article.doi,
                article.byline_summary,
                article.why_it_matters,
                article.main_findings,
                article.limitations,
                article.full_text,
                workspace_id,
            ],
        )?;

        conn.execute(
            "INSERT INTO article_chunks
                (article_uid, chunk_index, chunk_type, content, token_count, embedded_at)
             VALUES (?1, 0, 'body', ?2, ?3, datetime('now'))",
            params![
                article.uid,
                article.full_text,
                (article.full_text.len() / 4).max(1) as i64,
            ],
        )?;
    }
    Ok(())
}

fn seed_entities(conn: &Connection) -> Result<std::collections::BTreeMap<&'static str, i64>> {
    let entities = [
        (
            "Type 2 diabetes",
            "MEDICAL_CONDITION",
            "Adults with type 2 diabetes are the target population for the self-management evidence map.",
            8,
        ),
        (
            "Chatbot intervention",
            "TECHNOLOGY",
            "Rule-based or scripted chatbot support used for diabetes education, reminders, and self-management coaching.",
            7,
        ),
        (
            "Conversational agent",
            "TECHNOLOGY",
            "Interactive conversational interfaces, including chatbots and agentic education tools.",
            6,
        ),
        (
            "LLM patient education",
            "TECHNOLOGY",
            "Large language model counseling or education that can personalize responses but introduces misinformation and safety risks.",
            5,
        ),
        (
            "Self-management education",
            "CONCEPT",
            "Education and support for glucose monitoring, medication adherence, diet, activity, and problem solving.",
            7,
        ),
        (
            "HbA1c",
            "CONCEPT",
            "Glycemic outcome commonly used to assess diabetes intervention effectiveness.",
            6,
        ),
        (
            "Adherence",
            "CONCEPT",
            "Medication, monitoring, and behavior adherence outcomes reported in diabetes self-management studies.",
            5,
        ),
        (
            "HRQoL",
            "CONCEPT",
            "Health-related quality of life outcomes beyond biomedical glycemic control.",
            4,
        ),
        (
            "Safety escalation",
            "METHODOLOGY",
            "Predefined routing of unsafe, urgent, or medication-related patient concerns to a clinician or emergency guidance.",
            5,
        ),
        (
            "Misinformation handling",
            "METHODOLOGY",
            "Detection, correction, and audit of inaccurate or unsafe advice generated by conversational systems.",
            4,
        ),
        (
            "Subgroup effects",
            "METHODOLOGY",
            "Prespecified heterogeneity analyses such as digital literacy, baseline HbA1c, age, and language.",
            4,
        ),
        (
            "Evidence gap",
            "CONCEPT",
            "The unresolved distinction between feasibility evidence and comparative clinical effectiveness for LLM counseling.",
            5,
        ),
    ];

    let mut ids = std::collections::BTreeMap::new();
    for (name, entity_type, description, mention_count) in entities {
        conn.execute(
            "INSERT INTO kg_entities
                (canonical_name, entity_type, description, aliases_json, mention_count)
             VALUES (?1, ?2, ?3, '[]', ?4)
             ON CONFLICT(canonical_name) DO UPDATE SET
                entity_type = excluded.entity_type,
                description = excluded.description,
                mention_count = excluded.mention_count,
                updated_at = datetime('now')",
            params![name, entity_type, description, mention_count],
        )?;
        let id = conn.query_row(
            "SELECT id FROM kg_entities WHERE canonical_name = ?1",
            [name],
            |row| row.get::<_, i64>(0),
        )?;
        ids.insert(name, id);
    }
    Ok(ids)
}

fn seed_article_entities(
    conn: &Connection,
    entity_ids: &std::collections::BTreeMap<&'static str, i64>,
) -> Result<()> {
    let mentions = [
        (
            "demo-diabetes-chatbot-rct-2023",
            vec![
                "Type 2 diabetes",
                "Chatbot intervention",
                "Conversational agent",
                "Self-management education",
                "HbA1c",
                "Adherence",
                "Evidence gap",
            ],
        ),
        (
            "demo-diabetes-agent-adherence-2022",
            vec![
                "Type 2 diabetes",
                "Chatbot intervention",
                "Conversational agent",
                "Self-management education",
                "Adherence",
                "Subgroup effects",
            ],
        ),
        (
            "demo-diabetes-llm-counseling-2024",
            vec![
                "Type 2 diabetes",
                "Conversational agent",
                "LLM patient education",
                "Self-management education",
                "Safety escalation",
                "Misinformation handling",
                "Evidence gap",
            ],
        ),
        (
            "demo-diabetes-hrqol-2021",
            vec![
                "Type 2 diabetes",
                "Chatbot intervention",
                "Self-management education",
                "HbA1c",
                "HRQoL",
                "Evidence gap",
            ],
        ),
        (
            "demo-diabetes-safety-escalation-2025",
            vec![
                "Type 2 diabetes",
                "Conversational agent",
                "LLM patient education",
                "Safety escalation",
                "Misinformation handling",
                "Subgroup effects",
                "Evidence gap",
            ],
        ),
    ];

    for (uid, names) in mentions {
        for (idx, name) in names.into_iter().enumerate() {
            let entity_id = entity_ids[name];
            conn.execute(
                "INSERT OR IGNORE INTO kg_article_entities
                    (article_uid, entity_id, mention_text, context, chunk_index)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    uid,
                    entity_id,
                    name,
                    format!("{name} was identified in {SOURCE_PACKAGE}."),
                    idx as i64,
                ],
            )?;
        }
    }
    Ok(())
}

fn seed_relationships(
    conn: &Connection,
    entity_ids: &std::collections::BTreeMap<&'static str, i64>,
) -> Result<()> {
    let relationships = [
        (
            "Chatbot intervention",
            "Self-management education",
            "supports",
            4.0,
            "Chatbots deliver structured education, reminders, and goal-setting prompts.",
        ),
        (
            "Conversational agent",
            "Type 2 diabetes",
            "targets_population",
            4.0,
            "Most mapped interventions target adults with type 2 diabetes.",
        ),
        (
            "Self-management education",
            "HbA1c",
            "measures_outcome",
            3.0,
            "HbA1c is a common endpoint for diabetes self-management education.",
        ),
        (
            "Self-management education",
            "Adherence",
            "measures_outcome",
            3.0,
            "Adherence is reported as medication, monitoring, or behavior adherence.",
        ),
        (
            "Self-management education",
            "HRQoL",
            "measures_outcome",
            2.0,
            "HRQoL captures patient-centered benefit beyond glycemic control.",
        ),
        (
            "LLM patient education",
            "Misinformation handling",
            "requires_guardrail",
            3.0,
            "LLM counseling must address unsafe or inaccurate advice.",
        ),
        (
            "LLM patient education",
            "Safety escalation",
            "requires_guardrail",
            3.0,
            "The next research question should predefine escalation triggers.",
        ),
        (
            "Evidence gap",
            "LLM patient education",
            "distinguishes",
            3.0,
            "Existing chatbot evidence should be separated from newer LLM counseling.",
        ),
        (
            "Evidence gap",
            "Subgroup effects",
            "requires_prespecification",
            2.0,
            "Digital literacy, baseline HbA1c, and other subgroup effects remain underexplored.",
        ),
        (
            "Safety escalation",
            "Type 2 diabetes",
            "protects_population",
            2.0,
            "Escalation is clinically relevant for hypoglycemia, medication changes, and urgent symptoms.",
        ),
    ];

    for (source, target, rel_type, weight, evidence) in relationships {
        conn.execute(
            "INSERT INTO kg_relationships
                (source_entity_id, target_entity_id, relationship_type, description, weight,
                 source_articles_json, evidence_summary)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(source_entity_id, target_entity_id, relationship_type)
             DO UPDATE SET
                description = excluded.description,
                weight = excluded.weight,
                source_articles_json = excluded.source_articles_json,
                evidence_summary = excluded.evidence_summary,
                updated_at = datetime('now')",
            params![
                entity_ids[source],
                entity_ids[target],
                rel_type,
                evidence,
                weight,
                json!(demo_uids()).to_string(),
                evidence,
            ],
        )?;
    }
    Ok(())
}

fn seed_syntheses(
    conn: &Connection,
    entity_ids: &std::collections::BTreeMap<&'static str, i64>,
) -> Result<()> {
    let syntheses = [
        (
            "Chatbot intervention",
            3,
            "Feasible diabetes chatbots may improve engagement and possibly HbA1c, but the evidence is not yet definitive.",
            "# Chatbot self-management\n\nThe mapped evidence supports feasibility for structured diabetes chatbots that deliver self-management education, reminders, and goal-setting. Signals for HbA1c and adherence are promising but heterogeneous.\n\nThe main bridge is methodological: existing chatbot education should not be pooled uncritically with newer LLM counseling. The next trial should compare these approaches directly and define safety escalation, misinformation handling, and subgroup effects before enrollment.",
            vec![
                "Feasibility and engagement are stronger than clinical effectiveness.",
                "HbA1c and adherence signals are plausible but heterogeneous.",
                "LLM counseling should be analyzed separately from scripted education.",
            ],
        ),
        (
            "Self-management education",
            4,
            "Self-management education links the intervention to HbA1c, adherence, and HRQoL endpoints.",
            "# Self-management education\n\nDiabetes self-management education is the central intervention function across the evidence map. Conversational tools deliver education, reminders, and self-monitoring prompts.\n\nFuture evaluations should define whether the active component is scripted education, adaptive coaching, or LLM-based counseling.",
            vec![
                "Maps intervention content to outcomes.",
                "Supports both biomedical and patient-centered endpoints.",
                "Needs clearer component separation.",
            ],
        ),
        (
            "Evidence gap",
            4,
            "The evidence gap is the move from feasible chatbot education to safer, testable LLM counseling.",
            "# Evidence gap\n\nThe current evidence supports feasibility and possible benefit. It does not yet answer whether an LLM-based counseling agent improves outcomes compared with structured chatbot education or usual digital education.\n\nA refined trial should prespecify safety escalation, misinformation handling, and subgroup effects.",
            vec![
                "Separate chatbot education from LLM counseling.",
                "Predefine escalation and misinformation handling.",
                "Prespecify subgroup effects.",
            ],
        ),
    ];

    for (name, source_count, summary, synthesis, aspects) in syntheses {
        conn.execute(
            "INSERT INTO kg_entity_syntheses
                (entity_id, synthesis, summary, key_aspects_json, related_entities_json,
                 source_article_count, compiled_at, stale, version)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, datetime('now'), 0, 1)
             ON CONFLICT(entity_id) DO UPDATE SET
                synthesis = excluded.synthesis,
                summary = excluded.summary,
                key_aspects_json = excluded.key_aspects_json,
                related_entities_json = excluded.related_entities_json,
                source_article_count = excluded.source_article_count,
                compiled_at = datetime('now'),
                stale = 0,
                version = kg_entity_syntheses.version + 1,
                updated_at = datetime('now')",
            params![
                entity_ids[name],
                synthesis,
                summary,
                serde_json::to_string(&aspects)?,
                json!([
                    {"entity_name": "Type 2 diabetes", "relationship": "population"},
                    {"entity_name": "Safety escalation", "relationship": "trial safeguard"},
                    {"entity_name": "LLM patient education", "relationship": "refined intervention"}
                ])
                .to_string(),
                source_count,
            ],
        )?;
    }
    Ok(())
}

fn seed_gap_finding(conn: &Connection, workspace_id: i64) -> Result<()> {
    let issues = json!([
        {
            "entity_name": "LLM patient education",
            "issue_type": "under-specified comparator",
            "suggestion": "Separate scripted chatbot education from LLM-based counseling in the next trial.",
            "confidence": 0.91
        },
        {
            "entity_name": "Safety escalation",
            "issue_type": "safety protocol gap",
            "suggestion": "Predefine escalation triggers and clinician handoff for medication or urgent symptom advice.",
            "confidence": 0.88
        },
        {
            "entity_name": "Subgroup effects",
            "issue_type": "heterogeneity gap",
            "suggestion": "Prespecify subgroup effects by baseline HbA1c, digital literacy, language, and age.",
            "confidence": 0.84
        }
    ]);
    conn.execute(
        "INSERT INTO kg_gap_findings
            (workspace_id, entities_reviewed, issues_json, refined_question, created_at)
         VALUES (?1, 12, ?2, ?3, datetime('now'))",
        params![workspace_id, issues.to_string(), REFINED_QUESTION],
    )?;
    Ok(())
}

fn seed_job_history(conn: &Connection, workspace_id: i64) -> Result<()> {
    let now = Utc::now();
    let runs = [
        DemoRun {
            id: "demo-diabetes-cron-pubmed",
            source: "pubmed",
            requested_offset_min: 45,
            started_offset_min: 44,
            finished_offset_min: 38,
            found: 42,
            screened: 42,
            relevant: 11,
            fetched: 9,
            evaluated: 9,
            saved: 5,
            embedded: 0,
            skipped: 4,
            step: "cron completed · MDR_diabetes_chatbot_self_management_v1",
        },
        DemoRun {
            id: "demo-diabetes-all-sources",
            source: "all",
            requested_offset_min: 120,
            started_offset_min: 119,
            finished_offset_min: 102,
            found: 138,
            screened: 138,
            relevant: 24,
            fetched: 18,
            evaluated: 18,
            saved: 5,
            embedded: 0,
            skipped: 13,
            step: "gather completed · KG/wiki seeded",
        },
    ];

    for run in runs {
        conn.execute(
            "INSERT INTO job_runs
                (id, source, days_back, status, requested_at, started_at, finished_at,
                 candidates_found, candidates_screened, candidates_relevant, candidates_fetched,
                 candidates_evaluated, candidates_saved, candidates_embedded,
                 candidates_skipped, errors, current_item, current_step, error_message,
                 workspace_id)
             VALUES (?1, ?2, 3650, 'completed', ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10,
                     ?11, ?12, ?13, 0, ?14, ?15, NULL, ?16)",
            params![
                run.id,
                run.source,
                (now - chrono::Duration::minutes(run.requested_offset_min)).to_rfc3339(),
                (now - chrono::Duration::minutes(run.started_offset_min)).to_rfc3339(),
                (now - chrono::Duration::minutes(run.finished_offset_min)).to_rfc3339(),
                run.found,
                run.screened,
                run.relevant,
                run.fetched,
                run.evaluated,
                run.saved,
                run.embedded,
                run.skipped,
                SOURCE_PACKAGE,
                run.step,
                workspace_id,
            ],
        )?;
        for (event_type, payload) in [
            (
                "source_package_loaded",
                json!({"package": SOURCE_PACKAGE, "workspace": WORKSPACE_NAME}),
            ),
            (
                "cron_completed",
                json!({"source": run.source, "saved": run.saved, "status": "completed"}),
            ),
            (
                "kg_wiki_ready",
                json!({"nodes": 12, "edges": 10, "wiki_syntheses": 3}),
            ),
        ] {
            conn.execute(
                "INSERT INTO job_events (run_id, event_type, payload_json, created_at)
                 VALUES (?1, ?2, ?3, datetime('now'))",
                params![run.id, event_type, payload.to_string()],
            )?;
        }
    }
    Ok(())
}

fn demo_uids() -> Vec<&'static str> {
    vec![
        "demo-diabetes-chatbot-rct-2023",
        "demo-diabetes-agent-adherence-2022",
        "demo-diabetes-llm-counseling-2024",
        "demo-diabetes-hrqol-2021",
        "demo-diabetes-safety-escalation-2025",
    ]
}

fn entity_names() -> Vec<&'static str> {
    vec![
        "Type 2 diabetes",
        "Chatbot intervention",
        "Conversational agent",
        "LLM patient education",
        "Self-management education",
        "HbA1c",
        "Adherence",
        "HRQoL",
        "Safety escalation",
        "Misinformation handling",
        "Subgroup effects",
        "Evidence gap",
    ]
}

struct DemoArticle {
    uid: &'static str,
    category: &'static str,
    title: &'static str,
    first_author: &'static str,
    pub_date: &'static str,
    journal: &'static str,
    doi: &'static str,
    byline_summary: &'static str,
    why_it_matters: &'static str,
    main_findings: &'static str,
    limitations: &'static str,
    full_text: &'static str,
}

struct DemoRun {
    id: &'static str,
    source: &'static str,
    requested_offset_min: i64,
    started_offset_min: i64,
    finished_offset_min: i64,
    found: i32,
    screened: i32,
    relevant: i32,
    fetched: i32,
    evaluated: i32,
    saved: i32,
    embedded: i32,
    skipped: i32,
    step: &'static str,
}
