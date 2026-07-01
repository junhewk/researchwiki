use std::sync::Arc;

use tokio::{
    runtime::{Builder, Handle, Runtime},
    sync::{mpsc, watch},
};

use crate::config::{EmbeddingConfig, LlmConfig};
use crate::models::{job::JobRunResponse, settings::UiLanguage};
use crate::ui::{panels::Tab, toast::ToastKind};

#[derive(Debug, Clone)]
pub enum UiEvent {
    Status(String),
    /// Transient notification rendered in the top-right toast stack.
    Toast {
        kind: ToastKind,
        message: String,
    },
    /// Navigate the main window to a tab (e.g. from an empty-state action).
    SwitchTab(Tab),
    LanguageChanged(UiLanguage),
    /// Settings that are part of AppState's service graph changed; rebuild the
    /// active workspace state so new requests use the saved config immediately.
    LlmConfigChanged(LlmConfig),
    EmbeddingConfigChanged {
        embedding: EmbeddingConfig,
        embedding_dimensions: Option<u32>,
    },
    ContactEmailChanged(Option<String>),
    SemanticScholarApiKeyChanged(Option<String>),
    ActiveJobsUpdated {
        workspace_id: i64,
        jobs: Vec<JobRunResponse>,
    },
    ActiveJobsLoadFailed {
        workspace_id: i64,
        message: String,
    },
    JobProgress {
        run_id: String,
        step: String,
        message: String,
    },
    JobFinished {
        run_id: String,
        success: bool,
        message: Option<String>,
    },
}

pub struct DesktopRuntime {
    pub rt: Arc<Runtime>,
    pub handle: Handle,
    pub ui_tx: mpsc::UnboundedSender<UiEvent>,
    pub ui_rx: mpsc::UnboundedReceiver<UiEvent>,
    pub shutdown_tx: watch::Sender<bool>,
    pub shutdown_rx: watch::Receiver<bool>,
}

impl DesktopRuntime {
    pub fn new() -> anyhow::Result<Self> {
        let rt = Builder::new_multi_thread()
            .enable_all()
            .worker_threads(2)
            .thread_name("researchwiki-tokio")
            .build()?;
        let handle = rt.handle().clone();
        let (ui_tx, ui_rx) = mpsc::unbounded_channel();
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        Ok(Self {
            rt: Arc::new(rt),
            handle,
            ui_tx,
            ui_rx,
            shutdown_tx,
            shutdown_rx,
        })
    }
}
