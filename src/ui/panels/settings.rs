use crate::{
    config::{EmbeddingConfig, LlmConfig, normalize_api_key},
    models::settings::{NewsletterSettings, SchedulerSettings, SettingsUpdate},
};

use super::{MsgChannel, PanelCtx};

enum Msg {
    Loaded {
        scheduler: SchedulerSettings,
        newsletter: NewsletterSettings,
        embedding_dimensions: Option<u32>,
    },
    LoadError(String),
    Saved(&'static str),
    SaveError(String),
}

#[derive(Default)]
pub struct Panel {
    channel: Option<MsgChannel<Msg>>,
    initialized: bool,
    loading: bool,

    scheduler: Option<SchedulerSettings>,
    // Held so scheduler-only saves don't clobber the persisted newsletter
    // defaults; a future newsletter UI will edit this directly.
    #[allow(dead_code)]
    newsletter: Option<NewsletterSettings>,

    llm_base_url: String,
    llm_model: String,
    llm_api_key: String,
    llm_dirty: bool,

    embed_base_url: String,
    embed_model: String,
    embed_api_key: String,
    embed_dirty: bool,

    embedding_dim_persisted: Option<u32>,
    embedding_dim_input: String,
    embedding_confirm_open: bool,

    notice: Option<(NoticeKind, String)>,
}

#[derive(Clone, Copy)]
enum NoticeKind {
    Success,
    Error,
}

impl Panel {
    pub fn show(&mut self, ui: &mut egui::Ui, ctx: &PanelCtx<'_>) {
        if self.channel.is_none() {
            self.channel = Some(MsgChannel::default());
        }
        self.drain();
        if !self.initialized {
            self.initialized = true;
            self.populate_from_state(ctx);
            self.spawn_load(ctx);
        }

        ui.heading("Settings");
        ui.separator();

        if self.loading && self.scheduler.is_none() {
            ui.label("Loading settings…");
            return;
        }

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                self.section_llm(ui, ctx);
                ui.add_space(8.0);
                ui.separator();
                self.section_embedding_endpoint(ui, ctx);
                ui.add_space(8.0);
                ui.separator();
                self.section_paths(ui, ctx);
                ui.add_space(8.0);
                ui.separator();
                self.section_scheduler(ui, ctx);
                ui.add_space(8.0);
                ui.separator();
                self.section_embedding(ui, ctx);

                ui.add_space(12.0);
                if let Some((kind, msg)) = &self.notice {
                    let color = match kind {
                        NoticeKind::Success => egui::Color32::from_rgb(0, 130, 0),
                        NoticeKind::Error => egui::Color32::RED,
                    };
                    ui.colored_label(color, msg);
                }
            });

        if self.embedding_confirm_open {
            self.show_embedding_confirm(ui.ctx(), ctx);
        }
    }

    fn drain(&mut self) {
        let Some(channel) = self.channel.as_mut() else {
            return;
        };
        while let Ok(msg) = channel.rx.try_recv() {
            match msg {
                Msg::Loaded {
                    scheduler,
                    newsletter,
                    embedding_dimensions,
                } => {
                    self.scheduler = Some(scheduler);
                    self.newsletter = Some(newsletter);
                    self.embedding_dim_persisted = embedding_dimensions;
                    if let Some(dim) = embedding_dimensions {
                        self.embedding_dim_input = dim.to_string();
                    }
                    self.loading = false;
                }
                Msg::LoadError(err) => {
                    self.loading = false;
                    self.notice =
                        Some((NoticeKind::Error, format!("Failed to load settings: {err}")));
                }
                Msg::Saved(what) => {
                    self.notice = Some((NoticeKind::Success, format!("{what} saved.")));
                    match what {
                        "LLM endpoint" => self.llm_dirty = false,
                        "Embedding endpoint" => self.embed_dirty = false,
                        _ => {}
                    }
                }
                Msg::SaveError(err) => {
                    self.notice = Some((NoticeKind::Error, format!("Save failed: {err}")));
                }
            }
        }
    }

    fn populate_from_state(&mut self, ctx: &PanelCtx<'_>) {
        let cfg = &ctx.state.config;
        self.llm_base_url = cfg.llm.base_url.clone();
        self.llm_model = cfg.llm.model.clone();
        self.llm_api_key = cfg.llm.api_key.clone();
        self.embed_base_url = cfg.embedding.base_url.clone();
        self.embed_model = cfg.embedding.model.clone();
        self.embed_api_key = cfg.embedding.api_key.clone();
        self.embedding_dim_input = cfg.embedding_dimensions.to_string();
    }

    fn spawn_load(&mut self, ctx: &PanelCtx<'_>) {
        self.loading = true;
        let Some(channel) = self.channel.as_ref() else {
            return;
        };
        let tx = channel.tx.clone();
        let svc = ctx.state.settings_service.clone();
        ctx.handle.spawn(async move {
            match svc.get_settings().await {
                Ok(resp) => {
                    let dim = svc.get_embedding_dimensions().await.ok().flatten();
                    let _ = tx.send(Msg::Loaded {
                        scheduler: resp.scheduler,
                        newsletter: resp.newsletter,
                        embedding_dimensions: dim,
                    });
                }
                Err(err) => {
                    let _ = tx.send(Msg::LoadError(err.to_string()));
                }
            }
        });
    }

    fn section_llm(&mut self, ui: &mut egui::Ui, ctx: &PanelCtx<'_>) {
        ui.heading("LLM endpoint");
        ui.label("Changes are saved to settings.json. Restart to apply to the running LLM client.");
        ui.add_space(4.0);

        egui::Grid::new("settings-llm-grid")
            .num_columns(2)
            .spacing([8.0, 6.0])
            .show(ui, |ui| {
                ui.label("Base URL");
                if ui.text_edit_singleline(&mut self.llm_base_url).changed() {
                    self.llm_dirty = true;
                }
                ui.end_row();

                ui.label("Model");
                if ui.text_edit_singleline(&mut self.llm_model).changed() {
                    self.llm_dirty = true;
                }
                ui.end_row();

                ui.label("API key");
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut self.llm_api_key)
                        .password(true)
                        .hint_text("(unchanged)"),
                );
                if resp.changed() {
                    self.llm_dirty = true;
                }
                ui.end_row();
            });

        ui.horizontal(|ui| {
            let save_enabled = self.llm_dirty
                && !self.llm_base_url.trim().is_empty()
                && !self.llm_model.trim().is_empty();
            if ui
                .add_enabled(save_enabled, egui::Button::new("Save LLM endpoint"))
                .clicked()
            {
                self.save_llm(ctx);
            }
            if self.llm_dirty {
                ui.label(egui::RichText::new("unsaved changes").italics());
            }
        });
    }

    fn section_embedding_endpoint(&mut self, ui: &mut egui::Ui, ctx: &PanelCtx<'_>) {
        ui.heading("Embedding endpoint");
        ui.label("Used to embed article chunks for semantic + hybrid search. Restart to apply.");
        ui.add_space(4.0);

        egui::Grid::new("settings-embed-endpoint-grid")
            .num_columns(2)
            .spacing([8.0, 6.0])
            .show(ui, |ui| {
                ui.label("Base URL");
                if ui.text_edit_singleline(&mut self.embed_base_url).changed() {
                    self.embed_dirty = true;
                }
                ui.end_row();

                ui.label("Model");
                if ui.text_edit_singleline(&mut self.embed_model).changed() {
                    self.embed_dirty = true;
                }
                ui.end_row();

                ui.label("API key");
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut self.embed_api_key)
                        .password(true)
                        .hint_text("(leave blank for local servers)"),
                );
                if resp.changed() {
                    self.embed_dirty = true;
                }
                ui.end_row();
            });

        ui.horizontal(|ui| {
            let save_enabled = self.embed_dirty
                && !self.embed_base_url.trim().is_empty()
                && !self.embed_model.trim().is_empty();
            if ui
                .add_enabled(save_enabled, egui::Button::new("Save embedding endpoint"))
                .clicked()
            {
                self.save_embedding_endpoint(ctx);
            }
            if self.embed_dirty {
                ui.label(egui::RichText::new("unsaved changes").italics());
            }
        });
    }

    fn section_paths(&mut self, ui: &mut egui::Ui, ctx: &PanelCtx<'_>) {
        ui.heading("Paths");
        let storage = &ctx.state.config.storage;
        egui::Grid::new("settings-paths-grid")
            .num_columns(2)
            .spacing([8.0, 4.0])
            .show(ui, |ui| {
                path_row(ui, "Database", &storage.database_path);
                path_row(ui, "Prompts", &storage.prompts_dir);
                path_row(ui, "Wiki export", &storage.wiki_export_dir);
                path_row(ui, "Settings file", &storage.settings_file);
            });
    }

    fn section_scheduler(&mut self, ui: &mut egui::Ui, ctx: &PanelCtx<'_>) {
        ui.heading("Scheduler");
        let Some(sched) = self.scheduler.as_mut() else {
            ui.label("(unavailable)");
            return;
        };

        let mut changed = ui
            .checkbox(&mut sched.enabled, "Enable scheduled gathers")
            .changed();

        ui.add_space(4.0);
        ui.label("Daily schedule (KST, 24h)");
        egui::Grid::new("settings-sched-grid")
            .num_columns(3)
            .spacing([8.0, 4.0])
            .show(ui, |ui| {
                ui.label("Source");
                ui.label("Hour");
                ui.label("Minute");
                ui.end_row();

                changed |= hm_row(
                    ui,
                    "arXiv",
                    &mut sched.arxiv_schedule_hour,
                    &mut sched.arxiv_schedule_minute,
                );
                changed |= hm_row(
                    ui,
                    "PMC",
                    &mut sched.pmc_schedule_hour,
                    &mut sched.pmc_schedule_minute,
                );
                changed |= hm_row(
                    ui,
                    "PubMed",
                    &mut sched.pubmed_schedule_hour,
                    &mut sched.pubmed_schedule_minute,
                );
            });

        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if ui.button("Save scheduler").clicked() {
                self.save_scheduler(ctx);
            }
            if changed {
                ui.label(egui::RichText::new("unsaved changes").italics());
            }
        });
    }

    fn section_embedding(&mut self, ui: &mut egui::Ui, ctx: &PanelCtx<'_>) {
        ui.heading("Embeddings");
        ui.label(format!(
            "Current dimension: {}",
            ctx.state.config.embedding_dimensions
        ));
        if let Some(dim) = self.embedding_dim_persisted
            && dim != ctx.state.config.embedding_dimensions
        {
            ui.colored_label(
                egui::Color32::from_rgb(180, 120, 0),
                format!("Persisted override: {dim} (restart to apply)"),
            );
        }

        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label("New dimension:");
            ui.add(egui::TextEdit::singleline(&mut self.embedding_dim_input).desired_width(80.0));
            let parsed = self.embedding_dim_input.trim().parse::<u32>().ok();
            let new_dim_differs =
                parsed.is_some_and(|d| d != ctx.state.config.embedding_dimensions);
            let enabled = parsed.is_some_and(|d| (1..=8192).contains(&d)) && new_dim_differs;
            if ui
                .add_enabled(enabled, egui::Button::new("Change…"))
                .clicked()
            {
                self.embedding_confirm_open = true;
            }
        });
    }

    fn show_embedding_confirm(&mut self, egui_ctx: &egui::Context, ctx: &PanelCtx<'_>) {
        let mut close = false;
        let mut confirm = false;
        let new_dim = self.embedding_dim_input.trim().parse::<u32>().unwrap_or(0);

        egui::Window::new("Confirm dimension change")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(egui_ctx, |ui| {
                ui.label(
                    "Changing the embedding dimension drops the existing vector \
                     table on the next startup. All article and entity embeddings \
                     will need to be regenerated from scratch.",
                );
                ui.add_space(6.0);
                ui.label(format!(
                    "Current: {} → New: {new_dim}",
                    ctx.state.config.embedding_dimensions,
                ));
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                    if ui
                        .add(
                            egui::Button::new(
                                egui::RichText::new("Drop embeddings and save")
                                    .color(egui::Color32::WHITE),
                            )
                            .fill(egui::Color32::from_rgb(160, 30, 30)),
                        )
                        .clicked()
                    {
                        confirm = true;
                    }
                });
            });

        if confirm {
            self.save_embedding_dim(ctx, new_dim);
            close = true;
        }
        if close {
            self.embedding_confirm_open = false;
        }
    }

    fn save_llm(&mut self, ctx: &PanelCtx<'_>) {
        let Some(channel) = self.channel.as_ref() else {
            return;
        };
        let tx = channel.tx.clone();
        let mut new_llm: LlmConfig = ctx.state.config.llm.clone();
        new_llm.base_url = self.llm_base_url.trim().trim_end_matches('/').to_string();
        new_llm.model = self.llm_model.trim().to_string();
        new_llm.api_key = normalize_api_key(&self.llm_api_key);
        let svc = ctx.state.settings_service.clone();
        ctx.handle.spawn(async move {
            let result = svc.set_llm_config(new_llm).await;
            let _ = match result {
                Ok(()) => tx.send(Msg::Saved("LLM endpoint")),
                Err(err) => tx.send(Msg::SaveError(err.to_string())),
            };
        });
    }

    fn save_embedding_endpoint(&mut self, ctx: &PanelCtx<'_>) {
        let Some(channel) = self.channel.as_ref() else {
            return;
        };
        let tx = channel.tx.clone();
        let new_embed = EmbeddingConfig {
            base_url: self.embed_base_url.trim().trim_end_matches('/').to_string(),
            model: self.embed_model.trim().to_string(),
            api_key: normalize_api_key(&self.embed_api_key),
        };
        let svc = ctx.state.settings_service.clone();
        ctx.handle.spawn(async move {
            let result = svc.set_embedding_config(new_embed).await;
            let _ = match result {
                Ok(()) => tx.send(Msg::Saved("Embedding endpoint")),
                Err(err) => tx.send(Msg::SaveError(err.to_string())),
            };
        });
    }

    fn save_scheduler(&mut self, ctx: &PanelCtx<'_>) {
        let Some(channel) = self.channel.as_ref() else {
            return;
        };
        let Some(sched) = self.scheduler.clone() else {
            return;
        };
        let tx = channel.tx.clone();
        let svc = ctx.state.settings_service.clone();
        let update = SettingsUpdate {
            scheduler: Some(sched),
            newsletter: None,
        };
        ctx.handle.spawn(async move {
            let result = svc.update_settings(update).await;
            let _ = match result {
                Ok(()) => tx.send(Msg::Saved("Scheduler")),
                Err(err) => tx.send(Msg::SaveError(err.to_string())),
            };
        });
    }

    fn save_embedding_dim(&mut self, ctx: &PanelCtx<'_>, new_dim: u32) {
        let Some(channel) = self.channel.as_ref() else {
            return;
        };
        let tx = channel.tx.clone();
        let svc = ctx.state.settings_service.clone();
        ctx.handle.spawn(async move {
            let result = svc.set_embedding_dimensions(new_dim).await;
            let _ = match result {
                Ok(()) => tx.send(Msg::Saved("Embedding dimension")),
                Err(err) => tx.send(Msg::SaveError(err.to_string())),
            };
        });
        self.embedding_dim_persisted = Some(new_dim);
    }
}

fn path_row(ui: &mut egui::Ui, label: &str, path: &std::path::Path) {
    ui.label(label);
    let display = path.display().to_string();
    ui.horizontal(|ui| {
        ui.add(egui::Label::new(egui::RichText::new(&display).monospace()).truncate());
        if ui.small_button("Copy").clicked() {
            ui.ctx().copy_text(display.clone());
        }
        if path.exists() && ui.small_button("Open folder").clicked() {
            let target = if path.is_dir() {
                path.to_path_buf()
            } else {
                path.parent()
                    .map(std::path::Path::to_path_buf)
                    .unwrap_or_else(|| path.to_path_buf())
            };
            let _ = open_in_file_manager(&target);
        }
    });
    ui.end_row();
}

fn hm_row(ui: &mut egui::Ui, label: &str, hour: &mut u8, minute: &mut u8) -> bool {
    ui.label(label);
    let h_changed = ui
        .add(egui::DragValue::new(hour).range(0..=23).speed(0.1))
        .changed();
    let m_changed = ui
        .add(egui::DragValue::new(minute).range(0..=59).speed(0.5))
        .changed();
    ui.end_row();
    h_changed || m_changed
}

fn open_in_file_manager(path: &std::path::Path) -> std::io::Result<()> {
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("explorer")
            .arg(path)
            .spawn()
            .map(|_| ())
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(path)
            .spawn()
            .map(|_| ())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::process::Command::new("xdg-open")
            .arg(path)
            .spawn()
            .map(|_| ())
    }
}
