use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use tracing::warn;

use crate::{
    error::{AppError, run_blocking_db},
    models::prompt::{
        ModelConfigCategory, ModelConfigEntry, ModelConfigUpdate, ModelConfigsUpdateRequest,
        PromptCreate, PromptFileConfig, PromptResponse, PromptVersionResponse, SchemaFieldResponse,
        SchemaResponse,
    },
};

#[derive(Clone)]
pub struct PromptService {
    prompts_dir: Arc<PathBuf>,
    database_path: Arc<PathBuf>,
}

impl PromptService {
    pub fn new(prompts_dir: PathBuf, database_path: PathBuf) -> Self {
        Self {
            prompts_dir: Arc::new(prompts_dir),
            database_path: Arc::new(database_path),
        }
    }

    pub async fn prompt_count(&self) -> Result<usize, AppError> {
        Ok(self.read_prompt_files().await?.len())
    }

    pub async fn seed_prompt_versions(&self) -> Result<(), AppError> {
        let files = self.read_prompt_files().await?;
        let database_path = self.database_path.clone();

        run_blocking_db(move || {
            let conn = crate::db::open_connection(&*database_path)?;

            for file in files {
                let version_exists: Option<i64> = conn
                    .query_row(
                        "SELECT version FROM prompt_versions WHERE prompt_name = ?1 LIMIT 1",
                        [file.name.as_str()],
                        |row| row.get(0),
                    )
                    .optional()?;

                if version_exists.is_none() {
                    insert_prompt_version(
                        &conn,
                        &file.name,
                        1,
                        &file.content,
                        file.config.model.as_deref(),
                        file.config.temperature,
                        Some("Initial Rust backend import"),
                        Some("system"),
                    )?;
                }
            }

            Ok(())
        })
        .await
    }

    pub async fn list_prompts(&self) -> Result<Vec<PromptResponse>, AppError> {
        let files = self.read_prompt_files().await?;
        let database_path = self.database_path.clone();

        run_blocking_db(move || {
            let conn = crate::db::open_connection(&*database_path)?;
            let mut prompts = Vec::with_capacity(files.len());

            for file in files {
                let stats = prompt_stats(&conn, &file.name)?;
                prompts.push(PromptResponse {
                    name: file.name,
                    content: file.content,
                    model: file.config.model,
                    temperature: file.config.temperature,
                    current_version: stats.current_version,
                    execution_count: stats.execution_count,
                    last_executed: stats.last_executed,
                });
            }

            prompts.sort_by(|a, b| a.name.cmp(&b.name));
            Ok(prompts)
        })
        .await
    }

    pub async fn get_prompt(&self, name: &str) -> Result<PromptResponse, AppError> {
        let file = self.read_prompt_file(name).await?;
        let database_path = self.database_path.clone();
        let name = name.to_string();

        run_blocking_db(move || {
            let conn = crate::db::open_connection(&*database_path)?;
            let stats = prompt_stats(&conn, &name)?;

            Ok(PromptResponse {
                name,
                content: file.content,
                model: file.config.model,
                temperature: file.config.temperature,
                current_version: stats.current_version,
                execution_count: stats.execution_count,
                last_executed: stats.last_executed,
            })
        })
        .await
    }

    pub async fn get_prompt_config(&self, name: &str) -> Result<PromptFileConfig, AppError> {
        Ok(self.read_prompt_file(name).await?.config)
    }

    pub async fn update_prompt(
        &self,
        name: &str,
        request: PromptCreate,
    ) -> Result<PromptResponse, AppError> {
        let path = self.prompt_path(name);
        if !path.exists() {
            return Err(AppError::NotFound(format!("Prompt '{name}' not found")));
        }

        let parsed: PromptFileConfig = serde_yaml::from_str(&request.content)?;
        tokio::fs::write(&path, &request.content).await?;

        let database_path = self.database_path.clone();
        let name = name.to_string();
        let name_for_write = name.clone();
        let content = request.content.clone();
        let description = request.description.clone();
        let model = parsed.model.clone();
        let temperature = parsed.temperature;

        run_blocking_db(move || {
            let conn = crate::db::open_connection(&*database_path)?;
            let next_version: i64 = conn.query_row(
                "SELECT COALESCE(MAX(version), 0) + 1 FROM prompt_versions WHERE prompt_name = ?1",
                [name_for_write.as_str()],
                |row| row.get(0),
            )?;

            insert_prompt_version(
                &conn,
                &name_for_write,
                next_version,
                &content,
                model.as_deref(),
                temperature,
                description.as_deref(),
                Some("system"),
            )?;

            Ok(())
        })
        .await?;

        self.get_prompt(&name).await
    }

    pub async fn list_versions(&self, name: &str) -> Result<Vec<PromptVersionResponse>, AppError> {
        let database_path = self.database_path.clone();
        let name = name.to_string();

        run_blocking_db(move || {
            let conn = crate::db::open_connection(&*database_path)?;
            let mut stmt = conn.prepare(
                "SELECT id, prompt_name, version, content, model, temperature, description,
                        changed_by, created_at
                 FROM prompt_versions
                 WHERE prompt_name = ?1
                 ORDER BY version DESC",
            )?;

            let rows = stmt.query_map([name.as_str()], |row| {
                let created_at_raw: String = row.get(8)?;
                Ok(PromptVersionResponse {
                    id: row.get(0)?,
                    prompt_name: row.get(1)?,
                    version: row.get(2)?,
                    content: row.get(3)?,
                    model: row.get(4)?,
                    temperature: row.get(5)?,
                    description: row.get(6)?,
                    changed_by: row.get(7)?,
                    created_at: parse_sqlite_datetime(&created_at_raw),
                })
            })?;

            rows.collect::<Result<Vec<_>, _>>()
        })
        .await
    }

    pub async fn model_configs(&self) -> Result<Vec<ModelConfigCategory>, AppError> {
        let files = self.read_prompt_files().await?;

        let categories = MODEL_CONFIG_REGISTRY
            .iter()
            .map(|category| {
                let configs = category
                    .configs
                    .iter()
                    .map(|item| {
                        let file = files.iter().find(|file| file.name == item.prompt_name);
                        ModelConfigEntry {
                            prompt_name: item.prompt_name.to_string(),
                            label: item.label.to_string(),
                            model: file
                                .and_then(|file| file.config.model.clone())
                                .unwrap_or_else(|| "qwen3.6-27b-q8".to_string()),
                            temperature: file
                                .and_then(|file| file.config.temperature)
                                .unwrap_or(0.5),
                        }
                    })
                    .collect();

                ModelConfigCategory {
                    category: category.category.to_string(),
                    label: category.label.to_string(),
                    configs,
                }
            })
            .collect();

        Ok(categories)
    }

    pub async fn update_model_configs(
        &self,
        request: ModelConfigsUpdateRequest,
    ) -> Result<(), AppError> {
        for update in request.updates {
            self.update_single_model_config(update).await?;
        }

        Ok(())
    }

    async fn update_single_model_config(&self, update: ModelConfigUpdate) -> Result<(), AppError> {
        let path = self.prompt_path(&update.prompt_name);
        if !path.exists() {
            return Err(AppError::NotFound(format!(
                "Prompt file '{}.yaml' not found",
                update.prompt_name
            )));
        }

        let raw = tokio::fs::read_to_string(&path).await?;
        let mut value: serde_yaml::Value = serde_yaml::from_str(&raw)?;
        let model = update.model.clone();
        let temperature = update.temperature;
        let mapping = value.as_mapping_mut().ok_or_else(|| {
            AppError::BadRequest(format!(
                "Prompt '{}' is not a YAML mapping",
                update.prompt_name
            ))
        })?;

        mapping.insert(
            serde_yaml::Value::String("model".to_string()),
            serde_yaml::Value::String(model.clone()),
        );
        mapping.insert(
            serde_yaml::Value::String("temperature".to_string()),
            serde_yaml::Value::Number(serde_yaml::Number::from(temperature)),
        );

        let rendered =
            serde_yaml::to_string(&value).map_err(|error| AppError::Internal(error.to_string()))?;
        tokio::fs::write(&path, &rendered).await?;

        let database_path = self.database_path.clone();
        let prompt_name = update.prompt_name.clone();

        run_blocking_db(move || {
            let conn = crate::db::open_connection(&*database_path)?;
            let next_version: i64 = conn.query_row(
                "SELECT COALESCE(MAX(version), 0) + 1 FROM prompt_versions WHERE prompt_name = ?1",
                [prompt_name.as_str()],
                |row| row.get(0),
            )?;

            insert_prompt_version(
                &conn,
                &prompt_name,
                next_version,
                &rendered,
                Some(model.as_str()),
                Some(temperature),
                Some("Model config update"),
                Some("system"),
            )?;

            Ok(())
        })
        .await?;

        Ok(())
    }

    pub async fn get_prompt_version(&self, name: &str) -> Result<i64, AppError> {
        let database_path = self.database_path.clone();
        let name = name.to_string();

        run_blocking_db(move || {
            let conn = crate::db::open_connection(&*database_path)?;
            conn.query_row(
                "SELECT COALESCE(MAX(version), 1) FROM prompt_versions WHERE prompt_name = ?1",
                [name.as_str()],
                |row| row.get(0),
            )
        })
        .await
    }

    pub async fn render_prompt(
        &self,
        name: &str,
        variables: &BTreeMap<String, String>,
    ) -> Result<String, AppError> {
        let file = self.read_prompt_file(name).await?;
        let template = file.config.user.ok_or_else(|| {
            AppError::BadRequest(format!("Prompt '{name}' does not define a user template"))
        })?;

        render_template(&template, variables)
    }

    pub fn list_structured_output_models(&self) -> Vec<String> {
        let mut items: Vec<_> = structured_output_schemas()
            .iter()
            .map(|schema| schema.name.to_string())
            .collect();
        items.sort();
        items
    }

    pub fn get_structured_output_schema(&self, model_name: &str) -> Option<SchemaResponse> {
        structured_output_schemas()
            .iter()
            .find(|schema| schema.name == model_name)
            .cloned()
    }

    async fn read_prompt_files(&self) -> Result<Vec<PromptFile>, AppError> {
        let mut entries = tokio::fs::read_dir(&*self.prompts_dir).await?;
        let mut prompts = Vec::new();

        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("yaml") {
                continue;
            }

            let Some(stem) = path.file_stem().and_then(|value| value.to_str()) else {
                warn!(
                    "skipping prompt file with invalid UTF-8 path: {}",
                    path.display()
                );
                continue;
            };

            let content = tokio::fs::read_to_string(&path).await?;
            let config: PromptFileConfig = serde_yaml::from_str(&content).unwrap_or_else(|error| {
                warn!("failed to parse prompt file {}: {}", path.display(), error);
                PromptFileConfig::default()
            });

            prompts.push(PromptFile {
                name: stem.to_string(),
                content,
                config,
            });
        }

        Ok(prompts)
    }

    async fn read_prompt_file(&self, name: &str) -> Result<PromptFile, AppError> {
        let path = self.prompt_path(name);
        if !path.exists() {
            return Err(AppError::NotFound(format!("Prompt '{name}' not found")));
        }

        let content = tokio::fs::read_to_string(&path).await?;
        let config: PromptFileConfig = serde_yaml::from_str(&content).unwrap_or_default();

        Ok(PromptFile {
            name: name.to_string(),
            content,
            config,
        })
    }

    fn prompt_path(&self, name: &str) -> PathBuf {
        self.prompts_dir.join(format!("{name}.yaml"))
    }
}

#[derive(Clone)]
struct PromptFile {
    name: String,
    content: String,
    config: PromptFileConfig,
}

#[derive(Default)]
struct PromptStats {
    current_version: i64,
    execution_count: i64,
    last_executed: Option<DateTime<Utc>>,
}

fn prompt_stats(conn: &Connection, name: &str) -> Result<PromptStats, rusqlite::Error> {
    let current_version = conn.query_row(
        "SELECT COALESCE(MAX(version), 1) FROM prompt_versions WHERE prompt_name = ?1",
        [name],
        |row| row.get(0),
    )?;

    let execution_count = conn.query_row(
        "SELECT COUNT(*) FROM prompt_traces WHERE prompt_name = ?1",
        [name],
        |row| row.get(0),
    )?;

    let last_executed = conn
        .query_row(
            "SELECT created_at FROM prompt_traces WHERE prompt_name = ?1 ORDER BY created_at DESC LIMIT 1",
            [name],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|value| parse_sqlite_datetime(&value));

    Ok(PromptStats {
        current_version,
        execution_count,
        last_executed,
    })
}

fn parse_sqlite_datetime(value: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(value)
        .map(|dt| dt.with_timezone(&Utc))
        .or_else(|_| {
            chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S")
                .map(|dt| DateTime::from_naive_utc_and_offset(dt, Utc))
        })
        .unwrap_or_else(|_| Utc::now())
}

fn render_template(
    template: &str,
    variables: &BTreeMap<String, String>,
) -> Result<String, AppError> {
    let mut rendered = String::with_capacity(template.len());
    let mut chars = template.chars().peekable();

    while let Some(ch) = chars.next() {
        match ch {
            '{' => {
                if chars.peek() == Some(&'{') {
                    chars.next();
                    rendered.push('{');
                    continue;
                }

                let mut key = String::new();
                let mut closed = false;
                for next in chars.by_ref() {
                    if next == '}' {
                        closed = true;
                        break;
                    }
                    key.push(next);
                }

                if !closed {
                    return Err(AppError::BadRequest(
                        "Prompt template contains an unclosed variable placeholder".to_string(),
                    ));
                }

                let value = variables.get(&key).ok_or_else(|| {
                    AppError::BadRequest(format!("Missing variable '{key}' for prompt template"))
                })?;
                rendered.push_str(value);
            }
            '}' => {
                if chars.peek() == Some(&'}') {
                    chars.next();
                    rendered.push('}');
                } else {
                    return Err(AppError::BadRequest(
                        "Prompt template contains an unmatched '}'".to_string(),
                    ));
                }
            }
            _ => rendered.push(ch),
        }
    }

    Ok(rendered)
}

fn insert_prompt_version(
    conn: &Connection,
    prompt_name: &str,
    version: i64,
    content: &str,
    model: Option<&str>,
    temperature: Option<f64>,
    description: Option<&str>,
    changed_by: Option<&str>,
) -> Result<(), rusqlite::Error> {
    conn.execute(
        "INSERT INTO prompt_versions (
            prompt_name, version, content, model, temperature, description, changed_by
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            prompt_name,
            version,
            content,
            model,
            temperature,
            description,
            changed_by,
        ],
    )?;

    Ok(())
}

struct CategoryRegistry {
    category: &'static str,
    label: &'static str,
    configs: &'static [ConfigRegistry],
}

struct ConfigRegistry {
    prompt_name: &'static str,
    label: &'static str,
}

const MODEL_CONFIG_REGISTRY: &[CategoryRegistry] = &[
    CategoryRegistry {
        category: "article_pipeline",
        label: "Article Pipeline",
        configs: &[
            ConfigRegistry {
                prompt_name: "relevancy_filter",
                label: "Relevancy Filter (Quick Check)",
            },
            ConfigRegistry {
                prompt_name: "full_evaluation",
                label: "Full Evaluation (Detailed)",
            },
        ],
    },
    CategoryRegistry {
        category: "knowledge_graph",
        label: "Knowledge Graph",
        configs: &[
            ConfigRegistry {
                prompt_name: "entity_extraction",
                label: "Entity Extraction",
            },
            ConfigRegistry {
                prompt_name: "entity_verification",
                label: "Entity Verification",
            },
            ConfigRegistry {
                prompt_name: "entity_synthesis",
                label: "Entity Synthesis",
            },
            ConfigRegistry {
                prompt_name: "relationship_evidence",
                label: "Relationship Evidence",
            },
            ConfigRegistry {
                prompt_name: "kg_gap_analysis",
                label: "Gap Analysis",
            },
        ],
    },
    CategoryRegistry {
        category: "newsletter",
        label: "Newsletter Generation",
        configs: &[
            ConfigRegistry {
                prompt_name: "newsletter_introduction",
                label: "Introduction",
            },
            ConfigRegistry {
                prompt_name: "newsletter_title",
                label: "Title Generation",
            },
            ConfigRegistry {
                prompt_name: "newsletter_title_rephrase",
                label: "Title Rephrase (Korean)",
            },
            ConfigRegistry {
                prompt_name: "newsletter_highlights",
                label: "Highlights",
            },
            ConfigRegistry {
                prompt_name: "newsletter_closing",
                label: "Closing Remarks",
            },
        ],
    },
    CategoryRegistry {
        category: "library_search",
        label: "Library Search",
        configs: &[
            ConfigRegistry {
                prompt_name: "hyde_expansion",
                label: "HyDE Query Expansion",
            },
            ConfigRegistry {
                prompt_name: "multi_query_expansion",
                label: "Multi-Query Expansion",
            },
        ],
    },
];

fn structured_output_schemas() -> Vec<SchemaResponse> {
    vec![
        SchemaResponse {
            name: "ArticleEvaluation".to_string(),
            description: "Structured output schema for full article evaluation.\n\nUsed with LangChain's with_structured_output() for reliable parsing.".to_string(),
            fields: vec![
                schema_string("title", "Article title", false, Some(serde_json::Value::String(String::new()))),
                schema_string("pub_date", "Publication date in yyyy-MM-dd format", false, Some(serde_json::Value::String(String::new()))),
                schema_string("journal", "Journal name", false, Some(serde_json::Value::String(String::new()))),
                schema_literal("ai_tech", "AI technology type", false, Some("Other"), &["ML", "DL", "LLM", "Clinical Decision Support", "Diagnostic AI", "Other"]),
                schema_literal("clinical_domain", "Clinical domain", false, Some("General"), &["Radiology", "Pathology", "Primary Care", "ICU", "Multi-domain", "General"]),
                schema_literal("ethics_framework", "Ethics framework used in the paper", false, Some("명시 없음"), &["원칙주의", "덕윤리", "돌봄윤리", "결과주의", "권리 기반 접근", "다수 이론", "명시 없음"]),
                schema_literal("primary_issue", "Primary ethics issue addressed", false, Some("기타"), &["공정성", "프라이버시", "투명성", "설명책임", "자율성", "선행", "정의", "기타"]),
                schema_literal("key_stakeholders", "Key stakeholders discussed", false, Some("Other"), &["Patients", "Clinicians", "Developers", "Regulators", "Institutions", "Other"]),
                schema_literal("practical_impl", "Practical implementation domain", false, Some("기타"), &["정책", "임상 실무", "AI 개발", "규제", "기타"]),
                schema_string("secondary_issues", "2-3 other relevant ethics issues", false, Some(serde_json::Value::String(String::new()))),
                schema_string("key_argument", "Main thesis in one sentence", false, Some(serde_json::Value::String(String::new()))),
                schema_string("main_findings", "2-3 key findings/claims, line-separated", false, Some(serde_json::Value::String(String::new()))),
                schema_string("normative_claims", "Should/ought statements from the paper", false, Some(serde_json::Value::String(String::new()))),
                schema_string("limitations", "Author-stated limitations", false, Some(serde_json::Value::String(String::new()))),
                schema_string("theoretical_strengths", "Strong theoretical contributions", false, Some(serde_json::Value::String(String::new()))),
                schema_string("theoretical_weaknesses", "Theoretical gaps or problems", false, Some(serde_json::Value::String(String::new()))),
                schema_string("empirical_strengths", "Strong empirical aspects (if applicable)", false, Some(serde_json::Value::String(String::new()))),
                schema_string("empirical_weaknesses", "Empirical limitations (if applicable)", false, Some(serde_json::Value::String(String::new()))),
                schema_string("byline_summary", "3-4 sentence elevator pitch summary", false, Some(serde_json::Value::String(String::new()))),
                schema_string("why_it_matters", "Why researchers in the current collection focus should read this (3-4 sentences)", false, Some(serde_json::Value::String(String::new()))),
                schema_number("scholarly_rigor", "integer", "0-5: 5=methodologically/theoretically strong, 0=problematic reasoning", true, Some(0.0), Some(0.0), Some(5.0)),
                schema_number("novelty", "integer", "0-5: 5=major new contribution, 0=no clear novelty", true, Some(0.0), Some(0.0), Some(5.0)),
                schema_number("relevance_score", "integer", "0-5: 5=central to current collection focus, 0=not relevant", true, Some(0.0), Some(0.0), Some(5.0)),
                schema_number("practical_impact", "integer", "0-5: 5=clear actionable implications, 0=unclear implications", true, Some(0.0), Some(0.0), Some(5.0)),
                schema_number("interdisciplinary", "integer", "0-4: 4=strongly integrates multiple relevant disciplines, 0=single narrow lens", true, Some(0.0), Some(0.0), Some(4.0)),
                schema_number("critical_concerns", "integer", "-5 to 0: 0=no major concerns, -5=serious validity/safety/methodological concern", true, Some(0.0), Some(-5.0), Some(0.0)),
                schema_number("total_score", "integer", "Normalized 0-100 research-value score; recomputed by the app from component scores", false, Some(0.0), Some(0.0), Some(100.0)),
                schema_literal("priority", "Tier1 if total_score >= 75, Tier2 if 40-74, Tier3 if 0-39; recomputed by the app", false, None, &["Tier1", "Tier2", "Tier3"]),
            ],
        },
        SchemaResponse {
            name: "ChunkExtraction".to_string(),
            description: "Complete extraction result from a text chunk.".to_string(),
            fields: vec![
                SchemaFieldResponse {
                    name: "entities".to_string(),
                    r#type: "string".to_string(),
                    description: "List of entities extracted from the text (target: 10-30 per chunk)".to_string(),
                    required: false,
                    default: Some(serde_json::json!([])),
                    options: None,
                    min: None,
                    max: None,
                },
                SchemaFieldResponse {
                    name: "relationships".to_string(),
                    r#type: "string".to_string(),
                    description: "List of relationships between extracted entities".to_string(),
                    required: false,
                    default: Some(serde_json::json!([])),
                    options: None,
                    min: None,
                    max: None,
                },
            ],
        },
        SchemaResponse {
            name: "EntityVerification".to_string(),
            description: "Result of LLM verification for entity resolution.".to_string(),
            fields: vec![
                SchemaFieldResponse {
                    name: "same_entity".to_string(),
                    r#type: "string".to_string(),
                    description: "Whether the two entity mentions refer to the same real-world entity".to_string(),
                    required: true,
                    default: None,
                    options: None,
                    min: None,
                    max: None,
                },
                schema_number("confidence", "float", "Confidence score for the decision", true, None, Some(0.0), Some(1.0)),
                schema_string("reasoning", "Brief explanation for the decision", true, None),
            ],
        },
        SchemaResponse {
            name: "RelevancyFilter".to_string(),
            description: "Structured output for relevancy screening.\n\nUsed to quickly filter articles for healthcare AI ethics relevance.".to_string(),
            fields: vec![
                schema_literal("decision", "yes if article is relevant to healthcare AI ethics, no otherwise", true, None, &["yes", "no"]),
                schema_number("confidence", "float", "Confidence score between 0 and 1", false, Some(0.5), Some(0.0), Some(1.0)),
                schema_string("reasoning", "Brief explanation for the decision", false, Some(serde_json::Value::String(String::new()))),
            ],
        },
    ]
}

fn schema_string(
    name: &str,
    description: &str,
    required: bool,
    default: Option<serde_json::Value>,
) -> SchemaFieldResponse {
    SchemaFieldResponse {
        name: name.to_string(),
        r#type: "string".to_string(),
        description: description.to_string(),
        required,
        default,
        options: None,
        min: None,
        max: None,
    }
}

fn schema_literal(
    name: &str,
    description: &str,
    required: bool,
    default: Option<&str>,
    options: &[&str],
) -> SchemaFieldResponse {
    SchemaFieldResponse {
        name: name.to_string(),
        r#type: "literal".to_string(),
        description: description.to_string(),
        required,
        default: default.map(|value| serde_json::Value::String(value.to_string())),
        options: Some(options.iter().map(|value| (*value).to_string()).collect()),
        min: None,
        max: None,
    }
}

fn schema_number(
    name: &str,
    value_type: &str,
    description: &str,
    required: bool,
    default: Option<f64>,
    min: Option<f64>,
    max: Option<f64>,
) -> SchemaFieldResponse {
    SchemaFieldResponse {
        name: name.to_string(),
        r#type: value_type.to_string(),
        description: description.to_string(),
        required,
        default: default.map(serde_json::Value::from),
        options: None,
        min,
        max,
    }
}
