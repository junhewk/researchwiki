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
    models::{
        settings::UiLanguage,
        workspace::{WorkspaceSummary, WorkspaceUpdate},
    },
    runtime::{DesktopRuntime, UiEvent},
    services::{
        scheduler::run_scheduler_loop, settings::SettingsService, workspace::WorkspaceService,
    },
    state::AppState,
    tray::{TrayCommand, TrayController},
    ui::{
        first_run::{FirstRunForm, FirstRunOutcome, ResearchSetupForm, ResearchSetupOutcome},
        i18n,
        panels::{PanelCtx, Panels, Tab},
        style,
    },
};

/// Persisted UI state — only fields that should survive an app restart.
#[derive(Default, serde::Serialize, serde::Deserialize)]
struct PersistentUi {
    schema_version: u32,
    active_tab: Tab,
    /// Active workspace id; 0 = unset (reconciled against the DB on activate).
    active_workspace_id: i64,
}

const PERSIST_SCHEMA: u32 = 2;
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
    research_setup: ResearchSetupForm,
    /// Whether the guided research-setup step is done. While false (and the app
    /// is activated), the research-setup modal blocks the main UI.
    setup_complete: bool,
    panels: Panels,
    persistent: PersistentUi,
    status: Option<String>,
    workspaces: Vec<WorkspaceSummary>,
    workspaces_refreshed_at: Option<std::time::Instant>,
    language: UiLanguage,
}

impl DesktopApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        runtime: DesktopRuntime,
        config: AppConfig,
        language: UiLanguage,
        setup_complete: bool,
    ) -> Self {
        install_system_font_fallbacks(&cc.egui_ctx);
        style::apply_app_style(&cc.egui_ctx);

        let persistent = cc
            .storage
            .and_then(|storage| eframe::get_value::<PersistentUi>(storage, PERSIST_KEY))
            .filter(|p| p.schema_version == PERSIST_SCHEMA)
            .unwrap_or_else(|| PersistentUi {
                schema_version: PERSIST_SCHEMA,
                active_tab: Tab::default(),
                active_workspace_id: 0,
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
            research_setup: ResearchSetupForm::default(),
            setup_complete,
            panels: Panels::default(),
            persistent,
            status: None,
            workspaces: Vec::new(),
            workspaces_refreshed_at: None,
            language,
        };

        // Only skip setup when both endpoints are present *and* well-formed; a
        // malformed saved config routes to the wizard (pre-filled) instead of
        // activating and failing mid-job.
        if app.config.is_ready() {
            app.activate();
        } else {
            app.first_run.prefill_from(&app.config);
        }

        app
    }

    fn workspaces_dir(&self) -> std::path::PathBuf {
        self.config
            .storage
            .database_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| std::path::PathBuf::from("."))
    }

    fn workspace_registry(&self) -> WorkspaceService {
        let dir = self.workspaces_dir();
        WorkspaceService::new(dir.join("meta.db"), dir)
    }

    fn activate(&mut self) {
        // Reconcile the active workspace from the registry: keep the persisted
        // id if it still exists, else the registry's active/default workspace.
        let registry = self.workspace_registry();
        let persisted = self.persistent.active_workspace_id;
        let (workspaces, active_id) = self.handle.block_on(async move {
            let list = registry.list().await.unwrap_or_default();
            let valid = persisted > 0 && list.iter().any(|w| w.id == persisted);
            let active = if valid {
                persisted
            } else {
                registry.active_or_default_id().await.unwrap_or(1)
            };
            (list, active)
        });
        self.workspaces = workspaces;
        self.workspaces_refreshed_at = Some(std::time::Instant::now());
        self.build_state_for_workspace(active_id);
    }

    /// Builds (or rebuilds) the service graph pointed at `workspace_id`'s data
    /// file and restarts the scheduler bound to it. Used at startup and on every
    /// top-bar workspace switch.
    fn build_state_for_workspace(&mut self, workspace_id: i64) {
        let registry = self.workspace_registry();
        let ws_dir = self.workspaces_dir();
        let default_db = self.config.storage.database_path.clone();
        let db_path = self.handle.block_on(async move {
            let _ = registry.set_active(workspace_id).await;
            match registry.get(workspace_id).await {
                Ok(ws) => ws_dir.join(ws.db_filename),
                Err(_) => default_db,
            }
        });

        let dims = self.config.embedding_dimensions;
        if let Err(err) = self
            .handle
            .block_on(db::initialize_workspace_db(db_path.clone(), dims))
        {
            warn!("workspace db init failed: {err:#}");
        }

        let state = AppState::new(self.config.clone(), db_path, workspace_id);

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

        // Restart the scheduler bound to the now-active workspace.
        if let Some(old) = self.scheduler.take() {
            old.abort();
        }
        let scheduler_job = state.job_service.clone();
        let scheduler_settings = state.settings_service.clone();
        let shutdown_rx = self.shutdown_rx.clone();
        let scheduler = self.handle.spawn(run_scheduler_loop(
            scheduler_job,
            scheduler_settings,
            workspace_id,
            shutdown_rx,
        ));
        self.scheduler = Some(scheduler);

        self.persistent.active_workspace_id = workspace_id;
        self.state = Some(state);
        self.status = Some(i18n::t(self.language, "Ready.").to_string());
    }

    /// Renders the first-launch research-setup modal, pre-filling it once from
    /// the active workspace's seeded defaults.
    fn show_research_setup(&mut self, ctx: &egui::Context) {
        if !self.research_setup.is_prefilled() {
            let prefill = self.state.as_ref().map(|state| {
                let svc = state.workspace_service.clone();
                let id = self.persistent.active_workspace_id;
                self.handle.block_on(async move { svc.get(id).await })
            });
            match prefill {
                Some(Ok(ws)) => {
                    self.research_setup
                        .prefill(&ws.name, &ws.primary_question, &ws.seed_concepts);
                }
                _ => self.research_setup.prefill("", "", &[]),
            }
        }

        match self.research_setup.show(ctx, self.language) {
            ResearchSetupOutcome::Submitted {
                name,
                primary_question,
                topics,
            } => self.complete_research_setup(Some((name, primary_question, topics))),
            ResearchSetupOutcome::Skipped => self.complete_research_setup(None),
            ResearchSetupOutcome::Pending => {}
        }
    }

    /// Saves the research-setup values (when provided) into the active workspace
    /// and records that setup is done so the modal won't fire again.
    fn complete_research_setup(&mut self, values: Option<(String, String, Vec<String>)>) {
        let id = self.persistent.active_workspace_id;
        if let (Some((name, primary_question, topics)), Some(state)) = (values, self.state.as_ref())
        {
            let svc = state.workspace_service.clone();
            let update = WorkspaceUpdate {
                name: Some(name),
                primary_question: Some(primary_question),
                gap_note: None,
                refined_question: None,
                topic_descriptor: None,
                seed_concepts: Some(topics),
                override_queries: None,
                lookback_days: None,
            };
            if let Err(err) = self
                .handle
                .block_on(async move { svc.update(id, update).await })
            {
                warn!("failed to save research setup: {err:#}");
            }
        }

        let path = self.config.storage.settings_file.clone();
        if let Err(err) = self
            .handle
            .block_on(async move { SettingsService::new(path).set_setup_complete(true).await })
        {
            warn!("failed to persist setup_complete: {err:#}");
        }
        self.setup_complete = true;
        // Refresh the top-bar workspace list so the chosen name shows up.
        self.workspaces_refreshed_at = None;
    }

    fn drain_events(&mut self) {
        while let Ok(evt) = self.ui_rx.try_recv() {
            match evt {
                UiEvent::Status(msg) => self.status = Some(msg),
                UiEvent::LanguageChanged(language) => {
                    self.language = language;
                    self.status = Some(i18n::t(language, "Language updated.").to_string());
                }
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

    /// Reload the workspace list for the top-bar switcher, throttled so we
    /// don't hit SQLite every frame. Picks up workspaces created in the
    /// Input Set tab within a couple of seconds.
    fn maybe_refresh_workspaces(&mut self) {
        let stale = self
            .workspaces_refreshed_at
            .map(|t| t.elapsed() > Duration::from_secs(2))
            .unwrap_or(true);
        if !stale {
            return;
        }
        if let Some(state) = &self.state {
            let ws = state.workspace_service.clone();
            if let Ok(list) = self.handle.block_on(async move { ws.list().await }) {
                self.workspaces = list;
            }
        }
        self.workspaces_refreshed_at = Some(std::time::Instant::now());
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
        self.status = Some(i18n::t(self.language, "Restored from system tray.").to_string());
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
            self.status = Some(
                i18n::t(
                    self.language,
                    "Minimized to system tray. Scheduler remains active.",
                )
                .to_string(),
            );
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
            if let FirstRunOutcome::Submitted {
                llm,
                embedding,
                contact_email,
            } = self.first_run.show(ctx, self.language)
            {
                // Best-effort persist so the modal only fires once. A failed
                // write still lets the user continue in this session. We mark
                // setup_complete=false so a quit before the research step
                // resumes there next launch.
                let path = self.config.storage.settings_file.clone();
                let llm_to_save = llm.clone();
                let embedding_to_save = embedding.clone();
                let contact_to_save = contact_email.clone();
                let save_result = self.handle.block_on(async move {
                    let svc = SettingsService::new(path);
                    svc.set_llm_config(llm_to_save).await?;
                    svc.set_embedding_config(embedding_to_save).await?;
                    if let Some(email) = contact_to_save {
                        svc.set_contact_email(Some(email)).await?;
                    }
                    svc.set_setup_complete(false).await
                });
                if let Err(err) = save_result {
                    warn!("failed to persist first-run config: {err:#}");
                }
                self.config.llm = llm;
                self.config.embedding = embedding;
                if let Some(email) = contact_email {
                    self.config.contact_email = email;
                }
                self.activate();
            }
            return;
        }

        // Guided research-setup step (first launch only): blocks the main UI
        // until completed or skipped.
        if !self.setup_complete {
            self.show_research_setup(ctx);
            return;
        }

        self.maybe_refresh_workspaces();

        let mut pending_switch: Option<i64> = None;
        let workspace_items: Vec<(i64, String)> = self
            .workspaces
            .iter()
            .map(|w| (w.id, w.name.clone()))
            .collect();
        let active_ws = self.persistent.active_workspace_id;

        egui::TopBottomPanel::top("tabs").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(i18n::t(self.language, "Workspace:"));
                let mut selected = active_ws;
                let current = workspace_items
                    .iter()
                    .find(|(id, _)| *id == selected)
                    .map(|(_, name)| name.clone())
                    .unwrap_or_else(|| "—".to_string());
                egui::ComboBox::from_id_salt("workspace_switcher")
                    .selected_text(current)
                    .show_ui(ui, |ui| {
                        for (id, name) in &workspace_items {
                            ui.selectable_value(&mut selected, *id, name);
                        }
                    });
                if selected != active_ws {
                    pending_switch = Some(selected);
                }
            });
            ui.separator();
            ui.horizontal_wrapped(|ui| {
                for tab in Tab::ALL {
                    ui.selectable_value(
                        &mut self.persistent.active_tab,
                        tab,
                        format!("{}  {}", tab.icon(), tab.label_for(self.language)),
                    );
                }
            });
        });

        if let Some(new_id) = pending_switch {
            self.build_state_for_workspace(new_id);
        }

        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.label(self.status.as_deref().unwrap_or(""));
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            if let Some(state) = &self.state {
                let panel_ctx = PanelCtx {
                    state,
                    handle: &self.handle,
                    ui_tx: &self.ui_tx,
                    active_workspace_id: self.persistent.active_workspace_id,
                    language: self.language,
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
    // Phosphor icon glyphs, available as fallbacks in both families so the
    // icon constants render anywhere text does.
    egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);
    add_font_if_available(&mut fonts, "malgun_gothic", r"C:\Windows\Fonts\malgun.ttf");
    add_font_if_available(
        &mut fonts,
        "apple_sd_gothic",
        "/System/Library/Fonts/AppleSDGothicNeo.ttc",
    );
    add_font_if_available(
        &mut fonts,
        "noto_sans_cjk_kr",
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
    );
    add_font_if_available(
        &mut fonts,
        "noto_sans_cjk_kr_otf",
        "/usr/share/fonts/opentype/noto/NotoSansCJKkr-Regular.otf",
    );
    add_font_if_available(
        &mut fonts,
        "nanum_gothic",
        "/usr/share/fonts/truetype/nanum/NanumGothic.ttf",
    );
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
    // Registry of workspaces (meta DB) + the default workspace's data file
    // (the existing primary database). Other workspace files are created lazily
    // when first activated.
    let root = config
        .storage
        .database_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    let default_db_filename = config
        .storage
        .database_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("haie.db")
        .to_string();
    db::initialize_meta(root.join("meta.db"), default_db_filename).await?;
    db::initialize(config).await
}
