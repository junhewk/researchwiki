use crate::state::AppState;

pub fn show(ui: &mut egui::Ui, _state: &AppState) {
    ui.heading("Prompts");
    ui.separator();
    ui.label("Prompt list + editor + test + version history — TODO");
}
