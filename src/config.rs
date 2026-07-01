use std::{
    env,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};

const CUSTOM_MODEL_LABEL: &str = "Custom...";

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
    /// Semantic Scholar API key. Empty disables that source (its keyless pool is
    /// rate-limited to the point of being unusable).
    pub semantic_scholar_api_key: String,
}

#[derive(Clone, Debug)]
pub struct StorageConfig {
    pub database_path: PathBuf,
    pub prompts_dir: PathBuf,
    pub settings_file: PathBuf,
    pub wiki_export_dir: PathBuf,
    /// Where fetched article PDFs are persisted for re-extraction.
    pub pdf_dir: PathBuf,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct LlmConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<LlmProvider>,
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
            provider: Some(LlmProvider::Openai),
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

    pub fn effective_provider(&self) -> LlmProvider {
        self.provider.unwrap_or_else(|| {
            infer_llm_provider(&self.base_url).unwrap_or(LlmProvider::CustomOpenaiCompatible)
        })
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct EmbeddingConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<EmbeddingProvider>,
    pub base_url: String,
    pub model: String,
    #[serde(default)]
    pub api_key: String,
}

impl Default for EmbeddingConfig {
    fn default() -> Self {
        Self {
            provider: Some(EmbeddingProvider::Openai),
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

    pub fn effective_provider(&self) -> EmbeddingProvider {
        self.provider.unwrap_or_else(|| {
            infer_embedding_provider(&self.base_url)
                .unwrap_or(EmbeddingProvider::CustomOpenaiCompatible)
        })
    }
}

#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum LlmProvider {
    Openai,
    Anthropic,
    Gemini,
    Openrouter,
    Ollama,
    LmStudio,
    LlamaServer,
    CustomOpenaiCompatible,
}

impl LlmProvider {
    pub const ALL: [Self; 8] = [
        Self::Openai,
        Self::Anthropic,
        Self::Gemini,
        Self::Openrouter,
        Self::Ollama,
        Self::LmStudio,
        Self::LlamaServer,
        Self::CustomOpenaiCompatible,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Openai => "OpenAI",
            Self::Anthropic => "Anthropic",
            Self::Gemini => "Gemini",
            Self::Openrouter => "OpenRouter",
            Self::Ollama => "Ollama",
            Self::LmStudio => "LM Studio",
            Self::LlamaServer => "llama-server",
            Self::CustomOpenaiCompatible => "Custom OpenAI-compatible",
        }
    }

    pub fn default_base_url(self) -> &'static str {
        match self {
            Self::Openai => "https://api.openai.com/v1",
            Self::Anthropic => "https://api.anthropic.com/v1",
            Self::Gemini => "https://generativelanguage.googleapis.com/v1beta/openai",
            Self::Openrouter => "https://openrouter.ai/api/v1",
            Self::Ollama => "http://localhost:11434/v1",
            Self::LmStudio => "http://localhost:1234/v1",
            Self::LlamaServer => "http://localhost:8080/v1",
            Self::CustomOpenaiCompatible => "",
        }
    }

    pub fn default_model(self) -> &'static str {
        match self {
            Self::Openai => "gpt-5.5",
            Self::Anthropic => "claude-sonnet-5",
            Self::Gemini => "gemini-3.5-flash",
            Self::Openrouter => "~openai/gpt-latest",
            Self::Ollama => "gpt-oss:20b",
            Self::LmStudio | Self::LlamaServer | Self::CustomOpenaiCompatible => "",
        }
    }

    pub fn model_presets(self) -> &'static [&'static str] {
        match self {
            Self::Openai => &["gpt-5.5", "gpt-5.5-mini", "gpt-5.5-nano"],
            Self::Anthropic => &["claude-opus-5", "claude-sonnet-5", "claude-haiku-5"],
            Self::Gemini => &[
                "gemini-3.5-pro",
                "gemini-3.5-flash",
                "gemini-3.5-flash-lite",
            ],
            Self::Openrouter => &[
                "~openai/gpt-latest",
                "openai/gpt-5.5",
                "anthropic/claude-sonnet-5",
                "google/gemini-3.5-pro",
            ],
            Self::Ollama => &["gpt-oss:20b", "gpt-oss:120b", "qwen3:8b", "llama3.3:70b"],
            Self::LmStudio | Self::LlamaServer | Self::CustomOpenaiCompatible => &[],
        }
    }

    pub fn uses_native_anthropic_api(self) -> bool {
        self == Self::Anthropic
    }
}

impl Default for LlmProvider {
    fn default() -> Self {
        Self::Openai
    }
}

impl std::str::FromStr for LlmProvider {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match normalize_provider_key(value).as_str() {
            "openai" => Ok(Self::Openai),
            "anthropic" | "claude" => Ok(Self::Anthropic),
            "gemini" | "google" => Ok(Self::Gemini),
            "openrouter" => Ok(Self::Openrouter),
            "ollama" => Ok(Self::Ollama),
            "lm_studio" | "lmstudio" => Ok(Self::LmStudio),
            "llama_server" | "llama-server" | "llamacpp" | "llama_cpp" => Ok(Self::LlamaServer),
            "custom" | "custom_openai_compatible" | "openai_compatible" => {
                Ok(Self::CustomOpenaiCompatible)
            }
            _ => Err(()),
        }
    }
}

#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum EmbeddingProvider {
    Openai,
    Gemini,
    Openrouter,
    CustomOpenaiCompatible,
}

impl EmbeddingProvider {
    pub const ALL: [Self; 4] = [
        Self::Openai,
        Self::Gemini,
        Self::Openrouter,
        Self::CustomOpenaiCompatible,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Openai => "OpenAI",
            Self::Gemini => "Gemini",
            Self::Openrouter => "OpenRouter",
            Self::CustomOpenaiCompatible => "Custom OpenAI-compatible",
        }
    }

    pub fn default_base_url(self) -> &'static str {
        match self {
            Self::Openai => "https://api.openai.com/v1",
            Self::Gemini => "https://generativelanguage.googleapis.com/v1beta/openai",
            Self::Openrouter => "https://openrouter.ai/api/v1",
            Self::CustomOpenaiCompatible => "",
        }
    }

    pub fn default_model(self) -> &'static str {
        match self {
            Self::Openai => "text-embedding-3-small",
            Self::Gemini => "gemini-embedding-001",
            Self::Openrouter => "openai/text-embedding-3-small",
            Self::CustomOpenaiCompatible => "",
        }
    }

    pub fn model_presets(self) -> &'static [&'static str] {
        match self {
            Self::Openai => &["text-embedding-3-small", "text-embedding-3-large"],
            Self::Gemini => &["gemini-embedding-001", "text-embedding-004"],
            Self::Openrouter => &[
                "openai/text-embedding-3-small",
                "openai/text-embedding-3-large",
            ],
            Self::CustomOpenaiCompatible => &[],
        }
    }

    pub fn known_dimensions(self, model: &str) -> Option<u32> {
        match (self, model.trim()) {
            (Self::Openai, "text-embedding-3-small")
            | (Self::Openrouter, "openai/text-embedding-3-small") => Some(1536),
            (Self::Openai, "text-embedding-3-large")
            | (Self::Openrouter, "openai/text-embedding-3-large")
            | (Self::Gemini, "gemini-embedding-001") => Some(3072),
            (Self::Gemini, "text-embedding-004") => Some(768),
            _ => None,
        }
    }
}

impl Default for EmbeddingProvider {
    fn default() -> Self {
        Self::Openai
    }
}

impl std::str::FromStr for EmbeddingProvider {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match normalize_provider_key(value).as_str() {
            "openai" => Ok(Self::Openai),
            "gemini" | "google" => Ok(Self::Gemini),
            "openrouter" => Ok(Self::Openrouter),
            "custom" | "custom_openai_compatible" | "openai_compatible" => {
                Ok(Self::CustomOpenaiCompatible)
            }
            _ => Err(()),
        }
    }
}

pub fn custom_model_label() -> &'static str {
    CUSTOM_MODEL_LABEL
}

pub fn infer_llm_provider(base_url: &str) -> Option<LlmProvider> {
    let base = normalized_base_url(base_url);
    match base.as_str() {
        "https://api.openai.com/v1" => Some(LlmProvider::Openai),
        "https://api.anthropic.com/v1" => Some(LlmProvider::Anthropic),
        "https://generativelanguage.googleapis.com/v1beta/openai" => Some(LlmProvider::Gemini),
        "https://openrouter.ai/api/v1" => Some(LlmProvider::Openrouter),
        "http://localhost:11434/v1" | "http://127.0.0.1:11434/v1" => Some(LlmProvider::Ollama),
        "http://localhost:1234/v1" | "http://127.0.0.1:1234/v1" => Some(LlmProvider::LmStudio),
        "http://localhost:8080/v1" | "http://127.0.0.1:8080/v1" => Some(LlmProvider::LlamaServer),
        _ => None,
    }
}

pub fn infer_embedding_provider(base_url: &str) -> Option<EmbeddingProvider> {
    let base = normalized_base_url(base_url);
    match base.as_str() {
        "https://api.openai.com/v1" => Some(EmbeddingProvider::Openai),
        "https://generativelanguage.googleapis.com/v1beta/openai" => {
            Some(EmbeddingProvider::Gemini)
        }
        "https://openrouter.ai/api/v1" => Some(EmbeddingProvider::Openrouter),
        _ => None,
    }
}

fn normalize_provider_key(value: &str) -> String {
    value.trim().to_ascii_lowercase().replace([' ', '-'], "_")
}

fn normalized_base_url(base_url: &str) -> String {
    base_url.trim().trim_end_matches('/').to_ascii_lowercase()
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
                pdf_dir: root.join("pdfs"),
            },
            llm: LlmConfig::default(),
            embedding: EmbeddingConfig::default(),
            embedding_dimensions: 1536,
            contact_email: String::new(),
            semantic_scholar_api_key: String::new(),
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
            pdf_dir: env_path("PDF_DIR").unwrap_or(base.storage.pdf_dir),
        };

        let llm = LlmConfig {
            provider: env::var("LLM_PROVIDER")
                .ok()
                .and_then(|value| value.parse().ok())
                .or(base.llm.provider),
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
            provider: env::var("EMBEDDING_PROVIDER")
                .ok()
                .and_then(|value| value.parse().ok())
                .or(base.embedding.provider),
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

        let semantic_scholar_api_key = env::var("SEMANTIC_SCHOLAR_API_KEY")
            .map(|value| value.trim().to_string())
            .unwrap_or(base.semantic_scholar_api_key);

        Ok(Self {
            storage,
            llm,
            embedding,
            embedding_dimensions,
            contact_email,
            semantic_scholar_api_key,
        })
    }

    /// The Semantic Scholar API key as `Some` only when non-empty, so the source
    /// can be skipped cleanly when no key is configured.
    pub fn semantic_scholar_api_key_opt(&self) -> Option<String> {
        let trimmed = self.semantic_scholar_api_key.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_llm_config_without_provider_infers_from_base_url() {
        let config: LlmConfig = serde_json::from_str(
            r#"{"base_url":"https://api.anthropic.com/v1","model":"claude-sonnet-5"}"#,
        )
        .unwrap();

        assert_eq!(config.provider, None);
        assert_eq!(config.effective_provider(), LlmProvider::Anthropic);
    }

    #[test]
    fn legacy_embedding_config_without_provider_infers_from_base_url() {
        let config: EmbeddingConfig = serde_json::from_str(
            r#"{"base_url":"https://generativelanguage.googleapis.com/v1beta/openai","model":"gemini-embedding-001"}"#,
        )
        .unwrap();

        assert_eq!(config.provider, None);
        assert_eq!(config.effective_provider(), EmbeddingProvider::Gemini);
    }

    #[test]
    fn embedding_catalog_reports_known_dimensions() {
        assert_eq!(
            EmbeddingProvider::Openai.known_dimensions("text-embedding-3-small"),
            Some(1536)
        );
        assert_eq!(
            EmbeddingProvider::Gemini.known_dimensions("gemini-embedding-001"),
            Some(3072)
        );
        assert_eq!(
            EmbeddingProvider::CustomOpenaiCompatible.known_dimensions("local-embed"),
            None
        );
    }
}
