use std::sync::Arc;

use tokio::{
    runtime::{Builder, Handle, Runtime},
    sync::{mpsc, watch},
};

use crate::models::settings::UiLanguage;

#[derive(Debug, Clone)]
pub enum UiEvent {
    Status(String),
    LanguageChanged(UiLanguage),
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
