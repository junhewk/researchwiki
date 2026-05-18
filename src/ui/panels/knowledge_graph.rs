use super::PanelCtx;

#[derive(Default)]
pub struct Panel;

impl Panel {
    pub fn show(&mut self, ui: &mut egui::Ui, _ctx: &PanelCtx<'_>) {
        ui.heading("Knowledge Graph");
        ui.separator();
        ui.label("Entity search + neighborhood viz (egui_graphs, deferred to Phase 4) — TODO");
    }
}
