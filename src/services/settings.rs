use std::{
    collections::BTreeMap,
    env,
    path::{Path, PathBuf},
};

use tokio::sync::RwLock;

use crate::{
    config::{
        EmbeddingConfig, LlmConfig, infer_embedding_provider, infer_llm_provider, normalize_api_key,
    },
    error::AppError,
    models::settings::{
        AiProvider, ApiKeyStatus, SettingsResponse, SettingsUpdate, StoredSettings, UiLanguage,
    },
};

pub struct SettingsService {
    settings_file: PathBuf,
    lock: RwLock<()>,
}

impl SettingsService {
    pub fn new(settings_file: PathBuf) -> Self {
        Self {
            settings_file,
            lock: RwLock::new(()),
        }
    }

    pub async fn get_settings(&self) -> Result<SettingsResponse, AppError> {
        let stored = self.load().await?;

        let api_keys = AiProvider::ALL
            .into_iter()
            .map(|provider| {
                let value = stored
                    .api_keys
                    .get(provider.as_str())
                    .cloned()
                    .or_else(|| env::var(provider.env_key()).ok());

                ApiKeyStatus {
                    provider,
                    is_configured: value.is_some(),
                    masked_key: value.as_deref().map(mask_api_key),
                }
            })
            .collect();

        Ok(SettingsResponse {
            api_keys,
            scheduler: stored.scheduler,
            ui_language: stored.ui_language,
        })
    }

    pub async fn update_settings(&self, update: SettingsUpdate) -> Result<(), AppError> {
        let _guard = self.lock.write().await;
        let mut stored = self.load().await?;

        if let Some(scheduler) = update.scheduler {
            stored.scheduler = scheduler;
        }
        if let Some(language) = update.ui_language {
            stored.ui_language = language;
        }

        self.save(&stored).await
    }

    pub async fn get_ui_language(&self) -> Result<UiLanguage, AppError> {
        let stored = self.load().await?;
        Ok(stored.ui_language)
    }

    pub async fn set_ui_language(&self, language: UiLanguage) -> Result<(), AppError> {
        let _guard = self.lock.write().await;
        let mut stored = self.load().await?;
        stored.ui_language = language;
        self.save(&stored).await
    }

    pub async fn get_feature_flags(&self) -> Result<(bool, bool), AppError> {
        let stored = self.load().await?;
        Ok((stored.library_enabled, stored.kg_enabled))
    }

    pub async fn get_llm_config(&self) -> Result<Option<LlmConfig>, AppError> {
        let stored = self.load().await?;
        Ok(stored.llm)
    }

    pub async fn set_llm_config(&self, llm: LlmConfig) -> Result<(), AppError> {
        let _guard = self.lock.write().await;
        let mut stored = self.load().await?;
        stored.llm = Some(sanitize_llm_config(llm));
        self.save(&stored).await
    }

    pub async fn get_embedding_config(&self) -> Result<Option<EmbeddingConfig>, AppError> {
        let stored = self.load().await?;
        Ok(stored.embedding)
    }

    pub async fn set_embedding_config(&self, embedding: EmbeddingConfig) -> Result<(), AppError> {
        let _guard = self.lock.write().await;
        let mut stored = self.load().await?;
        stored.embedding = Some(sanitize_embedding_config(embedding));
        self.save(&stored).await
    }

    pub async fn get_embedding_dimensions(&self) -> Result<Option<u32>, AppError> {
        let stored = self.load().await?;
        Ok(stored.embedding_dimensions)
    }

    pub async fn get_contact_email(&self) -> Result<Option<String>, AppError> {
        let stored = self.load().await?;
        Ok(stored.contact_email)
    }

    pub async fn get_semantic_scholar_api_key(&self) -> Result<Option<String>, AppError> {
        let stored = self.load().await?;
        Ok(stored.semantic_scholar_api_key)
    }

    pub async fn set_semantic_scholar_api_key(&self, key: Option<String>) -> Result<(), AppError> {
        let _guard = self.lock.write().await;
        let mut stored = self.load().await?;
        stored.semantic_scholar_api_key = key
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        self.save(&stored).await
    }

    pub async fn set_setup_complete(&self, complete: bool) -> Result<(), AppError> {
        let _guard = self.lock.write().await;
        let mut stored = self.load().await?;
        stored.setup_complete = Some(complete);
        self.save(&stored).await
    }

    pub async fn set_contact_email(&self, email: Option<String>) -> Result<(), AppError> {
        let _guard = self.lock.write().await;
        let mut stored = self.load().await?;
        stored.contact_email = email
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        self.save(&stored).await
    }

    pub async fn set_embedding_dimensions(&self, dim: u32) -> Result<(), AppError> {
        let _guard = self.lock.write().await;
        let mut stored = self.load().await?;
        stored.embedding_dimensions = Some(dim);
        self.save(&stored).await
    }

    pub async fn get_api_key(&self, provider: AiProvider) -> Result<Option<String>, AppError> {
        let stored = self.load().await?;
        Ok(stored
            .api_keys
            .get(provider.as_str())
            .cloned()
            .or_else(|| env::var(provider.env_key()).ok()))
    }

    async fn load(&self) -> Result<StoredSettings, AppError> {
        if !self.settings_file.exists() {
            return Ok(StoredSettings {
                api_keys: BTreeMap::new(),
                ..StoredSettings::default()
            });
        }

        let raw = tokio::fs::read_to_string(&self.settings_file).await?;
        let mut parsed = serde_json::from_str::<StoredSettings>(strip_utf8_bom(&raw))
            .map_err(|error| AppError::Internal(error.to_string()))?;
        sanitize_stored_settings(&mut parsed);
        Ok(parsed)
    }

    async fn save(&self, stored: &StoredSettings) -> Result<(), AppError> {
        if let Some(parent) = self.settings_file.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        let raw = serde_json::to_string_pretty(stored)
            .map_err(|error| AppError::Internal(error.to_string()))?;
        tokio::fs::write(&self.settings_file, raw.as_bytes()).await?;
        // settings.json holds API keys; restrict it to the owner on Unix.
        // Windows relies on the per-user %APPDATA% location for access control.
        restrict_permissions(&self.settings_file).await;
        Ok(())
    }
}

/// Best-effort `chmod 0600` so the API keys in settings.json aren't world- or
/// group-readable. No-op on non-Unix (Windows has no equivalent mode bits).
async fn restrict_permissions(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(metadata) = tokio::fs::metadata(path).await {
            let mut perms = metadata.permissions();
            perms.set_mode(0o600);
            let _ = tokio::fs::set_permissions(path, perms).await;
        }
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
}

/// Persisted startup overrides read synchronously before the tokio runtime
/// exists. Missing/unparseable file → all-`None`; startup uses defaults.
pub struct StartupOverrides {
    pub llm: Option<LlmConfig>,
    pub embedding: Option<EmbeddingConfig>,
    pub embedding_dimensions: Option<u32>,
    pub contact_email: Option<String>,
    pub semantic_scholar_api_key: Option<String>,
}

/// Sync read of settings.json overrides at startup, before the tokio runtime
/// exists. Missing/unparseable file → all-`None`; startup uses defaults.
pub fn load_overrides_sync(settings_file: &Path) -> StartupOverrides {
    let empty = || StartupOverrides {
        llm: None,
        embedding: None,
        embedding_dimensions: None,
        contact_email: None,
        semantic_scholar_api_key: None,
    };
    let Ok(raw) = std::fs::read_to_string(settings_file) else {
        return empty();
    };
    let Ok(mut stored) = serde_json::from_str::<StoredSettings>(strip_utf8_bom(&raw)) else {
        return empty();
    };
    sanitize_stored_settings(&mut stored);
    StartupOverrides {
        llm: stored.llm,
        embedding: stored.embedding,
        embedding_dimensions: stored.embedding_dimensions,
        contact_email: stored.contact_email,
        semantic_scholar_api_key: stored.semantic_scholar_api_key,
    }
}

pub fn load_ui_language_sync(settings_file: &Path) -> UiLanguage {
    let Ok(raw) = std::fs::read_to_string(settings_file) else {
        return UiLanguage::default();
    };
    let Ok(mut stored) = serde_json::from_str::<StoredSettings>(strip_utf8_bom(&raw)) else {
        return UiLanguage::default();
    };
    sanitize_stored_settings(&mut stored);
    stored.ui_language
}

/// Raw persisted `setup_complete` flag read synchronously at startup. `None`
/// means the file is missing/unreadable or predates the field.
pub fn load_setup_complete_sync(settings_file: &Path) -> Option<bool> {
    let raw = std::fs::read_to_string(settings_file).ok()?;
    let stored = serde_json::from_str::<StoredSettings>(strip_utf8_bom(&raw)).ok()?;
    stored.setup_complete
}

fn strip_utf8_bom(raw: &str) -> &str {
    raw.strip_prefix('\u{feff}').unwrap_or(raw)
}

fn sanitize_stored_settings(stored: &mut StoredSettings) {
    for value in stored.api_keys.values_mut() {
        *value = normalize_api_key(&*value);
    }
    if let Some(llm) = stored.llm.take() {
        stored.llm = Some(sanitize_llm_config(llm));
    }
    if let Some(embedding) = stored.embedding.take() {
        stored.embedding = Some(sanitize_embedding_config(embedding));
    }
}

fn sanitize_llm_config(mut llm: LlmConfig) -> LlmConfig {
    llm.api_key = normalize_api_key(&llm.api_key);
    if llm.provider.is_none() {
        llm.provider = infer_llm_provider(&llm.base_url);
    }
    llm
}

fn sanitize_embedding_config(mut embedding: EmbeddingConfig) -> EmbeddingConfig {
    embedding.api_key = normalize_api_key(&embedding.api_key);
    if embedding.provider.is_none() {
        embedding.provider = infer_embedding_provider(&embedding.base_url);
    }
    embedding
}

fn mask_api_key(key: &str) -> String {
    if key.len() <= 8 {
        return "***".to_string();
    }
    format!("{}...{}", &key[..4], &key[key.len() - 4..])
}

#[cfg(test)]
mod tests {
    use super::strip_utf8_bom;

    #[test]
    fn strip_utf8_bom_removes_leading_bom_only() {
        assert_eq!(strip_utf8_bom("\u{feff}{\"ok\":true}"), "{\"ok\":true}");
        assert_eq!(strip_utf8_bom("{\"ok\":true}"), "{\"ok\":true}");
    }
}
