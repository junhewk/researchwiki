use super::PanelCtx;

#[derive(Default)]
pub struct Panel;

impl Panel {
    pub fn show(&mut self, ui: &mut egui::Ui, _ctx: &PanelCtx<'_>) {
        ui.heading("Prompts");
        ui.separator();
        ui.label("Prompt list + editor + test + version history — TODO");
    }
}
