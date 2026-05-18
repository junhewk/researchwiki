use std::sync::Arc;

use tokio::{
    runtime::{Builder, Handle, Runtime},
    sync::{mpsc, watch},
};

/// Events the background tokio runtime sends to the UI thread.
///
/// One variant per kind of update the UI might apply. Keep the payloads
/// small — UI receivers drain this channel every frame, so allocating large
/// strings here is fine but holding locks is not.
#[derive(Debug, Clone)]
pub enum UiEvent {
    /// Generic toast/status message to display in a notification area.
    Status(String),
    /// A long-running job emitted an update.
    JobProgress {
        run_id: String,
        step: String,
        message: String,
    },
    /// A long-running job finished (success or failure).
    JobFinished {
        run_id: String,
        success: bool,
        message: Option<String>,
    },
}

/// Bundled tokio runtime + the channels used to talk to the UI.
///
/// Constructed once at startup in main. The `Runtime` is held by `Arc` so
/// background tasks can keep it alive even if the UI re-creates its
/// `DesktopApp` (eframe's `creation_context` re-init flow).
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
