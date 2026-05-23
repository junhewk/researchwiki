use std::{
    env,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};

#[derive(Clone, Debug)]
pub struct AppConfig {
    pub storage: StorageConfig,
    pub llm: LlmConfig,
    pub embedding: EmbeddingConfig,
    pub embedding_dimensions: u32,
    /// Contact email sent to polite-pool APIs (OpenAlex/Crossref) and Unpaywall.
    /// Empty when the user has not provided one — callers must then omit it
    /// rather than impersonate anyone.
    pub contact_email: String,
}

#[derive(Clone, Debug)]
pub struct StorageConfig {
    pub database_path: PathBuf,
    pub prompts_dir: PathBuf,
    pub settings_file: PathBuf,
    pub wiki_export_dir: PathBuf,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct LlmConfig {
    pub base_url: String,
    pub model: String,
    #[serde(default)]
    pub api_key: String,
    #[serde(default = "default_true")]
    pub disable_thinking: bool,
    #[serde(default = "default_connect_timeout")]
    pub connect_timeout_seconds: u64,
    #[serde(default = "default_request_timeout")]
    pub request_timeout_seconds: u64,
    #[serde(default = "default_max_attempts")]
    pub max_attempts: usize,
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent_requests: usize,
}

fn default_true() -> bool {
    true
}
fn default_connect_timeout() -> u64 {
    10
}
fn default_request_timeout() -> u64 {
    300
}
fn default_max_attempts() -> usize {
    4
}
fn default_max_concurrent() -> usize {
    1
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            base_url: String::new(),
            model: String::new(),
            api_key: String::new(),
            disable_thinking: default_true(),
            connect_timeout_seconds: default_connect_timeout(),
            request_timeout_seconds: default_request_timeout(),
            max_attempts: default_max_attempts(),
            max_concurrent_requests: default_max_concurrent(),
        }
    }
}

impl LlmConfig {
    pub fn is_configured(&self) -> bool {
        !self.base_url.is_empty() && !self.model.is_empty()
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct EmbeddingConfig {
    pub base_url: String,
    pub model: String,
    #[serde(default)]
    pub api_key: String,
}

impl Default for EmbeddingConfig {
    fn default() -> Self {
        Self {
            base_url: "https://api.openai.com/v1".to_string(),
            model: "text-embedding-3-small".to_string(),
            api_key: String::new(),
        }
    }
}

impl EmbeddingConfig {
    pub fn is_configured(&self) -> bool {
        !self.base_url.is_empty() && !self.model.is_empty()
    }
}

impl AppConfig {
    /// Per-user data directory via `directories::ProjectDirs`.
    pub fn for_desktop() -> Result<Self> {
        let root = default_data_root()?;
        std::fs::create_dir_all(&root)
            .with_context(|| format!("failed to create data directory at {root:?}"))?;

        Ok(Self {
            storage: StorageConfig {
                database_path: root.join("haie.db"),
                prompts_dir: root.join("prompts"),
                settings_file: root.join("settings.json"),
                wiki_export_dir: root.join("wiki"),
            },
            llm: LlmConfig::default(),
            embedding: EmbeddingConfig::default(),
            embedding_dimensions: 1536,
            contact_email: String::new(),
        })
    }

    /// Overlay `.env` / process env on top of the desktop defaults.
    pub fn from_env() -> Result<Self> {
        let base = Self::for_desktop()?;

        let storage = StorageConfig {
            database_path: env::var("DATABASE_URL")
                .ok()
                .and_then(|value| parse_sqlite_database_path(&value))
                .or_else(|| env_path("DATABASE_PATH"))
                .unwrap_or(base.storage.database_path),
            prompts_dir: env_path("PROMPTS_DIR").unwrap_or(base.storage.prompts_dir),
            settings_file: env_path("SETTINGS_FILE").unwrap_or(base.storage.settings_file),
            wiki_export_dir: env_path("WIKI_EXPORT_DIR").unwrap_or(base.storage.wiki_export_dir),
        };

        let llm = LlmConfig {
            base_url: env::var("LLM_BASE_URL")
                .map(|v| v.trim_end_matches('/').to_string())
                .unwrap_or(base.llm.base_url),
            model: env::var("LLM_MODEL").unwrap_or(base.llm.model),
            api_key: env::var("LLM_API_KEY")
                .map(normalize_api_key)
                .unwrap_or(base.llm.api_key),
            disable_thinking: env_bool("LLM_DISABLE_THINKING", base.llm.disable_thinking),
            connect_timeout_seconds: env_parse(
                "LLM_CONNECT_TIMEOUT_SECONDS",
                base.llm.connect_timeout_seconds,
            )
            .clamp(1, 120),
            request_timeout_seconds: env_parse(
                "LLM_REQUEST_TIMEOUT_SECONDS",
                base.llm.request_timeout_seconds,
            )
            .clamp(10, 900),
            max_attempts: env_parse("LLM_MAX_ATTEMPTS", base.llm.max_attempts).clamp(1, 5),
            max_concurrent_requests: env_parse(
                "LLM_MAX_CONCURRENT_REQUESTS",
                base.llm.max_concurrent_requests,
            )
            .clamp(1, 16),
        };

        let embedding = EmbeddingConfig {
            base_url: env::var("EMBEDDING_BASE_URL")
                .map(|v| v.trim_end_matches('/').to_string())
                .unwrap_or(base.embedding.base_url),
            model: env::var("EMBEDDING_MODEL").unwrap_or(base.embedding.model),
            // OPENAI_API_KEY kept as a fallback so existing setups don't
            // break — embedding endpoint defaults to OpenAI anyway.
            api_key: env::var("EMBEDDING_API_KEY")
                .or_else(|_| env::var("OPENAI_API_KEY"))
                .map(normalize_api_key)
                .unwrap_or(base.embedding.api_key),
        };

        let embedding_dimensions = env::var("EMBEDDING_DIMENSIONS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(base.embedding_dimensions);

        // RESEARCHWIKI_CONTACT_EMAIL is preferred; UNPAYWALL_EMAIL stays as a
        // legacy fallback so existing setups keep working.
        let contact_email = env::var("RESEARCHWIKI_CONTACT_EMAIL")
            .or_else(|_| env::var("UNPAYWALL_EMAIL"))
            .map(|value| value.trim().to_string())
            .unwrap_or(base.contact_email);

        Ok(Self {
            storage,
            llm,
            embedding,
            embedding_dimensions,
            contact_email,
        })
    }

    /// The contact email as `Some` only when non-empty, so callers can cleanly
    /// skip features that require one (e.g. Unpaywall) instead of sending a
    /// placeholder address.
    pub fn contact_email_opt(&self) -> Option<String> {
        let trimmed = self.contact_email.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    }

    /// First configuration problem that would stop the app from working, or
    /// `None` when the LLM + embedding endpoints are usable. Drives the startup
    /// decision between the setup wizard and the main UI, so an endpoint that is
    /// present but malformed routes to setup instead of failing mid-job.
    pub fn validation_error(&self) -> Option<String> {
        validate_endpoint("LLM", &self.llm.base_url, &self.llm.model).or_else(|| {
            validate_endpoint("Embedding", &self.embedding.base_url, &self.embedding.model)
        })
    }

    /// Whether the LLM + embedding configuration is complete and well-formed.
    pub fn is_ready(&self) -> bool {
        self.validation_error().is_none()
    }
}

fn validate_endpoint(label: &str, base_url: &str, model: &str) -> Option<String> {
    let base_url = base_url.trim();
    if base_url.is_empty() {
        return Some(format!("{label} endpoint URL is not set."));
    }
    if !(base_url.starts_with("http://") || base_url.starts_with("https://")) {
        return Some(format!(
            "{label} endpoint URL must start with http:// or https://."
        ));
    }
    if model.trim().is_empty() {
        return Some(format!("{label} model name is not set."));
    }
    None
}

pub fn normalize_api_key(value: impl AsRef<str>) -> String {
    value
        .as_ref()
        .chars()
        .filter(|ch| !ch.is_whitespace())
        .collect()
}

fn default_data_root() -> Result<PathBuf> {
    if let Some(dirs) = directories::ProjectDirs::from("com", "ResearchWiki", "ResearchWiki") {
        return Ok(dirs.data_dir().to_owned());
    }
    // Fallback for unusual environments where ProjectDirs can't resolve.
    let exe = env::current_exe().context("failed to read current executable path")?;
    let parent = exe
        .parent()
        .context("current executable has no parent directory")?;
    Ok(parent.join("data"))
}

fn env_path(key: &str) -> Option<PathBuf> {
    env::var_os(key).map(PathBuf::from)
}

fn env_bool(key: &str, default: bool) -> bool {
    match env::var(key) {
        Ok(value) => match value.to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => true,
            "0" | "false" | "no" | "off" => false,
            _ => default,
        },
        Err(_) => default,
    }
}

fn env_parse<T: std::str::FromStr>(key: &str, default: T) -> T {
    env::var(key)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn parse_sqlite_database_path(value: &str) -> Option<PathBuf> {
    const PREFIXES: [&str; 2] = ["sqlite+aiosqlite:///", "sqlite:///"];

    PREFIXES
        .iter()
        .find_map(|prefix| value.strip_prefix(prefix))
        .map(Path::new)
        .map(Path::to_path_buf)
}
