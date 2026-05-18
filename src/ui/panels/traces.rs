use super::PanelCtx;

#[derive(Default)]
pub struct Panel;

impl Panel {
    pub fn show(&mut self, ui: &mut egui::Ui, _ctx: &PanelCtx<'_>) {
        ui.heading("Traces");
        ui.separator();
        ui.label("Paginated LLM trace table with detail view — TODO");
    }
}
