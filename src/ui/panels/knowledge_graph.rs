use crate::state::AppState;

pub fn show(ui: &mut egui::Ui, _state: &AppState) {
    ui.heading("Knowledge Graph");
    ui.separator();
    ui.label("Entity search + neighborhood viz (egui_graphs, deferred to Phase 4) — TODO");
}
