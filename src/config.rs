use std::{
    env,
    net::{IpAddr, Ipv4Addr},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};

#[derive(Clone, Debug)]
pub struct AppConfig {
    pub server: ServerConfig,
    pub storage: StorageConfig,
    pub llm: LlmConfig,
    pub embedding_dimensions: u32,
}

#[derive(Clone, Debug)]
pub struct ServerConfig {
    pub host: IpAddr,
    pub port: u16,
}

#[derive(Clone, Debug)]
pub struct StorageConfig {
    pub database_path: PathBuf,
    pub prompts_dir: PathBuf,
    pub settings_file: PathBuf,
    pub wiki_export_dir: PathBuf,
}

#[derive(Clone, Debug)]
pub struct LlmConfig {
    pub base_url: String,
    pub model: String,
    pub api_key: String,
    pub disable_thinking: bool,
    pub connect_timeout_seconds: u64,
    pub request_timeout_seconds: u64,
    pub max_attempts: usize,
    pub max_concurrent_requests: usize,
}

impl AppConfig {
    pub fn from_env() -> Result<Self> {
        let cwd = env::current_dir().context("failed to read current directory")?;
        let database_url = env::var("DATABASE_URL")
            .unwrap_or_else(|_| "sqlite+aiosqlite:////var/lib/articlegatherer/haie.db".to_string());

        let database_path =
            parse_sqlite_database_path(&database_url).unwrap_or_else(|| cwd.join("test_local.db"));

        let prompts_dir = env_path("PROMPTS_DIR").unwrap_or_else(|| cwd.join("backend/prompts"));
        let settings_file =
            env_path("SETTINGS_FILE").unwrap_or_else(|| cwd.join("backend/data/settings.json"));
        let wiki_export_dir = env_path("WIKI_EXPORT_DIR")
            .unwrap_or_else(|| PathBuf::from("/var/lib/articlegatherer/wiki/healthcare-ai-ethics"));

        let host = env::var("RUST_BACKEND_HOST")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST));
        let port = env::var("RUST_BACKEND_PORT")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(8787);

        let embedding_dimensions = env::var("EMBEDDING_DIMENSIONS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(1536);

        let llm = LlmConfig {
            base_url: env::var("LLM_BASE_URL")
                .unwrap_or_else(|_| "http://100.122.169.13:8091/v1".to_string())
                .trim_end_matches('/')
                .to_string(),
            model: env::var("LLM_MODEL").unwrap_or_else(|_| "qwen3.6-27b-q8".to_string()),
            api_key: env::var("LLM_API_KEY").unwrap_or_else(|_| "local".to_string()),
            disable_thinking: env_bool("LLM_DISABLE_THINKING", true),
            connect_timeout_seconds: env_u64("LLM_CONNECT_TIMEOUT_SECONDS", 10).clamp(1, 120),
            request_timeout_seconds: env_u64("LLM_REQUEST_TIMEOUT_SECONDS", 300).clamp(10, 900),
            max_attempts: env_usize("LLM_MAX_ATTEMPTS", 4).clamp(1, 5),
            max_concurrent_requests: env_usize("LLM_MAX_CONCURRENT_REQUESTS", 1).clamp(1, 16),
        };

        Ok(Self {
            server: ServerConfig { host, port },
            storage: StorageConfig {
                database_path,
                prompts_dir,
                settings_file,
                wiki_export_dir,
            },
            llm,
            embedding_dimensions,
        })
    }
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

fn env_u64(key: &str, default: u64) -> u64 {
    env::var(key)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn env_usize(key: &str, default: usize) -> usize {
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
