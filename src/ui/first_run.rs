use crate::config::LlmConfig;

/// State of the first-run / LLM-endpoint setup modal.
///
/// Stored on the `DesktopApp` and shown whenever `LlmConfig::is_configured`
/// returns false. The user must complete it before any other panel becomes
/// interactive, because every LLM-dependent feature will silently fail
/// otherwise.
#[derive(Default)]
pub struct FirstRunForm {
    pub base_url: String,
    pub model: String,
    pub api_key: String,
    pub error: Option<String>,
}

pub enum FirstRunOutcome {
    Pending,
    Submitted(LlmConfig),
}

impl FirstRunForm {
    pub fn show(&mut self, ctx: &egui::Context) -> FirstRunOutcome {
        let mut outcome = FirstRunOutcome::Pending;

        egui::Window::new("Welcome to ResearchWiki")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.label("Point ResearchWiki at an OpenAI-compatible LLM endpoint.");
                ui.label("You can change these later in Settings.");
                ui.add_space(8.0);

                egui::Grid::new("first-run-grid")
                    .num_columns(2)
                    .spacing([8.0, 6.0])
                    .show(ui, |ui| {
                        ui.label("Base URL");
                        ui.text_edit_singleline(&mut self.base_url);
                        ui.end_row();

                        ui.label("Model");
                        ui.text_edit_singleline(&mut self.model);
                        ui.end_row();

                        ui.label("API key");
                        ui.add(egui::TextEdit::singleline(&mut self.api_key).password(true));
                        ui.end_row();
                    });

                ui.add_space(8.0);
                if let Some(err) = &self.error {
                    ui.colored_label(egui::Color32::RED, err);
                }

                ui.horizontal(|ui| {
                    if ui.button("Save and continue").clicked() {
                        match self.validate() {
                            Ok(cfg) => outcome = FirstRunOutcome::Submitted(cfg),
                            Err(msg) => self.error = Some(msg),
                        }
                    }
                });
            });

        outcome
    }

    fn validate(&self) -> Result<LlmConfig, String> {
        let base_url = self.base_url.trim().trim_end_matches('/').to_string();
        let model = self.model.trim().to_string();
        let api_key = self.api_key.trim().to_string();

        if base_url.is_empty() {
            return Err("Base URL is required.".to_string());
        }
        if !(base_url.starts_with("http://") || base_url.starts_with("https://")) {
            return Err("Base URL must start with http:// or https://".to_string());
        }
        if model.is_empty() {
            return Err("Model name is required.".to_string());
        }

        Ok(LlmConfig {
            base_url,
            model,
            api_key,
            ..LlmConfig::default()
        })
    }
}
