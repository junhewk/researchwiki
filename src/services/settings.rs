use std::{collections::BTreeMap, env, path::PathBuf};

use tokio::sync::RwLock;

use crate::{
    error::AppError,
    models::settings::{
        AiProvider, ApiKeyStatus, ApiKeyUpdate, SettingsResponse, SettingsUpdate, StoredSettings,
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

    pub async fn set_api_key(&self, update: ApiKeyUpdate) -> Result<String, AppError> {
        let _guard = self.lock.write().await;
        let mut stored = self.load().await?;
        stored
            .api_keys
            .insert(update.provider.as_str().to_string(), update.api_key.clone());
        self.save(&stored).await?;
        Ok(mask_api_key(&update.api_key))
    }

    pub async fn delete_api_key(&self, provider: AiProvider) -> Result<(), AppError> {
        let _guard = self.lock.write().await;
        let mut stored = self.load().await?;
        let removed = stored.api_keys.remove(provider.as_str());
        if removed.is_none() {
            return Err(AppError::NotFound(format!(
                "API key for {} not found",
                provider.as_str()
            )));
        }
        self.save(&stored).await
    }

    pub async fn get_feature_flags(&self) -> Result<(bool, bool), AppError> {
        let stored = self.load().await?;
        Ok((stored.library_enabled, stored.kg_enabled))
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
        let parsed = serde_json::from_str::<StoredSettings>(&raw)
            .map_err(|error| AppError::Internal(error.to_string()))?;
        Ok(parsed)
    }

    async fn save(&self, stored: &StoredSettings) -> Result<(), AppError> {
        if let Some(parent) = self.settings_file.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        let raw = serde_json::to_string_pretty(stored)
            .map_err(|error| AppError::Internal(error.to_string()))?;
        tokio::fs::write(&self.settings_file, raw).await?;
        Ok(())
    }
}

fn mask_api_key(key: &str) -> String {
    if key.len() <= 8 {
        return "***".to_string();
    }
    format!("{}...{}", &key[..4], &key[key.len() - 4..])
}
