use crate::state::AppState;

pub fn show(ui: &mut egui::Ui, _state: &AppState) {
    ui.heading("Articles");
    ui.separator();
    ui.label("Virtualized TableBuilder with filters and detail panel — TODO");
}
