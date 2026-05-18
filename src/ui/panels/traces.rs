use crate::state::AppState;

pub fn show(ui: &mut egui::Ui, _state: &AppState) {
    ui.heading("Traces");
    ui.separator();
    ui.label("Paginated LLM trace table with detail view — TODO");
}
