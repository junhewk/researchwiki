use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AiProvider {
    Openai,
    Anthropic,
    Google,
    AzureOpenai,
}

impl AiProvider {
    pub const ALL: [Self; 1] = [Self::Openai];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Openai => "openai",
            Self::Anthropic => "anthropic",
            Self::Google => "google",
            Self::AzureOpenai => "azure_openai",
        }
    }

    pub fn env_key(self) -> &'static str {
        match self {
            Self::Openai => "OPENAI_API_KEY",
            Self::Anthropic => "ANTHROPIC_API_KEY",
            Self::Google => "GOOGLE_API_KEY",
            Self::AzureOpenai => "AZURE_OPENAI_API_KEY",
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ApiKeyUpdate {
    pub provider: AiProvider,
    pub api_key: String,
}

#[derive(Debug, Serialize)]
pub struct ApiKeyStatus {
    pub provider: AiProvider,
    pub is_configured: bool,
    pub masked_key: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SchedulerSettings {
    pub arxiv_schedule_hour: u8,
    pub arxiv_schedule_minute: u8,
    pub pmc_schedule_hour: u8,
    pub pmc_schedule_minute: u8,
    pub pubmed_schedule_hour: u8,
    pub pubmed_schedule_minute: u8,
    pub enabled: bool,
}

impl Default for SchedulerSettings {
    fn default() -> Self {
        Self {
            arxiv_schedule_hour: 19,
            arxiv_schedule_minute: 0,
            pmc_schedule_hour: 18,
            pmc_schedule_minute: 0,
            pubmed_schedule_hour: 18,
            pubmed_schedule_minute: 30,
            enabled: true,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct NewsletterSettings {
    pub default_article_count: u8,
    pub default_days: u8,
}

impl Default for NewsletterSettings {
    fn default() -> Self {
        Self {
            default_article_count: 7,
            default_days: 7,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct StoredSettings {
    #[serde(default)]
    pub api_keys: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    pub scheduler: SchedulerSettings,
    #[serde(default)]
    pub newsletter: NewsletterSettings,
    #[serde(default = "default_true")]
    pub library_enabled: bool,
    #[serde(default = "default_true")]
    pub kg_enabled: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Serialize)]
pub struct SettingsResponse {
    pub api_keys: Vec<ApiKeyStatus>,
    pub scheduler: SchedulerSettings,
    pub newsletter: NewsletterSettings,
}

#[derive(Debug, Deserialize)]
pub struct SettingsUpdate {
    pub scheduler: Option<SchedulerSettings>,
    pub newsletter: Option<NewsletterSettings>,
}

#[derive(Debug, Serialize)]
pub struct SchedulerStatusResponse {
    pub status: String,
    pub jobs: Vec<SchedulerJob>,
}

#[derive(Debug, Serialize)]
pub struct SchedulerJob {
    pub id: String,
    pub name: String,
    pub next_run: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ProviderInfo {
    pub id: String,
    pub name: String,
    pub models: Vec<String>,
}
