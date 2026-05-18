use crate::state::AppState;

pub fn show(ui: &mut egui::Ui, state: &AppState) {
    ui.heading("Settings");
    ui.separator();
    ui.label("LLM endpoint, paths, scheduler toggles — TODO");
    ui.add_space(8.0);
    ui.group(|ui| {
        ui.label(format!("Database: {}", state.config.storage.database_path.display()));
        ui.label(format!("Prompts:  {}", state.config.storage.prompts_dir.display()));
        ui.label(format!("Wiki dir: {}", state.config.storage.wiki_export_dir.display()));
        ui.label(format!("Settings: {}", state.config.storage.settings_file.display()));
        ui.label(format!("LLM URL:  {}", if state.config.llm.base_url.is_empty() { "(unset)" } else { state.config.llm.base_url.as_str() }));
    });
}
