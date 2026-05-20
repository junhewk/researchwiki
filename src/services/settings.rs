use std::{
    collections::BTreeMap,
    env,
    path::{Path, PathBuf},
};

use tokio::sync::RwLock;

use crate::{
    config::{EmbeddingConfig, LlmConfig, normalize_api_key},
    error::AppError,
    models::settings::{
        AiProvider, ApiKeyStatus, SettingsResponse, SettingsUpdate, StoredSettings,
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
            newsletter: stored.newsletter,
        })
    }

    pub async fn update_settings(&self, update: SettingsUpdate) -> Result<(), AppError> {
        let _guard = self.lock.write().await;
        let mut stored = self.load().await?;

        if let Some(scheduler) = update.scheduler {
            stored.scheduler = scheduler;
        }
        if let Some(newsletter) = update.newsletter {
            stored.newsletter = newsletter;
        }

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
        Ok(())
    }
}

/// Sync read of settings.json overrides at startup, before the tokio runtime
/// exists. Missing/unparseable file → all-`None`; startup uses defaults.
pub fn load_overrides_sync(
    settings_file: &Path,
) -> (Option<LlmConfig>, Option<EmbeddingConfig>, Option<u32>) {
    let Ok(raw) = std::fs::read_to_string(settings_file) else {
        return (None, None, None);
    };
    let Ok(mut stored) = serde_json::from_str::<StoredSettings>(strip_utf8_bom(&raw)) else {
        return (None, None, None);
    };
    sanitize_stored_settings(&mut stored);
    (stored.llm, stored.embedding, stored.embedding_dimensions)
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
    llm
}

fn sanitize_embedding_config(mut embedding: EmbeddingConfig) -> EmbeddingConfig {
    embedding.api_key = normalize_api_key(&embedding.api_key);
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
