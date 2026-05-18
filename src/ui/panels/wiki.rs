use crate::state::AppState;

pub fn show(ui: &mut egui::Ui, _state: &AppState) {
    ui.heading("Wiki");
    ui.separator();
    ui.label("Entity list + synthesis markdown rendering (egui_commonmark) — TODO");
}
