use std::{path::Path, sync::Arc, time::Duration};

use anyhow::{Context, Result};
use tokio::{
    runtime::{Handle, Runtime},
    sync::{mpsc, watch},
    task::JoinHandle,
    time::timeout,
};
use tracing::{info, warn};

use crate::{
    config::AppConfig,
    db,
    runtime::{DesktopRuntime, UiEvent},
    services::{scheduler::run_scheduler_loop, settings::SettingsService},
    state::AppState,
    tray::{TrayCommand, TrayController},
    ui::{
        first_run::{FirstRunForm, FirstRunOutcome},
        panels::{PanelCtx, Panels, Tab},
    },
};

/// Persisted UI state — only fields that should survive an app restart.
#[derive(Default, serde::Serialize, serde::Deserialize)]
struct PersistentUi {
    schema_version: u32,
    active_tab: Tab,
}

const PERSIST_SCHEMA: u32 = 1;
const PERSIST_KEY: &str = "researchwiki_ui";

pub struct DesktopApp {
    // Held only to keep the tokio runtime alive — dropping it tears down
    // every spawned task.
    _rt: Arc<Runtime>,
    handle: Handle,
    config: AppConfig,
    state: Option<AppState>,
    scheduler: Option<JoinHandle<()>>,
    shutdown_tx: watch::Sender<bool>,
    shutdown_rx: watch::Receiver<bool>,
    ui_tx: mpsc::UnboundedSender<UiEvent>,
    ui_rx: mpsc::UnboundedReceiver<UiEvent>,
    tray: Option<TrayController>,
    tray_error_reported: bool,
    hidden_to_tray: bool,
    restoring_from_tray: bool,
    native_window_handle: Option<isize>,
    first_run: FirstRunForm,
    panels: Panels,
    persistent: PersistentUi,
    status: Option<String>,
}

impl DesktopApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        runtime: DesktopRuntime,
        config: AppConfig,
    ) -> Self {
        install_system_font_fallbacks(&cc.egui_ctx);

        let persistent = cc
            .storage
            .and_then(|storage| eframe::get_value::<PersistentUi>(storage, PERSIST_KEY))
            .filter(|p| p.schema_version == PERSIST_SCHEMA)
            .unwrap_or_else(|| PersistentUi {
                schema_version: PERSIST_SCHEMA,
                active_tab: Tab::default(),
            });

        let mut app = Self {
            _rt: runtime.rt,
            handle: runtime.handle,
            config,
            state: None,
            scheduler: None,
            shutdown_tx: runtime.shutdown_tx,
            shutdown_rx: runtime.shutdown_rx,
            ui_tx: runtime.ui_tx,
            ui_rx: runtime.ui_rx,
            tray: None,
            tray_error_reported: false,
            hidden_to_tray: false,
            restoring_from_tray: false,
            native_window_handle: native_window_handle(cc),
            first_run: FirstRunForm::default(),
            panels: Panels::default(),
            persistent,
            status: None,
        };

        if app.config.llm.is_configured() {
            app.activate();
        }

        app
    }

    fn activate(&mut self) {
        let state = AppState::new(self.config.clone());

        let prompt_service = state.prompt_service.clone();
        let job_service = state.job_service.clone();
        self.handle.block_on(async move {
            if let Err(err) = prompt_service.seed_prompt_versions().await {
                warn!("seed_prompt_versions failed: {err:#}");
            }
            match job_service.recover_interrupted_runs().await {
                Ok(n) if n > 0 => info!("marked {n} interrupted job runs as failed"),
                Ok(_) => {}
                Err(err) => warn!("recover_interrupted_runs failed: {err:#}"),
            }
        });

        let scheduler_job = state.job_service.clone();
        let scheduler_settings = state.settings_service.clone();
        let shutdown_rx = self.shutdown_rx.clone();
        let scheduler = self.handle.spawn(run_scheduler_loop(
            scheduler_job,
            scheduler_settings,
            shutdown_rx,
        ));

        self.scheduler = Some(scheduler);
        self.state = Some(state);
        self.status = Some("Ready.".to_string());
    }

    fn drain_events(&mut self) {
        while let Ok(evt) = self.ui_rx.try_recv() {
            match evt {
                UiEvent::Status(msg) => self.status = Some(msg),
                UiEvent::JobProgress {
                    run_id,
                    step,
                    message,
                } => {
                    self.status = Some(format!("[{run_id}] {step}: {message}"));
                }
                UiEvent::JobFinished {
                    run_id,
                    success,
                    message,
                } => {
                    let outcome = if success { "ok" } else { "failed" };
                    self.status = Some(match message {
                        Some(m) => format!("[{run_id}] {outcome}: {m}"),
                        None => format!("[{run_id}] {outcome}"),
                    });
                }
            }
        }
    }

    fn ensure_tray(&mut self, ctx: &egui::Context) {
        if self.tray.is_some() || self.tray_error_reported {
            return;
        }

        match TrayController::new(ctx, self.native_window_handle) {
            Ok(tray) => self.tray = Some(tray),
            Err(err) => {
                self.tray_error_reported = true;
                warn!("failed to initialize system tray: {err:#}");
                self.status = Some(format!("System tray unavailable: {err}"));
            }
        }
    }

    fn handle_tray(&mut self, ctx: &egui::Context) {
        let Some(tray) = self.tray.as_mut() else {
            return;
        };

        for command in tray.drain_commands() {
            match command {
                TrayCommand::Show => self.restore_from_tray(ctx),
                TrayCommand::Quit => {
                    self.hidden_to_tray = false;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
        }
    }

    fn restore_from_tray(&mut self, ctx: &egui::Context) {
        self.hidden_to_tray = false;
        self.restoring_from_tray = true;
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        self.status = Some("Restored from system tray.".to_string());
    }

    fn hide_to_tray_if_minimized(&mut self, ctx: &egui::Context) {
        if self.hidden_to_tray {
            return;
        }

        let minimized = ctx.input(|input| input.viewport().minimized.unwrap_or(false));
        if self.restoring_from_tray {
            if minimized {
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                return;
            }
            self.restoring_from_tray = false;
        }

        if minimized {
            self.hidden_to_tray = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            self.status = Some("Minimized to system tray. Scheduler remains active.".to_string());
        }
    }
}

impl eframe::App for DesktopApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.ensure_tray(ctx);
        self.handle_tray(ctx);
        self.hide_to_tray_if_minimized(ctx);
        self.drain_events();

        if self.state.is_none() {
            if let FirstRunOutcome::Submitted { llm, embedding } = self.first_run.show(ctx) {
                // Best-effort persist so the modal only fires once. A failed
                // write still lets the user continue in this session.
                let path = self.config.storage.settings_file.clone();
                let llm_to_save = llm.clone();
                let embedding_to_save = embedding.clone();
                let save_result = self.handle.block_on(async move {
                    let svc = SettingsService::new(path);
                    svc.set_llm_config(llm_to_save).await?;
                    svc.set_embedding_config(embedding_to_save).await
                });
                if let Err(err) = save_result {
                    warn!("failed to persist first-run config: {err:#}");
                }
                self.config.llm = llm;
                self.config.embedding = embedding;
                self.activate();
            }
            return;
        }

        egui::TopBottomPanel::top("tabs").show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                for tab in Tab::ALL {
                    ui.selectable_value(&mut self.persistent.active_tab, tab, tab.label());
                }
            });
        });

        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.label(self.status.as_deref().unwrap_or(""));
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            if let Some(state) = &self.state {
                let panel_ctx = PanelCtx {
                    state,
                    handle: &self.handle,
                    ui_tx: &self.ui_tx,
                };
                self.panels.show(self.persistent.active_tab, ui, &panel_ctx);
            }
        });
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, PERSIST_KEY, &self.persistent);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        info!("desktop app exiting; signalling scheduler shutdown");
        let _ = self.shutdown_tx.send(true);
        if let Some(scheduler) = self.scheduler.take() {
            let _ = self
                .handle
                .block_on(async move { timeout(Duration::from_secs(5), scheduler).await });
        }
    }
}

#[cfg(target_os = "windows")]
fn native_window_handle(cc: &eframe::CreationContext<'_>) -> Option<isize> {
    use raw_window_handle::{HasWindowHandle as _, RawWindowHandle};

    let handle = cc.window_handle().ok()?.as_raw();
    match handle {
        RawWindowHandle::Win32(handle) => Some(handle.hwnd.get()),
        _ => None,
    }
}

#[cfg(not(target_os = "windows"))]
fn native_window_handle(_cc: &eframe::CreationContext<'_>) -> Option<isize> {
    None
}

fn install_system_font_fallbacks(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    add_font_if_available(&mut fonts, "malgun_gothic", r"C:\Windows\Fonts\malgun.ttf");
    ctx.set_fonts(fonts);
}

fn add_font_if_available(fonts: &mut egui::FontDefinitions, name: &str, path: &str) {
    let Ok(bytes) = std::fs::read(path) else {
        return;
    };

    fonts
        .font_data
        .insert(name.to_string(), egui::FontData::from_owned(bytes).into());
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        fonts
            .families
            .entry(family)
            .or_default()
            .push(name.to_string());
    }
}

/// Create per-user directories and copy bundled prompts into the user copy on first launch.
pub fn first_launch_seed(config: &AppConfig) -> Result<()> {
    let storage = &config.storage;
    if let Some(parent) = storage.database_path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("failed to create {parent:?}"))?;
    }
    std::fs::create_dir_all(&storage.prompts_dir)
        .with_context(|| format!("failed to create {:?}", storage.prompts_dir))?;
    std::fs::create_dir_all(&storage.wiki_export_dir)
        .with_context(|| format!("failed to create {:?}", storage.wiki_export_dir))?;

    if dir_is_empty(&storage.prompts_dir)? {
        if let Some(bundled) = bundled_prompts_dir() {
            copy_dir_recursive(&bundled, &storage.prompts_dir).with_context(|| {
                format!(
                    "failed to seed prompts from {:?} to {:?}",
                    bundled, storage.prompts_dir
                )
            })?;
            info!(
                "seeded prompts from {} to {}",
                bundled.display(),
                storage.prompts_dir.display()
            );
        } else {
            warn!(
                "no bundled prompts directory found beside executable; \
                 leaving {} empty (LLM prompts will need to be authored from scratch)",
                storage.prompts_dir.display()
            );
        }
    }

    Ok(())
}

fn dir_is_empty(path: &Path) -> Result<bool> {
    let mut entries = std::fs::read_dir(path)?;
    Ok(entries.next().is_none())
}

fn bundled_prompts_dir() -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let exe_dir = exe.parent()?;
    let candidates = [
        exe_dir.join("prompts"),
        exe_dir.parent()?.join("prompts"),
        std::env::current_dir().ok()?.join("prompts"),
    ];
    candidates.into_iter().find(|p| p.is_dir())
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else if file_type.is_file() {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

pub async fn bootstrap_db(config: &AppConfig) -> Result<()> {
    db::initialize(config).await
}
