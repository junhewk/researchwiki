use crate::config::{EmbeddingConfig, LlmConfig};

pub struct FirstRunForm {
    pub llm_base_url: String,
    pub llm_model: String,
    pub llm_api_key: String,
    pub embed_base_url: String,
    pub embed_model: String,
    pub embed_api_key: String,
    pub error: Option<String>,
}

impl Default for FirstRunForm {
    fn default() -> Self {
        let embed_defaults = EmbeddingConfig::default();
        Self {
            llm_base_url: String::new(),
            llm_model: String::new(),
            llm_api_key: String::new(),
            embed_base_url: embed_defaults.base_url,
            embed_model: embed_defaults.model,
            embed_api_key: String::new(),
            error: None,
        }
    }
}

pub enum FirstRunOutcome {
    Pending,
    Submitted {
        llm: LlmConfig,
        embedding: EmbeddingConfig,
    },
}

impl FirstRunForm {
    pub fn show(&mut self, ctx: &egui::Context) -> FirstRunOutcome {
        let mut outcome = FirstRunOutcome::Pending;

        egui::Window::new("Welcome to ResearchWiki")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.label("Configure the two OpenAI-compatible endpoints ResearchWiki uses.");
                ui.label("You can change either later in Settings.");
                ui.add_space(10.0);

                ui.heading("LLM endpoint");
                ui.label("Used for evaluation, screening, knowledge-graph extraction, etc.");
                egui::Grid::new("first-run-llm-grid")
                    .num_columns(2)
                    .spacing([8.0, 6.0])
                    .show(ui, |ui| {
                        ui.label("Base URL");
                        ui.text_edit_singleline(&mut self.llm_base_url);
                        ui.end_row();

                        ui.label("Model");
                        ui.text_edit_singleline(&mut self.llm_model);
                        ui.end_row();

                        ui.label("API key");
                        ui.add(egui::TextEdit::singleline(&mut self.llm_api_key).password(true));
                        ui.end_row();
                    });

                ui.add_space(12.0);
                ui.heading("Embedding endpoint");
                ui.label("Used to embed article chunks for semantic + hybrid search.");
                egui::Grid::new("first-run-embed-grid")
                    .num_columns(2)
                    .spacing([8.0, 6.0])
                    .show(ui, |ui| {
                        ui.label("Base URL");
                        ui.text_edit_singleline(&mut self.embed_base_url);
                        ui.end_row();

                        ui.label("Model");
                        ui.text_edit_singleline(&mut self.embed_model);
                        ui.end_row();

                        ui.label("API key");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.embed_api_key)
                                .password(true)
                                .hint_text("(leave blank for local servers)"),
                        );
                        ui.end_row();
                    });

                ui.add_space(8.0);
                if let Some(err) = &self.error {
                    ui.colored_label(egui::Color32::RED, err);
                }

                ui.horizontal(|ui| {
                    if ui.button("Save and continue").clicked() {
                        match self.validate() {
                            Ok((llm, embedding)) => {
                                outcome = FirstRunOutcome::Submitted { llm, embedding }
                            }
                            Err(msg) => self.error = Some(msg),
                        }
                    }
                });
            });

        outcome
    }

    fn validate(&self) -> Result<(LlmConfig, EmbeddingConfig), String> {
        let llm_base_url = self.llm_base_url.trim().trim_end_matches('/').to_string();
        let llm_model = self.llm_model.trim().to_string();
        let llm_api_key = self.llm_api_key.trim().to_string();

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
        let embed_api_key = self.embed_api_key.trim().to_string();

        if embed_base_url.is_empty() {
            return Err("Embedding Base URL is required.".to_string());
        }
        if !(embed_base_url.starts_with("http://") || embed_base_url.starts_with("https://")) {
            return Err("Embedding Base URL must start with http:// or https://".to_string());
        }
        if embed_model.is_empty() {
            return Err("Embedding Model name is required.".to_string());
        }

        Ok((
            LlmConfig {
                base_url: llm_base_url,
                model: llm_model,
                api_key: llm_api_key,
                ..LlmConfig::default()
            },
            EmbeddingConfig {
                base_url: embed_base_url,
                model: embed_model,
                api_key: embed_api_key,
            },
        ))
    }
}
