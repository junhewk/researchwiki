use crate::{
    config::{EmbeddingConfig, LlmConfig, normalize_api_key},
    models::settings::{NewsletterSettings, UiLanguage},
    runtime::UiEvent,
    ui::{style, toast::ToastKind},
};

use super::{MsgChannel, PanelCtx};

enum Msg {
    Loaded {
        newsletter: NewsletterSettings,
        embedding_dimensions: Option<u32>,
        ui_language: UiLanguage,
    },
    LoadError(String),
    Saved(&'static str),
    SaveError(String),
}

#[derive(Default)]
pub struct Panel {
    channel: Option<MsgChannel<Msg>>,
    initialized: bool,
    /// Tracks the workspace whose `AppState` the form was populated from, so a
    /// switch re-populates — unless the user has unsaved edits.
    loaded_workspace: Option<i64>,
    loading: bool,

    ui_language: UiLanguage,
    // Loaded so a future newsletter UI can edit it; also drives the initial
    // "Loading settings…" guard.
    newsletter: Option<NewsletterSettings>,

    llm_base_url: String,
    llm_model: String,
    llm_api_key: String,
    llm_key_revealed: bool,
    llm_dirty: bool,

    embed_base_url: String,
    embed_model: String,
    embed_api_key: String,
    embed_key_revealed: bool,
    embed_dirty: bool,

    contact_email: String,
    contact_email_dirty: bool,

    semantic_scholar_api_key: String,
    s2_key_revealed: bool,
    s2_dirty: bool,

    embedding_dim_persisted: Option<u32>,
    embedding_dim_input: String,
    embedding_confirm_open: bool,

    /// Which section's save is in flight (the `Msg::Saved` label); all save
    /// buttons disable until it completes.
    saving: Option<&'static str>,
    /// Persistent error from the last failed load/save. Successes toast.
    error: Option<String>,
}

impl Panel {
    pub fn show(&mut self, ui: &mut egui::Ui, ctx: &PanelCtx<'_>) {
        if self.channel.is_none() {
            self.channel = Some(MsgChannel::default());
        }
        self.drain(ctx);
        if !self.initialized {
            self.initialized = true;
            self.loaded_workspace = Some(ctx.active_workspace_id);
            self.populate_from_state(ctx);
            self.spawn_load(ctx);
        } else if self.loaded_workspace != Some(ctx.active_workspace_id) {
            self.loaded_workspace = Some(ctx.active_workspace_id);
            let dirty =
                self.llm_dirty || self.embed_dirty || self.contact_email_dirty || self.s2_dirty;
            if !dirty {
                self.populate_from_state(ctx);
                self.spawn_load(ctx);
            }
        }

        style::panel_header_icon(ui, style::icon::GEAR, ctx.t("Settings"), None);

        if self.loading && self.newsletter.is_none() {
            ui.label(ctx.t("Loading settings..."));
            return;
        }

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                self.section_interface(ui, ctx);
                ui.add_space(8.0);
                ui.separator();
                self.section_llm(ui, ctx);
                ui.add_space(8.0);
                ui.separator();
                self.section_embedding_endpoint(ui, ctx);
                ui.add_space(8.0);
                ui.separator();
                self.section_contact_email(ui, ctx);
                ui.add_space(8.0);
                ui.separator();
                self.section_semantic_scholar(ui, ctx);
                ui.add_space(8.0);
                ui.separator();
                self.section_paths(ui, ctx);
                ui.add_space(8.0);
                ui.separator();
                self.section_embedding(ui, ctx);

                ui.add_space(12.0);
                if let Some(err) = self.error.clone()
                    && style::error_notice(ui, &err, None) == style::NoticeAction::Dismiss
                {
                    self.error = None;
                }
            });

        if self.embedding_confirm_open {
            self.show_embedding_confirm(ui.ctx(), ctx);
        }
    }

    fn drain(&mut self, ctx: &PanelCtx<'_>) {
        let Some(channel) = self.channel.as_mut() else {
            return;
        };
        while let Ok(msg) = channel.rx.try_recv() {
            match msg {
                Msg::Loaded {
                    newsletter,
                    embedding_dimensions,
                    ui_language,
                } => {
                    self.ui_language = ui_language;
                    self.newsletter = Some(newsletter);
                    self.embedding_dim_persisted = embedding_dimensions;
                    if let Some(dim) = embedding_dimensions {
                        self.embedding_dim_input = dim.to_string();
                    }
                    self.loading = false;
                }
                Msg::LoadError(err) => {
                    self.loading = false;
                    self.error = Some(format!("Failed to load settings: {err}"));
                }
                Msg::Saved(what) => {
                    self.saving = None;
                    let _ = ctx.ui_tx.send(UiEvent::Toast {
                        kind: ToastKind::Success,
                        message: format!("{what} saved."),
                    });
                    match what {
                        "LLM endpoint" => self.llm_dirty = false,
                        "Embedding endpoint" => self.embed_dirty = false,
                        "Contact email" => self.contact_email_dirty = false,
                        "Semantic Scholar key" => self.s2_dirty = false,
                        _ => {}
                    }
                }
                Msg::SaveError(err) => {
                    self.saving = None;
                    self.error = Some(format!("Save failed: {err}"));
                }
            }
        }
    }

    fn populate_from_state(&mut self, ctx: &PanelCtx<'_>) {
        let cfg = &ctx.state.config;
        self.ui_language = ctx.language;
        self.llm_base_url = cfg.llm.base_url.clone();
        self.llm_model = cfg.llm.model.clone();
        self.llm_api_key = cfg.llm.api_key.clone();
        self.embed_base_url = cfg.embedding.base_url.clone();
        self.embed_model = cfg.embedding.model.clone();
        self.embed_api_key = cfg.embedding.api_key.clone();
        self.contact_email = cfg.contact_email.clone();
        self.semantic_scholar_api_key = cfg.semantic_scholar_api_key.clone();
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
                        newsletter: resp.newsletter,
                        embedding_dimensions: dim,
                        ui_language: resp.ui_language,
                    });
                }
                Err(err) => {
                    let _ = tx.send(Msg::LoadError(err.to_string()));
                }
            }
        });
    }

    fn section_interface(&mut self, ui: &mut egui::Ui, ctx: &PanelCtx<'_>) {
        style::section_heading(ui, ctx.t("Interface"));
        ui.horizontal(|ui| {
            ui.label(ctx.t("Language"));
            let mut next = self.ui_language;
            egui::ComboBox::from_id_salt("settings-language-combo")
                .selected_text(next.label())
                .show_ui(ui, |ui| {
                    for language in UiLanguage::ALL {
                        ui.selectable_value(&mut next, language, language.label());
                    }
                });
            if next != self.ui_language {
                self.ui_language = next;
                self.save_language(ctx, next);
            }
        });
    }

    fn section_llm(&mut self, ui: &mut egui::Ui, ctx: &PanelCtx<'_>) {
        style::section_heading(ui, ctx.t("LLM endpoint"));
        style::muted_label(
            ui,
            ctx.t(
                "Changes are saved to settings.json. Restart to apply to the running LLM client.",
            ),
        );
        ui.add_space(4.0);

        egui::Grid::new("settings-llm-grid")
            .num_columns(2)
            .spacing([8.0, 6.0])
            .show(ui, |ui| {
                ui.label(ctx.t("Base URL"));
                if ui.text_edit_singleline(&mut self.llm_base_url).changed() {
                    self.llm_dirty = true;
                }
                ui.end_row();

                ui.label(ctx.t("Model"));
                if ui.text_edit_singleline(&mut self.llm_model).changed() {
                    self.llm_dirty = true;
                }
                ui.end_row();

                ui.label(ctx.t("API key"));
                let resp = style::secret_edit(
                    ui,
                    &mut self.llm_api_key,
                    &mut self.llm_key_revealed,
                    ctx.t("(no key set)"),
                );
                if resp.changed() {
                    self.llm_dirty = true;
                }
                ui.end_row();
            });

        ui.horizontal(|ui| {
            let save_enabled = self.llm_dirty
                && self.saving.is_none()
                && !self.llm_base_url.trim().is_empty()
                && !self.llm_model.trim().is_empty();
            if ui
                .add_enabled(save_enabled, egui::Button::new(ctx.t("Save LLM endpoint")))
                .clicked()
            {
                self.save_llm(ctx);
            }
            if self.saving == Some("LLM endpoint") {
                style::loading_indicator(ui, ctx.t("Saving…"));
            } else if self.llm_dirty {
                ui.label(egui::RichText::new(ctx.t("unsaved changes")).italics());
            }
        });
    }

    fn section_contact_email(&mut self, ui: &mut egui::Ui, ctx: &PanelCtx<'_>) {
        style::section_heading(ui, ctx.t("Contact email"));
        style::muted_label(
            ui,
            ctx.t(
                "Sent to scholarly APIs (OpenAlex, Crossref, Unpaywall). Required for Unpaywall; leave blank to skip it. Restart to apply.",
            ),
        );
        ui.add_space(4.0);

        egui::Grid::new("settings-contact-grid")
            .num_columns(2)
            .spacing([8.0, 6.0])
            .show(ui, |ui| {
                ui.label(ctx.t("Email"));
                if ui
                    .add(
                        egui::TextEdit::singleline(&mut self.contact_email)
                            .hint_text("you@example.com"),
                    )
                    .changed()
                {
                    self.contact_email_dirty = true;
                }
                ui.end_row();
            });

        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    self.contact_email_dirty && self.saving.is_none(),
                    egui::Button::new(ctx.t("Save contact email")),
                )
                .clicked()
            {
                self.save_contact_email(ctx);
            }
            if self.saving == Some("Contact email") {
                style::loading_indicator(ui, ctx.t("Saving…"));
            } else if self.contact_email_dirty {
                ui.label(egui::RichText::new(ctx.t("unsaved changes")).italics());
            }
        });
    }

    fn section_semantic_scholar(&mut self, ui: &mut egui::Ui, ctx: &PanelCtx<'_>) {
        style::section_heading(ui, ctx.t("Semantic Scholar API key"));
        style::muted_label(
            ui,
            ctx.t(
                "Optional. The Semantic Scholar gather source only runs when a key is set (its keyless tier is rate-limited). Get one free at semanticscholar.org. Restart to apply.",
            ),
        );
        ui.add_space(4.0);

        egui::Grid::new("settings-s2-grid")
            .num_columns(2)
            .spacing([8.0, 6.0])
            .show(ui, |ui| {
                ui.label(ctx.t("API key"));
                if style::secret_edit(
                    ui,
                    &mut self.semantic_scholar_api_key,
                    &mut self.s2_key_revealed,
                    ctx.t("(leave blank to skip Semantic Scholar)"),
                )
                .changed()
                {
                    self.s2_dirty = true;
                }
                ui.end_row();
            });

        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    self.s2_dirty && self.saving.is_none(),
                    egui::Button::new(ctx.t("Save key")),
                )
                .clicked()
            {
                self.save_semantic_scholar(ctx);
            }
            if self.saving == Some("Semantic Scholar key") {
                style::loading_indicator(ui, ctx.t("Saving…"));
            } else if self.s2_dirty {
                ui.label(egui::RichText::new(ctx.t("unsaved changes")).italics());
            }
        });
    }

    fn section_embedding_endpoint(&mut self, ui: &mut egui::Ui, ctx: &PanelCtx<'_>) {
        style::section_heading(ui, ctx.t("Embedding endpoint"));
        style::muted_label(
            ui,
            ctx.t("Used to embed article chunks for semantic + hybrid search. Restart to apply."),
        );
        ui.add_space(4.0);

        egui::Grid::new("settings-embed-endpoint-grid")
            .num_columns(2)
            .spacing([8.0, 6.0])
            .show(ui, |ui| {
                ui.label(ctx.t("Base URL"));
                if ui.text_edit_singleline(&mut self.embed_base_url).changed() {
                    self.embed_dirty = true;
                }
                ui.end_row();

                ui.label(ctx.t("Model"));
                if ui.text_edit_singleline(&mut self.embed_model).changed() {
                    self.embed_dirty = true;
                }
                ui.end_row();

                ui.label(ctx.t("API key"));
                let resp = style::secret_edit(
                    ui,
                    &mut self.embed_api_key,
                    &mut self.embed_key_revealed,
                    ctx.t("(leave blank for local servers)"),
                );
                if resp.changed() {
                    self.embed_dirty = true;
                }
                ui.end_row();
            });

        ui.horizontal(|ui| {
            let save_enabled = self.embed_dirty
                && self.saving.is_none()
                && !self.embed_base_url.trim().is_empty()
                && !self.embed_model.trim().is_empty();
            if ui
                .add_enabled(
                    save_enabled,
                    egui::Button::new(ctx.t("Save embedding endpoint")),
                )
                .clicked()
            {
                self.save_embedding_endpoint(ctx);
            }
            if self.saving == Some("Embedding endpoint") {
                style::loading_indicator(ui, ctx.t("Saving…"));
            } else if self.embed_dirty {
                ui.label(egui::RichText::new(ctx.t("unsaved changes")).italics());
            }
        });
    }

    fn section_paths(&mut self, ui: &mut egui::Ui, ctx: &PanelCtx<'_>) {
        style::section_heading(ui, ctx.t("Paths"));
        let storage = &ctx.state.config.storage;
        egui::Grid::new("settings-paths-grid")
            .num_columns(2)
            .spacing([8.0, 4.0])
            .show(ui, |ui| {
                path_row(ui, ctx, "Database", &storage.database_path);
                path_row(ui, ctx, "Prompts", &storage.prompts_dir);
                path_row(ui, ctx, "Wiki export", &storage.wiki_export_dir);
                path_row(ui, ctx, "Settings file", &storage.settings_file);
            });
    }

    fn section_embedding(&mut self, ui: &mut egui::Ui, ctx: &PanelCtx<'_>) {
        style::section_heading(ui, ctx.t("Embeddings"));
        ui.label(format!(
            "{} {}",
            ctx.t("Current dimension:"),
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
            ui.label(ctx.t("New dimension:"));
            ui.add(egui::TextEdit::singleline(&mut self.embedding_dim_input).desired_width(80.0));
            let parsed = self.embedding_dim_input.trim().parse::<u32>().ok();
            let new_dim_differs =
                parsed.is_some_and(|d| d != ctx.state.config.embedding_dimensions);
            let enabled = parsed.is_some_and(|d| (1..=8192).contains(&d)) && new_dim_differs;
            if ui
                .add_enabled(enabled, egui::Button::new(ctx.t("Change...")))
                .clicked()
            {
                self.embedding_confirm_open = true;
            }
        });
    }

    fn show_embedding_confirm(&mut self, egui_ctx: &egui::Context, ctx: &PanelCtx<'_>) {
        if egui_ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.embedding_confirm_open = false;
            return;
        }
        let mut close = false;
        let mut confirm = false;
        let new_dim = self.embedding_dim_input.trim().parse::<u32>().unwrap_or(0);

        egui::Window::new(ctx.t("Confirm dimension change"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(egui_ctx, |ui| {
                ui.label(ctx.t(
                    "Changing the embedding dimension drops the existing vector \
                     table on the next startup. All article and entity embeddings \
                     will need to be regenerated from scratch.",
                ));
                ui.add_space(6.0);
                ui.label(format!(
                    "Current: {} -> New: {new_dim}",
                    ctx.state.config.embedding_dimensions,
                ));
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.button(ctx.t("Cancel")).clicked() {
                        close = true;
                    }
                    if ui
                        .add(
                            egui::Button::new(
                                egui::RichText::new(ctx.t("Drop embeddings and save"))
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
        self.saving = Some("LLM endpoint");
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

    fn save_contact_email(&mut self, ctx: &PanelCtx<'_>) {
        let Some(channel) = self.channel.as_ref() else {
            return;
        };
        self.saving = Some("Contact email");
        let tx = channel.tx.clone();
        let email = {
            let trimmed = self.contact_email.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        };
        let svc = ctx.state.settings_service.clone();
        ctx.handle.spawn(async move {
            let result = svc.set_contact_email(email).await;
            let _ = match result {
                Ok(()) => tx.send(Msg::Saved("Contact email")),
                Err(err) => tx.send(Msg::SaveError(err.to_string())),
            };
        });
    }

    fn save_semantic_scholar(&mut self, ctx: &PanelCtx<'_>) {
        let Some(channel) = self.channel.as_ref() else {
            return;
        };
        self.saving = Some("Semantic Scholar key");
        let tx = channel.tx.clone();
        let key = {
            let trimmed = self.semantic_scholar_api_key.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        };
        let svc = ctx.state.settings_service.clone();
        ctx.handle.spawn(async move {
            let result = svc.set_semantic_scholar_api_key(key).await;
            let _ = match result {
                Ok(()) => tx.send(Msg::Saved("Semantic Scholar key")),
                Err(err) => tx.send(Msg::SaveError(err.to_string())),
            };
        });
    }

    fn save_language(&mut self, ctx: &PanelCtx<'_>, language: UiLanguage) {
        let Some(channel) = self.channel.as_ref() else {
            return;
        };
        let tx = channel.tx.clone();
        let ui_tx = ctx.ui_tx.clone();
        let svc = ctx.state.settings_service.clone();
        ctx.handle.spawn(async move {
            let result = svc.set_ui_language(language).await;
            let _ = match result {
                Ok(()) => {
                    let _ = ui_tx.send(UiEvent::LanguageChanged(language));
                    tx.send(Msg::Saved("Language"))
                }
                Err(err) => tx.send(Msg::SaveError(err.to_string())),
            };
        });
    }

    fn save_embedding_endpoint(&mut self, ctx: &PanelCtx<'_>) {
        let Some(channel) = self.channel.as_ref() else {
            return;
        };
        self.saving = Some("Embedding endpoint");
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

    fn save_embedding_dim(&mut self, ctx: &PanelCtx<'_>, new_dim: u32) {
        let Some(channel) = self.channel.as_ref() else {
            return;
        };
        self.saving = Some("Embedding dimension");
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

fn path_row(ui: &mut egui::Ui, ctx: &PanelCtx<'_>, label: &'static str, path: &std::path::Path) {
    ui.label(ctx.t(label));
    let display = path.display().to_string();
    ui.horizontal(|ui| {
        ui.add(egui::Label::new(egui::RichText::new(&display).monospace()).truncate());
        if ui.small_button(ctx.t("Copy")).clicked() {
            ui.ctx().copy_text(display.clone());
        }
        if path.exists() && ui.small_button(ctx.t("Open folder")).clicked() {
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
