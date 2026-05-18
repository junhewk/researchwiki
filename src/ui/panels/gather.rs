use super::PanelCtx;

#[derive(Default)]
pub struct Panel;

impl Panel {
    pub fn show(&mut self, ui: &mut egui::Ui, _ctx: &PanelCtx<'_>) {
        ui.heading("Gather");
        ui.separator();
        ui.label("Trigger buttons per source + live job progress + run history — TODO");
    }
}
