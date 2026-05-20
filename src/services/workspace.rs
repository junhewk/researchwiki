use std::{path::PathBuf, sync::Arc};

use rusqlite::{Connection, OptionalExtension, params};

use crate::{
    error::{AppError, run_blocking_db},
    models::workspace::{
        Workspace, WorkspaceCreate, WorkspaceResearchContext, WorkspaceSummary, WorkspaceUpdate,
    },
};

const WORKSPACE_COLUMNS: &str = "id, name, slug, db_filename, primary_question, gap_note, \
     refined_question, seed_concepts_json, override_queries_json, topic_descriptor, \
     lookback_days, is_active, created_at, updated_at";

/// Registry of workspaces. Lives in a single meta database; each workspace's
/// actual data lives in its own file under `workspaces_dir/<db_filename>`.
#[derive(Clone)]
pub struct WorkspaceService {
    meta_path: Arc<PathBuf>,
    workspaces_dir: Arc<PathBuf>,
}

impl WorkspaceService {
    pub fn new(meta_path: PathBuf, workspaces_dir: PathBuf) -> Self {
        Self {
            meta_path: Arc::new(meta_path),
            workspaces_dir: Arc::new(workspaces_dir),
        }
    }

    /// Absolute path to a workspace's data DB file.
    pub fn db_path_for(&self, db_filename: &str) -> PathBuf {
        self.workspaces_dir.join(db_filename)
    }

    pub async fn list(&self) -> Result<Vec<WorkspaceSummary>, AppError> {
        let meta_path = self.meta_path.clone();

        run_blocking_db(move || {
            let conn = crate::db::open_connection(&*meta_path)?;
            let mut stmt =
                conn.prepare("SELECT id, name, slug, is_active FROM workspaces ORDER BY id ASC")?;
            let rows = stmt.query_map([], |row| {
                Ok(WorkspaceSummary {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    slug: row.get(2)?,
                    is_active: row.get::<_, i64>(3)? != 0,
                })
            })?;
            rows.collect::<Result<Vec<_>, _>>()
        })
        .await
    }

    pub async fn get(&self, id: i64) -> Result<Workspace, AppError> {
        let meta_path = self.meta_path.clone();
        run_blocking_db(move || {
            let conn = crate::db::open_connection(&*meta_path)?;
            get_sync(&conn, id)
        })
        .await
    }

    /// The active workspace's id, falling back to the lowest id.
    pub async fn active_or_default_id(&self) -> Result<i64, AppError> {
        let meta_path = self.meta_path.clone();
        run_blocking_db(move || {
            let conn = crate::db::open_connection(&*meta_path)?;
            let active: Option<i64> = conn
                .query_row(
                    "SELECT id FROM workspaces WHERE is_active = 1 ORDER BY id LIMIT 1",
                    [],
                    |row| row.get(0),
                )
                .optional()?;
            if let Some(id) = active {
                return Ok(id);
            }
            conn.query_row("SELECT id FROM workspaces ORDER BY id LIMIT 1", [], |row| {
                row.get(0)
            })
        })
        .await
    }

    /// Canonical research context that drives gather, screening, KG extraction,
    /// wiki synthesis, and Gap Bridge framing.
    pub async fn research_context(&self, id: i64) -> Result<WorkspaceResearchContext, AppError> {
        self.get(id)
            .await
            .map(|workspace| WorkspaceResearchContext::from_workspace(&workspace))
    }

    pub async fn create(&self, request: WorkspaceCreate) -> Result<Workspace, AppError> {
        let meta_path = self.meta_path.clone();

        run_blocking_db(move || {
            let conn = crate::db::open_connection(&*meta_path)?;
            let slug = unique_slug(&conn, &slugify(&request.name))?;
            let db_filename = format!("ws_{slug}.db");
            let seed_json =
                serde_json::to_string(&request.seed_concepts).unwrap_or_else(|_| "[]".into());
            let override_json =
                serde_json::to_string(&request.override_queries).unwrap_or_else(|_| "[]".into());

            conn.execute(
                "INSERT INTO workspaces
                    (name, slug, db_filename, primary_question, gap_note, topic_descriptor,
                     seed_concepts_json, override_queries_json, lookback_days, is_active)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 0)",
                params![
                    request.name,
                    slug,
                    db_filename,
                    request.primary_question,
                    request.gap_note,
                    request.topic_descriptor,
                    seed_json,
                    override_json,
                    request.lookback_days.max(1),
                ],
            )?;

            let id = conn.last_insert_rowid();
            get_sync(&conn, id)
        })
        .await
    }

    pub async fn update(&self, id: i64, request: WorkspaceUpdate) -> Result<Workspace, AppError> {
        let meta_path = self.meta_path.clone();

        run_blocking_db(move || {
            let conn = crate::db::open_connection(&*meta_path)?;
            let mut current = get_sync(&conn, id)?;

            if let Some(value) = request.name {
                current.name = value;
            }
            if let Some(value) = request.primary_question {
                current.primary_question = value;
            }
            if let Some(value) = request.gap_note {
                current.gap_note = value;
            }
            if let Some(value) = request.refined_question {
                current.refined_question = value;
            }
            if let Some(value) = request.topic_descriptor {
                current.topic_descriptor = value;
            }
            if let Some(value) = request.seed_concepts {
                current.seed_concepts = value;
            }
            if let Some(value) = request.override_queries {
                current.override_queries = value;
            }
            if let Some(value) = request.lookback_days {
                current.lookback_days = value.max(1);
            }

            let seed_json =
                serde_json::to_string(&current.seed_concepts).unwrap_or_else(|_| "[]".into());
            let override_json =
                serde_json::to_string(&current.override_queries).unwrap_or_else(|_| "[]".into());

            conn.execute(
                "UPDATE workspaces SET
                    name = ?2, primary_question = ?3, gap_note = ?4, refined_question = ?5,
                    topic_descriptor = ?6, seed_concepts_json = ?7, override_queries_json = ?8,
                    lookback_days = ?9, updated_at = datetime('now')
                 WHERE id = ?1",
                params![
                    id,
                    current.name,
                    current.primary_question,
                    current.gap_note,
                    current.refined_question,
                    current.topic_descriptor,
                    seed_json,
                    override_json,
                    current.lookback_days,
                ],
            )?;

            get_sync(&conn, id)
        })
        .await
    }

    pub async fn set_active(&self, id: i64) -> Result<(), AppError> {
        let meta_path = self.meta_path.clone();
        run_blocking_db(move || {
            let conn = crate::db::open_connection(&*meta_path)?;
            conn.execute(
                "UPDATE workspaces SET is_active = CASE WHEN id = ?1 THEN 1 ELSE 0 END",
                [id],
            )?;
            Ok(())
        })
        .await
    }

    pub async fn set_refined_question(&self, id: i64, text: String) -> Result<(), AppError> {
        let meta_path = self.meta_path.clone();
        run_blocking_db(move || {
            let conn = crate::db::open_connection(&*meta_path)?;
            conn.execute(
                "UPDATE workspaces SET refined_question = ?2, updated_at = datetime('now') WHERE id = ?1",
                params![id, text],
            )?;
            Ok(())
        })
        .await
    }
}

fn get_sync(conn: &Connection, id: i64) -> Result<Workspace, rusqlite::Error> {
    let sql = format!("SELECT {WORKSPACE_COLUMNS} FROM workspaces WHERE id = ?1");
    conn.query_row(&sql, [id], map_workspace_row)
}

fn map_workspace_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Workspace> {
    let seed_concepts_json: String = row.get(7)?;
    let override_queries_json: String = row.get(8)?;
    Ok(Workspace {
        id: row.get(0)?,
        name: row.get(1)?,
        slug: row.get(2)?,
        db_filename: row.get(3)?,
        primary_question: row.get(4)?,
        gap_note: row.get(5)?,
        refined_question: row.get(6)?,
        seed_concepts: serde_json::from_str(&seed_concepts_json).unwrap_or_default(),
        override_queries: serde_json::from_str(&override_queries_json).unwrap_or_default(),
        topic_descriptor: row.get(9)?,
        lookback_days: row.get(10)?,
        is_active: row.get::<_, i64>(11)? != 0,
        created_at: row.get(12)?,
        updated_at: row.get(13)?,
    })
}

fn slugify(name: &str) -> String {
    let lowered: String = name
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect();
    let collapsed = lowered
        .split('-')
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    if collapsed.is_empty() {
        "workspace".to_string()
    } else {
        collapsed
    }
}

fn unique_slug(conn: &Connection, base: &str) -> Result<String, rusqlite::Error> {
    let mut candidate = base.to_string();
    let mut suffix = 2;
    loop {
        let exists: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM workspaces WHERE slug = ?1)",
            [candidate.as_str()],
            |row| row.get(0),
        )?;
        if !exists {
            return Ok(candidate);
        }
        candidate = format!("{base}-{suffix}");
        suffix += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn research_context_loads_full_registry_row() {
        let root = std::env::temp_dir().join(format!(
            "researchwiki-workspace-test-{}",
            uuid::Uuid::new_v4()
        ));
        let meta_path = root.join("meta.db");
        crate::db::initialize_meta(meta_path.clone(), "haie.db".to_string())
            .await
            .expect("meta init");
        let service = WorkspaceService::new(meta_path, root.clone());

        let workspace = service
            .create(WorkspaceCreate {
                name: "Diabetes chatbot evidence map".to_string(),
                primary_question: "Do chatbots improve self-management?".to_string(),
                gap_note: "Separate older chatbots from LLM counseling.".to_string(),
                topic_descriptor: "diabetes chatbot self-management".to_string(),
                seed_concepts: vec!["type 2 diabetes".to_string(), "HbA1c".to_string()],
                override_queries: vec!["diabetes chatbot HbA1c".to_string()],
                lookback_days: 365,
            })
            .await
            .expect("create workspace");

        let context = service
            .research_context(workspace.id)
            .await
            .expect("research context");

        assert_eq!(context.name, "Diabetes chatbot evidence map");
        assert_eq!(
            context.primary_question,
            "Do chatbots improve self-management?"
        );
        assert_eq!(
            context.gap_note,
            "Separate older chatbots from LLM counseling."
        );
        assert_eq!(context.refined_question, "");
        assert_eq!(
            context.seed_concepts,
            vec!["type 2 diabetes".to_string(), "HbA1c".to_string()]
        );
        assert_eq!(
            context.override_queries,
            vec!["diabetes chatbot HbA1c".to_string()]
        );
        assert_eq!(context.topic_descriptor, "diabetes chatbot self-management");
        assert_eq!(context.lookback_days, 365);

        let _ = std::fs::remove_dir_all(root);
    }
}
