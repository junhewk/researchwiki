use super::PanelCtx;

#[derive(Default)]
pub struct Panel;

impl Panel {
    pub fn show(&mut self, ui: &mut egui::Ui, _ctx: &PanelCtx<'_>) {
        ui.heading("Wiki");
        ui.separator();
        ui.label("Entity list + synthesis markdown rendering (egui_commonmark) — TODO");
    }
}
