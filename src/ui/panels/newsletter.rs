use super::PanelCtx;

#[derive(Default)]
pub struct Panel;

impl Panel {
    pub fn show(&mut self, ui: &mut egui::Ui, _ctx: &PanelCtx<'_>) {
        ui.heading("Newsletter");
        ui.separator();
        ui.label("Drag-drop article selector + LLM generation + preview — TODO");
    }
}
