use crate::state::AppState;

pub fn show(ui: &mut egui::Ui, _state: &AppState) {
    ui.heading("Gather");
    ui.separator();
    ui.label("Trigger buttons per source + live job progress + run history — TODO");
}
