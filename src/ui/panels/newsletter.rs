use crate::state::AppState;

pub fn show(ui: &mut egui::Ui, _state: &AppState) {
    ui.heading("Newsletter");
    ui.separator();
    ui.label("Drag-drop article selector + LLM generation + preview — TODO");
}
