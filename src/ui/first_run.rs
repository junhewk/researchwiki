use crate::{
    config::{
        AppConfig, EmbeddingConfig, EmbeddingProvider, LlmConfig, LlmProvider, custom_model_label,
        infer_embedding_provider, infer_llm_provider, normalize_api_key,
    },
    models::settings::UiLanguage,
    ui::{i18n, style},
};

/// Step 1 of the setup wizard: connect the two OpenAI-compatible endpoints and
/// (optionally) provide a contact email for polite-pool/Unpaywall requests.
pub struct FirstRunForm {
    pub llm_provider: LlmProvider,
    pub llm_base_url: String,
    pub llm_model: String,
    pub llm_api_key: String,
    pub embed_provider: EmbeddingProvider,
    pub embed_base_url: String,
    pub embed_model: String,
    pub embed_api_key: String,
    pub contact_email: String,
    pub error: Option<String>,
    llm_key_revealed: bool,
    embed_key_revealed: bool,
}

impl Default for FirstRunForm {
    fn default() -> Self {
        let llm_provider = LlmProvider::Openai;
        let embed_defaults = EmbeddingConfig::default();
        Self {
            llm_provider,
            llm_base_url: llm_provider.default_base_url().to_string(),
            llm_model: llm_provider.default_model().to_string(),
            llm_api_key: String::new(),
            embed_provider: embed_defaults.effective_provider(),
            embed_base_url: embed_defaults.base_url,
            embed_model: embed_defaults.model,
            embed_api_key: String::new(),
            contact_email: String::new(),
            error: None,
            llm_key_revealed: false,
            embed_key_revealed: false,
        }
    }
}

/// True when a Base URL is filled in but missing the scheme `validate()` will
/// reject — used for a live inline warning before submit.
fn missing_scheme(value: &str) -> bool {
    let v = value.trim();
    !(v.is_empty() || v.starts_with("http://") || v.starts_with("https://"))
}

fn scheme_warning(ui: &mut egui::Ui, language: UiLanguage) {
    ui.label(
        egui::RichText::new(i18n::t(language, "Must start with http:// or https://"))
            .size(style::HELP_TEXT_SIZE)
            .color(style::color::WARNING),
    );
}

// Built once and consumed immediately on submit, so the size gap doesn't matter.
#[allow(clippy::large_enum_variant)]
pub enum FirstRunOutcome {
    Pending,
    Submitted {
        llm: LlmConfig,
        embedding: EmbeddingConfig,
        embedding_dimensions: Option<u32>,
        contact_email: Option<String>,
    },
}

impl FirstRunForm {
    /// Pre-fills from an existing (possibly invalid) config so a user routed
    /// back to setup can fix what's wrong instead of retyping everything.
    pub fn prefill_from(&mut self, config: &AppConfig) {
        self.llm_provider = config.llm.effective_provider();
        if !config.llm.base_url.is_empty() {
            self.llm_base_url = config.llm.base_url.clone();
        }
        if !config.llm.model.is_empty() {
            self.llm_model = config.llm.model.clone();
        }
        if !config.llm.api_key.is_empty() {
            self.llm_api_key = config.llm.api_key.clone();
        }
        self.embed_provider = config.embedding.effective_provider();
        if !config.embedding.base_url.is_empty() {
            self.embed_base_url = config.embedding.base_url.clone();
        }
        if !config.embedding.model.is_empty() {
            self.embed_model = config.embedding.model.clone();
        }
        if !config.embedding.api_key.is_empty() {
            self.embed_api_key = config.embedding.api_key.clone();
        }
        if !config.contact_email.is_empty() {
            self.contact_email = config.contact_email.clone();
        }
        self.error = config.validation_error();
    }

    pub fn show(&mut self, ctx: &egui::Context, language: UiLanguage) -> FirstRunOutcome {
        let mut outcome = FirstRunOutcome::Pending;

        egui::Window::new(i18n::t(language, "Welcome to ResearchWiki"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                style::muted_label(ui, i18n::t(language, "Step 1 of 2 · Connect"));
                style::body_label(
                    ui,
                    i18n::t(
                        language,
                        "Configure the two OpenAI-compatible endpoints ResearchWiki uses.",
                    ),
                );
                style::muted_label(
                    ui,
                    i18n::t(language, "You can change either later in Settings."),
                );
                ui.add_space(10.0);

                style::section_heading(ui, i18n::t(language, "LLM endpoint"));
                style::muted_label(
                    ui,
                    i18n::t(
                        language,
                        "Used for evaluation, screening, knowledge-graph extraction, etc.",
                    ),
                );
                egui::Grid::new("first-run-llm-grid")
                    .num_columns(2)
                    .spacing([8.0, 6.0])
                    .show(ui, |ui| {
                        ui.label(i18n::t(language, "Provider"));
                        let old_provider = self.llm_provider;
                        egui::ComboBox::from_id_salt("first-run-llm-provider")
                            .selected_text(self.llm_provider.label())
                            .show_ui(ui, |ui| {
                                for provider in LlmProvider::ALL {
                                    ui.selectable_value(
                                        &mut self.llm_provider,
                                        provider,
                                        provider.label(),
                                    );
                                }
                            });
                        if self.llm_provider != old_provider {
                            apply_llm_provider_defaults(
                                old_provider,
                                self.llm_provider,
                                &mut self.llm_base_url,
                                &mut self.llm_model,
                            );
                        }
                        ui.end_row();

                        ui.label(i18n::t(language, "Base URL"));
                        if ui.text_edit_singleline(&mut self.llm_base_url).changed()
                            && let Some(provider) = infer_llm_provider(&self.llm_base_url)
                        {
                            self.llm_provider = provider;
                        }
                        ui.end_row();
                        if missing_scheme(&self.llm_base_url) {
                            ui.label("");
                            scheme_warning(ui, language);
                            ui.end_row();
                        }

                        ui.label(i18n::t(language, "Model"));
                        model_combo(
                            ui,
                            "first-run-llm-model",
                            self.llm_provider.model_presets(),
                            &mut self.llm_model,
                            i18n::t(language, custom_model_label()),
                        );
                        ui.end_row();

                        ui.label(i18n::t(language, "API key"));
                        style::secret_edit(ui, &mut self.llm_api_key, &mut self.llm_key_revealed, "");
                        ui.end_row();
                    });

                ui.add_space(12.0);
                style::section_heading(ui, i18n::t(language, "Embedding endpoint"));
                style::muted_label(
                    ui,
                    i18n::t(
                        language,
                        "Used to embed article chunks for semantic + hybrid search.",
                    ),
                );
                egui::Grid::new("first-run-embed-grid")
                    .num_columns(2)
                    .spacing([8.0, 6.0])
                    .show(ui, |ui| {
                        ui.label(i18n::t(language, "Provider"));
                        let old_provider = self.embed_provider;
                        egui::ComboBox::from_id_salt("first-run-embed-provider")
                            .selected_text(self.embed_provider.label())
                            .show_ui(ui, |ui| {
                                for provider in EmbeddingProvider::ALL {
                                    ui.selectable_value(
                                        &mut self.embed_provider,
                                        provider,
                                        provider.label(),
                                    );
                                }
                            });
                        if self.embed_provider != old_provider {
                            apply_embedding_provider_defaults(
                                old_provider,
                                self.embed_provider,
                                &mut self.embed_base_url,
                                &mut self.embed_model,
                            );
                        }
                        ui.end_row();

                        ui.label(i18n::t(language, "Base URL"));
                        if ui.text_edit_singleline(&mut self.embed_base_url).changed()
                            && let Some(provider) =
                                infer_embedding_provider(&self.embed_base_url)
                        {
                            self.embed_provider = provider;
                        }
                        ui.end_row();
                        if missing_scheme(&self.embed_base_url) {
                            ui.label("");
                            scheme_warning(ui, language);
                            ui.end_row();
                        }

                        ui.label(i18n::t(language, "Model"));
                        model_combo(
                            ui,
                            "first-run-embed-model",
                            self.embed_provider.model_presets(),
                            &mut self.embed_model,
                            i18n::t(language, custom_model_label()),
                        );
                        ui.end_row();

                        ui.label(i18n::t(language, "API key"));
                        style::secret_edit(
                            ui,
                            &mut self.embed_api_key,
                            &mut self.embed_key_revealed,
                            i18n::t(language, "(leave blank for local servers)"),
                        );
                        ui.end_row();
                    });

                ui.add_space(12.0);
                style::section_heading(ui, i18n::t(language, "Contact email (optional)"));
                style::muted_label(
                    ui,
                    i18n::t(
                        language,
                        "Sent to scholarly APIs (OpenAlex, Crossref, Unpaywall). Leave blank to skip Unpaywall.",
                    ),
                );
                egui::Grid::new("first-run-contact-grid")
                    .num_columns(2)
                    .spacing([8.0, 6.0])
                    .show(ui, |ui| {
                        ui.label(i18n::t(language, "Email"));
                        ui.add(
                            egui::TextEdit::singleline(&mut self.contact_email)
                                .hint_text("you@example.com"),
                        );
                        ui.end_row();
                    });

                ui.add_space(8.0);
                if let Some(err) = self.error.clone() {
                    if style::error_notice(ui, &err, None) == style::NoticeAction::Dismiss {
                        self.error = None;
                    }
                    ui.add_space(4.0);
                }

                ui.horizontal(|ui| {
                    if style::primary_button(ui, i18n::t(language, "Next")).clicked() {
                        match self.validate() {
                            Ok((llm, embedding, contact_email)) => {
                                let embedding_dimensions = embedding
                                    .effective_provider()
                                    .known_dimensions(&embedding.model);
                                outcome = FirstRunOutcome::Submitted {
                                    llm,
                                    embedding,
                                    embedding_dimensions,
                                    contact_email,
                                }
                            }
                            Err(msg) => self.error = Some(msg),
                        }
                    }
                });
            });

        outcome
    }

    fn validate(&self) -> Result<(LlmConfig, EmbeddingConfig, Option<String>), String> {
        let llm_base_url = self.llm_base_url.trim().trim_end_matches('/').to_string();
        let llm_model = self.llm_model.trim().to_string();
        let llm_api_key = normalize_api_key(&self.llm_api_key);

        if llm_base_url.is_empty() {
            return Err("LLM Base URL is required.".to_string());
        }
        if !(llm_base_url.starts_with("http://") || llm_base_url.starts_with("https://")) {
            return Err("LLM Base URL must start with http:// or https://".to_string());
        }
        if llm_model.is_empty() {
            return Err("LLM Model name is required.".to_string());
        }

        let embed_base_url = self.embed_base_url.trim().trim_end_matches('/').to_string();
        let embed_model = self.embed_model.trim().to_string();
        let embed_api_key = normalize_api_key(&self.embed_api_key);

        if embed_base_url.is_empty() {
            return Err("Embedding Base URL is required.".to_string());
        }
        if !(embed_base_url.starts_with("http://") || embed_base_url.starts_with("https://")) {
            return Err("Embedding Base URL must start with http:// or https://".to_string());
        }
        if embed_model.is_empty() {
            return Err("Embedding Model name is required.".to_string());
        }

        let contact_email = {
            let trimmed = self.contact_email.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        };

        Ok((
            LlmConfig {
                provider: Some(self.llm_provider),
                base_url: llm_base_url,
                model: llm_model,
                api_key: llm_api_key,
                ..LlmConfig::default()
            },
            EmbeddingConfig {
                provider: Some(self.embed_provider),
                base_url: embed_base_url,
                model: embed_model,
                api_key: embed_api_key,
            },
            contact_email,
        ))
    }
}

fn model_combo(
    ui: &mut egui::Ui,
    id: &'static str,
    presets: &[&'static str],
    model: &mut String,
    custom_label: &'static str,
) {
    let current_is_preset = presets.iter().any(|preset| *preset == model.trim());
    let mut selected = if current_is_preset {
        model.trim().to_string()
    } else {
        custom_label.to_string()
    };
    let selected_text = if current_is_preset {
        model.trim().to_string()
    } else if model.trim().is_empty() {
        custom_label.to_string()
    } else {
        model.trim().to_string()
    };

    egui::ComboBox::from_id_salt(id)
        .selected_text(selected_text)
        .show_ui(ui, |ui| {
            for preset in presets {
                ui.selectable_value(&mut selected, (*preset).to_string(), *preset);
            }
            ui.selectable_value(&mut selected, custom_label.to_string(), custom_label);
        });

    if selected != custom_label && selected != model.trim() {
        *model = selected.clone();
    }

    if selected == custom_label {
        ui.add(egui::TextEdit::singleline(model).hint_text(custom_label));
    }
}

fn apply_llm_provider_defaults(
    old: LlmProvider,
    new: LlmProvider,
    base_url: &mut String,
    model: &mut String,
) {
    if base_url.trim().is_empty() || infer_llm_provider(base_url) == Some(old) {
        *base_url = new.default_base_url().to_string();
    }
    if should_replace_model(old.model_presets(), model) {
        *model = new.default_model().to_string();
    }
}

fn apply_embedding_provider_defaults(
    old: EmbeddingProvider,
    new: EmbeddingProvider,
    base_url: &mut String,
    model: &mut String,
) {
    if base_url.trim().is_empty() || infer_embedding_provider(base_url) == Some(old) {
        *base_url = new.default_base_url().to_string();
    }
    if should_replace_model(old.model_presets(), model) {
        *model = new.default_model().to_string();
    }
}

fn should_replace_model(old_presets: &[&'static str], model: &str) -> bool {
    let trimmed = model.trim();
    trimmed.is_empty() || old_presets.iter().any(|preset| *preset == trimmed)
}

/// Step 2 of the setup wizard: describe the research in plain language. Writes
/// into the active workspace; the same fields are editable later in Input Set.
#[derive(Default)]
pub struct ResearchSetupForm {
    pub name: String,
    pub primary_question: String,
    pub topics_text: String,
    pub error: Option<String>,
    prefilled: bool,
}

pub enum ResearchSetupOutcome {
    Pending,
    Submitted {
        name: String,
        primary_question: String,
        topics: Vec<String>,
    },
    Skipped,
}

impl ResearchSetupForm {
    pub fn is_prefilled(&self) -> bool {
        self.prefilled
    }

    /// Loads the seeded defaults once so the user edits rather than starts blank.
    pub fn prefill(&mut self, name: &str, primary_question: &str, topics: &[String]) {
        if self.prefilled {
            return;
        }
        self.name = name.to_string();
        self.primary_question = primary_question.to_string();
        self.topics_text = topics.join("\n");
        self.prefilled = true;
    }

    pub fn show(&mut self, ctx: &egui::Context, language: UiLanguage) -> ResearchSetupOutcome {
        let mut outcome = ResearchSetupOutcome::Pending;

        egui::Window::new(i18n::t(language, "Set up your research"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                style::muted_label(ui, i18n::t(language, "Step 2 of 2 · Your research"));
                style::body_label(
                    ui,
                    i18n::t(
                        language,
                        "Tell ResearchWiki what to gather and study. You can refine this anytime in Input Set.",
                    ),
                );
                ui.add_space(10.0);

                egui::Grid::new("research-setup-grid")
                    .num_columns(2)
                    .spacing([10.0, 8.0])
                    .show(ui, |ui| {
                        ui.label(i18n::t(language, "Research name"));
                        ui.text_edit_singleline(&mut self.name);
                        ui.end_row();

                        ui.label(i18n::t(language, "What question are you trying to answer?"));
                        ui.add(
                            egui::TextEdit::multiline(&mut self.primary_question)
                                .desired_rows(2)
                                .desired_width(360.0),
                        );
                        ui.end_row();

                        ui.label(i18n::t(language, "Key topics & search terms\n(one per line)"));
                        ui.add(
                            egui::TextEdit::multiline(&mut self.topics_text)
                                .desired_rows(6)
                                .desired_width(360.0),
                        );
                        ui.end_row();
                    });

                ui.add_space(8.0);
                if let Some(err) = self.error.clone() {
                    if style::error_notice(ui, &err, None) == style::NoticeAction::Dismiss {
                        self.error = None;
                    }
                    ui.add_space(4.0);
                }

                ui.horizontal(|ui| {
                    if style::primary_button(ui, i18n::t(language, "Finish setup")).clicked() {
                        match self.validate() {
                            Ok((name, primary_question, topics)) => {
                                outcome = ResearchSetupOutcome::Submitted {
                                    name,
                                    primary_question,
                                    topics,
                                }
                            }
                            Err(msg) => self.error = Some(msg),
                        }
                    }
                    if style::secondary_button(ui, i18n::t(language, "Skip for now")).clicked() {
                        outcome = ResearchSetupOutcome::Skipped;
                    }
                });
            });

        outcome
    }

    fn validate(&self) -> Result<(String, String, Vec<String>), String> {
        let name = self.name.trim().to_string();
        if name.is_empty() {
            return Err("Please give your research a name.".to_string());
        }
        let primary_question = self.primary_question.trim().to_string();
        let topics = self
            .topics_text
            .lines()
            .map(|line| line.trim().to_string())
            .filter(|line| !line.is_empty())
            .collect::<Vec<_>>();
        Ok((name, primary_question, topics))
    }
}
