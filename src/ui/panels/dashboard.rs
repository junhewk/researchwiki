use super::PanelCtx;

#[derive(Default)]
pub struct Panel;

impl Panel {
    pub fn show(&mut self, ui: &mut egui::Ui, _ctx: &PanelCtx<'_>) {
        ui.heading("Dashboard");
        ui.separator();
        ui.label("Article counts, daily stats chart, tier summary tiles — TODO");
    }
}
