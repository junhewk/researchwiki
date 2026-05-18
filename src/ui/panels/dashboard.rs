use crate::state::AppState;

pub fn show(ui: &mut egui::Ui, _state: &AppState) {
    ui.heading("Dashboard");
    ui.separator();
    ui.label("Article counts, daily stats chart, tier summary tiles — TODO");
}
